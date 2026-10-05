"""实时监控：轮询系统进程表，捕捉「用户打开了某个 exe」。

判定策略（两段式，避免把后台服务/更新器误记成"你打开的软件"）：

- 新出现的进程先进入 pending 名单；
- 满足任一条件即确认为一次"打开"：
  a) 它拥有可见且带标题的窗口（用户看得见）；
  b) 它的父进程是 explorer.exe（用户从桌面/开始菜单/资源管理器启动）；
- PENDING_TTL 秒内都没满足（后台服务、计划任务）→ 丢弃。

同一 exe 一次启动常派生多个进程（Chromium 系会冒出十几个），
故落库时按 dedupe 窗口去重，只算一次打开。
"""
from __future__ import annotations

import logging
import os
import threading
import time

import winutil

log = logging.getLogger("exetrace.watcher")

PENDING_TTL = 120.0
DEDUPE_WINDOW = 300.0


class ProcessWatcher(threading.Thread):
    def __init__(self, store, interval: float = 2.0, on_record=None):
        super().__init__(name="ExeTrace-Watcher", daemon=True)
        self.store = store
        self.interval = interval
        self.on_record = on_record          # 回调在 watcher 线程执行，调用方负责转主线程
        self._stop_evt = threading.Event()
        self._paused = threading.Event()
        self._last_fg: int | None = None
        self._fg_recent: dict[int, float] = {}   # 最近成为前台的 pid → 时间戳（hook 线程写）
        self._fg_pid: int | None = None          # 当前时长归属的前台 pid
        self._fg_since: float = 0.0              # 该前台的开始时刻
        self._time_accum: dict[int, float] = {}  # pid → 未落库秒数（轮询线程 flush）
        self._last_time_flush: float = 0.0
        self._pid_exe: dict[int, str] = {}       # pid → exe 路径（快照时维护，落库换算用）
        self._path_cache: dict[int, str] = {}    # pid → exe 路径缓存（空串=查过但拿不到）
        self._hook: winutil.ForegroundEventHook | None = None
        self.hook_available = False
        self._known: set[int] = set()
        self._pending: dict[int, dict] = {}
        self._primed = False
        self.recorded_count = 0
        self.last_error = ""

    # ------------------------------------------------------------ 生命周期

    def stop(self) -> None:
        self._stop_evt.set()
        if self._hook is not None:
            self._hook.stop()

    def pause(self) -> None:
        """暂停记录：轮询照常吸收进程表，但不产生任何新记录。"""
        self._paused.set()

    def resume(self) -> None:
        self._paused.clear()

    @property
    def paused(self) -> bool:
        return self._paused.is_set()

    def run(self) -> None:
        # 事件驱动（实时捕获前台切换；hook 不可用时自动退化为纯轮询）
        self._hook = winutil.ForegroundEventHook(self._on_foreground_event)
        self.hook_available = self._hook.start()
        if self.hook_available:
            log.info("前台事件 hook 已启用（事件驱动 + %.1fs 轮询兜底）", self.interval)
        else:
            log.warning("前台事件 hook 不可用（%s），退化纯轮询", self._hook.last_error)
        while not self._stop_evt.is_set():
            try:
                self._tick()
            except Exception as exc:
                self.last_error = f"{type(exc).__name__}: {exc}"
                log.warning("监控轮询异常: %s", self.last_error)
            self._stop_evt.wait(self.interval)
        if self._hook is not None:
            self._hook.stop()

    def _on_foreground_event(self, pid: int) -> None:
        """hook 线程回调：只登记「最近前台」+ 结算上一段时长，绝不阻塞。"""
        now = time.time()
        self._fg_recent[pid] = now
        self._switch_foreground(pid, now)
        if len(self._fg_recent) > 64:
            deadline = now - PENDING_TTL
            for k in [k for k, t in self._fg_recent.items() if t < deadline]:
                self._fg_recent.pop(k, None)

    def _switch_foreground(self, pid: int, now: float) -> None:
        """前台切到 pid：上一段停留时长入内存累加器（落库由轮询线程做）。"""
        if self._fg_pid is not None and self._fg_pid != pid and self._fg_since:
            delta = now - self._fg_since
            if 0 < delta < 86400:  # 防御异常时间差（休眠/时钟跳变）
                self._time_accum[self._fg_pid] = self._time_accum.get(self._fg_pid, 0.0) + delta
        self._fg_pid = pid
        self._fg_since = now

    def _flush_usage(self, now: float) -> None:
        """把未落库时长写入数据库（含正在进行的当前段）。"""
        accum = dict(self._time_accum)
        if self._fg_pid is not None and self._fg_since:
            delta = now - self._fg_since
            if 0 < delta < 86400:
                accum[self._fg_pid] = accum.get(self._fg_pid, 0.0) + delta
                self._fg_since = now  # 已计入，重置起点
        self._time_accum.clear()
        self._last_time_flush = now
        if not accum:
            return
        by_path: dict[str, float] = {}
        for pid, secs in accum.items():
            exe = self._pid_exe.get(pid)
            if exe:
                by_path[exe] = by_path.get(exe, 0.0) + secs
        if by_path:
            try:
                self.store.add_usage(by_path)
            except Exception as exc:  # noqa: BLE001
                self.last_error = f"写入时长失败: {exc}"
                log.warning("写入使用时长失败: %s", exc)

    # ------------------------------------------------------------ 单轮

    def _snapshot(self) -> dict[int, tuple[str | None, int | None]]:
        """系统快照 {pid: (exe 全路径, 父 pid)}。

        热路径用 ToolHelp（一次调用枚举全部进程，不 OpenProcess）；exe 全路径
        带缓存、只对「新 pid」查询一次 —— 这是把每 2 秒轮询的 CPU 从 ~5.5%
        压到 1% 以下的关键（旧版 psutil.process_iter 逐进程打开句柄是主要开销）。
        """
        raw = winutil.snapshot_processes()
        procs: dict[int, tuple[str | None, int | None]] = {}
        for pid, (name, ppid) in raw.items():
            path = self._path_cache.get(pid)
            if path is None or (path and os.path.basename(path).lower() != name):
                # 新 pid；或 pid 被复用（exe 名对不上）→ 重新查询全路径
                path = winutil.process_exe_path(pid) or ""  # 空串：查过但拿不到（高权限进程）
                self._path_cache[pid] = path
            procs[pid] = (path or None, ppid)
        # pid → exe 映射（时长落库用）；缓存超限时收缩为当前快照，防 pid 历史无限增长
        self._pid_exe.update({p: e for p, (e, _) in procs.items() if e})
        if len(self._path_cache) > 4096:
            self._path_cache = {p: v for p, v in self._path_cache.items() if p in raw}
        if len(self._pid_exe) > 4096:
            self._pid_exe = {p: e for p, e in self._pid_exe.items() if p in raw}
        return procs

    def _tick(self) -> None:
        t0 = time.perf_counter()
        procs = self._snapshot()
        now = time.time()

        if not self._primed:
            # 首个快照只建立基线：此刻已在运行的进程并不是"刚刚被打开"的。
            # 它们的历史由冷启动扫描负责，否则打开 ExeTrace 的一瞬间会给几十个
            # 常驻软件（微信/浏览器/输入法）全部 +1 次打开，记录立刻失真。
            self._known = set(procs)
            self._primed = True
            return

        if self._paused.is_set():
            # 暂停期间照常吸收进程表：恢复后不会把"暂停时已在运行"的进程误当新进程；
            # 同时结算并清除时长归属，暂停期不计入任何应用
            self._flush_usage(now)
            self._fg_pid = None
            self._fg_since = 0.0
            self._known = set(procs)
            self._pending.clear()
            return

        # 1) 新进程登记为待定
        for pid in set(procs) - self._known:
            exe, ppid = procs[pid]
            if winutil.is_noise_exe(exe):
                continue
            parent_exe = (procs.get(ppid) or (None, None))[0]
            from_explorer = bool(parent_exe) and os.path.basename(parent_exe).lower() == "explorer.exe"
            self._pending[pid] = {"exe": exe, "deadline": now + PENDING_TTL, "explorer": from_explorer}
            log.info("候选进程: pid=%s exe=%s explorer=%s", pid, exe, from_explorer)

        # 2) 待定名单复核（含本轮新增）
        # 判据（2026-10-05 收紧）：「拥有前台窗口」或「由 explorer 启动」才算打开。
        # 原「任意可见窗口」判据会把输入法 / 崩溃报告器这类后台弹窗组件也记成
        # "用户打开的软件"（实测 SGBizLauncher / crashrpt 噪音）。
        fg_pid = winutil.foreground_pid()
        if fg_pid is not None:
            self._fg_recent[fg_pid] = now      # 兜底：hook 失效/漏事件时现场补记
        if fg_pid is not None and fg_pid != self._last_fg and self._pending:
            log.info("前台切换: fg=%s，待复核=%s", fg_pid, sorted(self._pending))
        self._last_fg = fg_pid
        # 判定（事件驱动）：候选进程只要在 PENDING_TTL 内成为过前台即算「被打开」。
        # 相比旧版「本轮 fg_pid 恰等于」，事件表能捞住「瞬间到前台又切走」的窗口。
        deadline = now - PENDING_TTL
        for k in [k for k, t in self._fg_recent.items() if t < deadline]:
            self._fg_recent.pop(k, None)
        for pid, item in list(self._pending.items()):
            if pid in self._fg_recent or item["explorer"]:
                self._record(item["exe"])
                self._pending.pop(pid, None)
            elif pid not in procs or now > item["deadline"]:
                self._pending.pop(pid, None)  # 一直没到前台的进程，静默丢弃

        self._known = set(procs)

        # 使用时长落库（节流：≥30s 一次）
        if now - self._last_time_flush >= 30.0:
            self._flush_usage(now)

        cost = time.perf_counter() - t0
        if cost > 5.0:
            log.warning("监控轮询耗时 %.1fs（窗口枚举可能被阻塞）", cost)

    def _record(self, exe: str) -> None:
        try:
            counted = self.store.record_launch(exe, source="watch", dedupe_window=DEDUPE_WINDOW)
            self.recorded_count += 1
            log.info("记录打开: %s%s", exe, "" if counted else " (窗口内去重)")
            if self.on_record:
                self.on_record(exe, counted)
        except Exception as exc:
            self.last_error = f"{type(exc).__name__}: {exc}"
            log.warning("写入历史库失败: %s", self.last_error)
