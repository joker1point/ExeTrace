"""ExeTrace 入口。

用法：
  python src/main.py                正常启动
  python src/main.py --minimized    启动后直接缩到托盘（开机自启用）
  python src/main.py --selftest     非 GUI 自检（打包前后冒烟验证）
  python src/main.py --no-tray      禁用托盘（调试用）
"""
from __future__ import annotations

import argparse
import logging
import os
import sys
import threading
import time
import tkinter as tk

import paths
import scanner
import shortcut
import ui
import winutil
from store import Store
from watcher import ProcessWatcher

log = logging.getLogger("exetrace")


def setup_logging() -> None:
    handlers: list[logging.Handler] = [logging.FileHandler(paths.log_path(), encoding="utf-8")]
    if sys.stderr is not None and sys.stderr.isatty():
        handlers.append(logging.StreamHandler(sys.stderr))
    logging.basicConfig(
        level=logging.INFO,
        format="%(asctime)s %(levelname)s %(name)s: %(message)s",
        handlers=handlers,
    )


def _sample_exes() -> list[str]:
    """挑几个真实存在的 exe 用于自检。"""
    out: list[str] = []
    for root in (os.environ.get("ProgramFiles", ""), os.environ.get("ProgramFiles(x86)", ""),
                 os.environ.get("LOCALAPPDATA", ""), os.environ.get("WINDIR", "")):
        if not root or not os.path.isdir(root):
            continue
        count = 0
        for dirpath, _dirnames, filenames in os.walk(root):
            for fn in filenames:
                if fn.lower().endswith(".exe"):
                    out.append(os.path.join(dirpath, fn))
                    count += 1
                    if count >= 3:
                        break
            if count >= 3 or len(out) >= 6:
                break
        if len(out) >= 6:
            break
    return out


def selftest() -> int:
    """非 GUI 自检：路径模型 / 存储 / 扫描 / 图标提取。结果同时写入 selftest.txt。

    --windowed 打包产物没有控制台，必须靠文件回读结果（交付形态验收）。
    """
    import tempfile

    lines: list[str] = []
    ok = True

    def report(name: str, passed: bool, detail: str = "") -> None:
        nonlocal ok
        ok = ok and passed
        flag = "PASS" if passed else "FAIL"
        line = f"[{flag}] {name} {detail}".rstrip()
        lines.append(line)
        print(line)

    report("frozen", True, f"frozen={paths.is_frozen()}")
    report("data_dir", paths.data_dir().is_dir(), str(paths.data_dir()))

    samples = _sample_exes()
    report("sample_exes", bool(samples), f"{len(samples)} 个真实 exe")

    with tempfile.TemporaryDirectory() as td:
        st = Store(os.path.join(td, "t.db"))
        try:
            for i, p in enumerate(samples[:3]):
                # 种子时间放在去重窗口（300s）之外，才能验证 record_launch 的计数分支
                st.upsert_seed(p, launch_count=i + 1, last_seen=time.time() - 3600 - i * 60)
            report("store_seed", st.total() == len(samples[:3]), f"total={st.total()}")

            if samples:
                counted = st.record_launch(samples[0])
                report("store_record", counted is True, f"first={counted}")
                counted2 = st.record_launch(samples[0])  # 去重窗口内
                report("store_dedupe", counted2 is False, f"second={counted2}")
                rows = st.query("", "count")
                report("store_query", bool(rows), f"rows={len(rows)} first={rows[0].name if rows else '-'}")
                smart = st.query("", "smart")
                report("store_frecency", len(smart) == len(rows), f"smart={len(smart)} rows={len(rows)}")
                st.add_usage({samples[0]: 90.0})
                usage = next(
                    (r.total_seconds for r in st.query("", "recent") if r.path == samples[0]), 0.0
                )
                report("store_usage", usage >= 90.0, f"total_seconds={usage}")
                st.remove(samples[0])
                report("store_remove", st.total() == len(samples[:3]) - 1, f"total={st.total()}")
        finally:
            st.close()

    if samples:
        png = winutil.extract_icon_png(samples[0], ui.ICON_SIZE)
        report("icon_extract", bool(png), f"{len(png) if png else 0} bytes from {os.path.basename(samples[0])}")
    report("noise_filter", winutil.is_noise_exe(r"C:\Windows\System32\svchost.exe") is True, "系统进程被过滤")
    report("noise_filter_user", winutil.is_noise_exe(samples[0]) is False if samples else True, "用户应用不被误杀")
    cat = (
        winutil.categorize(r"C:\tools\Code.exe"),
        winutil.categorize(r"C:\Program Files\Google\Chrome\Application\chrome.exe"),
        winutil.categorize(r"C:\tools\mystery-tool.exe"),
    )
    report("categorize", cat == ("开发", "浏览器", "其他"), "/".join(cat))
    snap = winutil.snapshot_processes()
    report("proc_snapshot", len(snap) > 50 and os.getpid() in snap, f"{len(snap)} 个进程")
    me = winutil.process_exe_path(os.getpid())
    report("proc_exe_path", bool(me) and me.lower().endswith(".exe"), f"{os.path.basename(me) if me else 'None'}")
    report("trim_working_set", isinstance(winutil.trim_working_set(), bool), "调用成功（后台内存压缩）")
    report("window_enum", isinstance(winutil.visible_window_pids(), set),
           f"{len(winutil.visible_window_pids())} 个带窗口进程")
    fg = winutil.foreground_pid()
    report("foreground_pid", fg is None or isinstance(fg, int), f"fg_pid={fg}")
    report("user_ignores", isinstance(winutil._user_ignores(), frozenset),
           f"{len(winutil._user_ignores())} 条用户忽略")
    hook = winutil.ForegroundEventHook(lambda _pid: None)
    report("fg_hook", hook.start(), f"available={hook.available} err={hook.last_error or '-'}")
    hook.stop()
    hk = winutil.GlobalHotkey(lambda: None)
    ok_start = hk.start()
    report("global_hotkey", isinstance(ok_start, bool),
           f"available={hk.available} err={hk.last_error or '-'}")
    hk.stop()

    try:
        st2 = Store(os.path.join(tempfile.gettempdir(), "exetrace_selftest_scan.db"))
        stats = scanner.scan_all(st2)
        st2.close()
        report("registry_scan", True, f"收录 {stats['total']} 个应用")
    except Exception as exc:
        report("registry_scan", False, str(exc))

    # v3：桌面快捷方式（在临时目录验证，不打扰用户桌面）
    report("com_init", shortcut.com_initialize(), "COM 初始化")
    report("desktop_dir", os.path.isdir(shortcut.desktop_dir()), shortcut.desktop_dir())
    with tempfile.TemporaryDirectory() as td:
        if samples:
            lnk = os.path.join(td, "ExeTrace selftest.lnk")
            try:
                shortcut.create_shortcut(
                    lnk, samples[0], workdir=os.path.dirname(samples[0]), icon=samples[0]
                )
                report("shortcut_create", os.path.isfile(lnk), f"{os.path.getsize(lnk)} bytes")
                info = shortcut.read_shortcut(lnk)
                same = os.path.normcase(info.get("target", "")) == os.path.normcase(samples[0])
                report("shortcut_readback", same, f"target={info.get('target')}")
            except OSError as exc:
                report("shortcut_create", False, repr(exc))
        # 同名自动编号（绝不覆盖已有文件）
        first = shortcut.unique_lnk_path(td, "same")
        with open(first, "w", encoding="utf-8"):
            pass
        second = shortcut.unique_lnk_path(td, "same")
        report("shortcut_unique", second != first, os.path.basename(second))

    lines.append("SELFTEST " + ("OK" if ok else "FAILED"))
    try:
        (paths.data_dir() / "selftest.txt").write_text("\n".join(lines), encoding="utf-8")
    except OSError:
        pass
    print(lines[-1])
    return 0 if ok else 1


def _ensure_streams() -> None:
    """windowed 打包产物没有控制台时 stdout/stderr 可能是 None，print 会崩。"""
    if sys.stdout is None:
        sys.stdout = open(os.devnull, "w", encoding="utf-8")
    if sys.stderr is None:
        sys.stderr = open(os.devnull, "w", encoding="utf-8")


def create_from_cli(raw_path: str, name: str | None = None) -> int:
    """命令行直接创建桌面快捷方式（脚本 / 批量场景）。退出码：0 成功且已校验。"""
    setup_logging()
    if not shortcut.com_initialize():
        print("error: COM init failed")
        return 2
    target = os.path.abspath(raw_path)
    if not os.path.isfile(target):
        print(f"error: 目标不存在或不是文件: {target}")
        return 3
    try:
        desktop = shortcut.desktop_dir()
        os.makedirs(desktop, exist_ok=True)
        base = shortcut.sanitize_name(name or "") or shortcut.default_shortcut_name(target)
        lnk_path = shortcut.unique_lnk_path(desktop, base)
        shortcut.create_shortcut(
            lnk_path,
            target,
            workdir=os.path.dirname(target),
            icon=target,
            description=os.path.basename(target),
        )
    except OSError as exc:
        print(f"error: {exc}")
        return 4

    verified = False
    try:
        info = shortcut.read_shortcut(lnk_path)
        verified = os.path.normcase(info.get("target", "")) == os.path.normcase(target)
    except Exception as exc:  # noqa: BLE001
        log.warning("读回校验失败: %s", exc)
    print(f"created: {lnk_path}")
    log.info("CLI 创建快捷方式: %s -> %s (verified=%s)", lnk_path, target, verified)
    return 0 if verified else 5


def main(argv: list[str] | None = None) -> int:
    parser = argparse.ArgumentParser(prog="ExeTrace", description="应用历史与桌面快捷方式")
    parser.add_argument("--minimized", action="store_true", help="启动后直接缩到托盘")
    parser.add_argument("--no-tray", action="store_true", help="禁用系统托盘")
    parser.add_argument("--selftest", action="store_true", help="运行自检并退出")
    parser.add_argument(
        "--make-shortcut", metavar="PATH", help="直接创建桌面快捷方式（脚本/批量用；成功退出码 0）"
    )
    parser.add_argument("--name", help="快捷方式名称（默认取文件名）")
    args = parser.parse_args(argv)

    _ensure_streams()
    if args.selftest:
        return selftest()
    if args.make_shortcut:
        return create_from_cli(args.make_shortcut, args.name)

    setup_logging()
    log.info("启动 ExeTrace %s (frozen=%s, pid=%s)", paths.APP_VERSION, paths.is_frozen(), os.getpid())

    mutex = winutil.acquire_single_instance()
    if mutex is None:
        import ctypes

        ctypes.windll.user32.MessageBoxW(
            None, "ExeTrace 已经在运行了。\n请在系统托盘里查看它。", "ExeTrace", 0x40
        )
        return 0

    t_boot = time.perf_counter()
    store = Store()
    t_store = time.perf_counter()
    shortcut.com_initialize()  # 主线程 COM（创建快捷方式用），幂等
    ui.enable_dpi_awareness()  # 必须在 Tk() 之前调用，否则系统忽略
    root = tk.Tk()
    t_tk = time.perf_counter()
    ui.apply_scaling(root)
    app = ui.AppWindow(root, store, start_hidden=args.minimized, tray_enabled=not args.no_tray)
    log.info(
        "启动耗时: Store %.2fs, Tk 创建 %.2fs, UI 合计 %.2fs",
        t_store - t_boot, t_tk - t_store, time.perf_counter() - t_boot,
    )

    def _scan_worker() -> None:
        try:
            stats = scanner.scan_all(store)
        except Exception as exc:  # noqa: BLE001
            log.exception("系统历史扫描失败")
            stats = {"total": 0, "error": str(exc)}
        app.post(lambda: app.on_scan_done(stats))

    threading.Thread(target=_scan_worker, name="ExeTrace-Scan", daemon=True).start()

    watcher = ProcessWatcher(store, on_record=app.on_watcher_record)
    watcher.start()
    app.watcher = watcher

    try:
        root.mainloop()
    finally:
        watcher.stop()
        store.close()
        log.info("已退出")
    return 0


if __name__ == "__main__":
    sys.exit(main())
