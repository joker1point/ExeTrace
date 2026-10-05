"""Windows 平台工具：可见窗口枚举、exe 图标提取、开机自启、噪音进程判定。

全部基于 ctypes 标准库实现，不引入 pywin32 依赖（打包体积更小）。
"""
from __future__ import annotations

import ctypes
import io
import os
import subprocess
import sys
import threading
import time
from ctypes import wintypes
from pathlib import Path

import paths

IS_WINDOWS = sys.platform == "win32"

if IS_WINDOWS:
    user32 = ctypes.WinDLL("user32", use_last_error=True)
    gdi32 = ctypes.WinDLL("gdi32", use_last_error=True)
    shell32 = ctypes.WinDLL("shell32", use_last_error=True)
    kernel32 = ctypes.WinDLL("kernel32", use_last_error=True)
else:  # pragma: no cover - 仅用于非 Windows 下的导入安全
    user32 = gdi32 = shell32 = kernel32 = None


# ---------------------------------------------------------------- 噪音过滤

_WINDOWS_DIR = os.environ.get("WINDIR") or r"C:\Windows"

# 路径前缀：这些目录下的进程是系统/后台设施，不是"用户打开的软件"
_PATH_PREFIX_BLOCK = tuple(
    os.path.normcase(p)
    for p in (
        _WINDOWS_DIR,
        os.path.join(_WINDOWS_DIR, "System32"),
        os.path.join(_WINDOWS_DIR, "SysWOW64"),
        os.path.join(_WINDOWS_DIR, "WinSxS"),
        os.path.join(_WINDOWS_DIR, "Installer"),
    )
    if p
)

# 文件名黑名单：即使不在系统目录，也属于"后台设施"而非用户主动打开的应用
_NAME_BLOCK = {
    "conhost.exe", "dllhost.exe", "backgroundtaskhost.exe", "runtimebroker.exe",
    "werfault.exe", "taskhostw.exe", "msiexec.exe", "wmiprvse.exe", "smartscreen.exe",
    "securityhealthsystray.exe", "securityhealthservice.exe", "searchindexer.exe",
    "searchapp.exe", "searchhost.exe", "sihost.exe", "ctfmon.exe", "textinputhost.exe",
    "startmenuexperiencehost.exe", "shellexperiencehost.exe", "applicationframehost.exe",
    "explorer.exe", "usoclient.exe", "sppsvc.exe", "tim.exe", "ngcisoinstaller.exe",
    "csrss.exe", "lsass.exe", "services.exe", "winlogon.exe", "wininit.exe",
    "registry.exe", "fontdrvhost.exe", "spoolsv.exe", "audiodg.exe", "dwm.exe",
    "msedgewebview2.exe", "crashpad_handler.exe", "elevation_service.exe",
}

_user_ignore_cache: tuple[float, frozenset[str]] = (0.0, frozenset())


def _user_ignores() -> frozenset[str]:
    """用户自定义忽略表（config.json 的 ignore 字段），按文件 mtime 热重载。

    条目语义：含路径分隔符 → 小写路径前缀；否则 → exe 文件名（精确匹配）。
    """
    global _user_ignore_cache
    try:
        mtime = paths.config_path().stat().st_mtime
    except OSError:
        _user_ignore_cache = (0.0, frozenset())
        return frozenset()
    stamp, items = _user_ignore_cache
    if stamp == mtime:
        return items
    raw = paths.load_config().get("ignore") or []
    items = frozenset(str(x).strip().lower() for x in raw if str(x).strip())
    _user_ignore_cache = (mtime, items)
    return items


# ---------------------------------------------------------------- 进程快照
#
# ToolHelp 一次调用枚举全部进程 (pid, 文件名, 父 pid)，无需 OpenProcess ——
# 这是监控轮询的热路径（Rust 版 winx.rs 同款做法）。exe 全路径只在
# 「新出现的 pid」上按需查询，其余走调用方缓存。

_TH32CS_SNAPPROCESS = 0x00000002
_PROCESS_QUERY_LIMITED_INFORMATION = 0x1000
_MAX_PATH = 32768
_INVALID_HANDLE = ctypes.c_void_p(-1).value


class _PROCESSENTRY32W(ctypes.Structure):
    _fields_ = [
        ("dwSize", wintypes.DWORD),
        ("cntUsage", wintypes.DWORD),
        ("th32ProcessID", wintypes.DWORD),
        ("th32DefaultHeapID", ctypes.c_size_t),
        ("th32ModuleID", wintypes.DWORD),
        ("cntThreads", wintypes.DWORD),
        ("th32ParentProcessID", wintypes.DWORD),
        ("pcPriClassBase", ctypes.c_long),
        ("dwFlags", wintypes.DWORD),
        ("szExeFile", wintypes.WCHAR * 260),
    ]


if IS_WINDOWS:
    kernel32.CreateToolhelp32Snapshot.argtypes = [wintypes.DWORD, wintypes.DWORD]
    kernel32.CreateToolhelp32Snapshot.restype = wintypes.HANDLE
    kernel32.Process32FirstW.argtypes = [wintypes.HANDLE, ctypes.POINTER(_PROCESSENTRY32W)]
    kernel32.Process32FirstW.restype = wintypes.BOOL
    kernel32.Process32NextW.argtypes = [wintypes.HANDLE, ctypes.POINTER(_PROCESSENTRY32W)]
    kernel32.Process32NextW.restype = wintypes.BOOL
    kernel32.OpenProcess.argtypes = [wintypes.DWORD, wintypes.BOOL, wintypes.DWORD]
    kernel32.OpenProcess.restype = wintypes.HANDLE
    kernel32.QueryFullProcessImageNameW.argtypes = [
        wintypes.HANDLE, wintypes.DWORD, wintypes.LPWSTR, ctypes.POINTER(wintypes.DWORD),
    ]
    kernel32.QueryFullProcessImageNameW.restype = wintypes.BOOL
    kernel32.CloseHandle.argtypes = [wintypes.HANDLE]
    kernel32.CloseHandle.restype = wintypes.BOOL


def snapshot_processes() -> dict[int, tuple[str, int]]:
    """系统进程快照：{pid: (exe 文件名小写, 父 pid)}；一次 ToolHelp 枚举。"""
    out: dict[int, tuple[str, int]] = {}
    if not IS_WINDOWS:
        return out
    snap = kernel32.CreateToolhelp32Snapshot(_TH32CS_SNAPPROCESS, 0)
    if not snap or snap == _INVALID_HANDLE:
        return out
    try:
        entry = _PROCESSENTRY32W()
        entry.dwSize = ctypes.sizeof(_PROCESSENTRY32W)
        if not kernel32.Process32FirstW(snap, ctypes.byref(entry)):
            return out
        while True:
            out[int(entry.th32ProcessID)] = (
                entry.szExeFile.lower(), int(entry.th32ParentProcessID),
            )
            if not kernel32.Process32NextW(snap, ctypes.byref(entry)):
                break
    except Exception:
        pass
    finally:
        kernel32.CloseHandle(snap)
    return out


def process_exe_path(pid: int) -> str | None:
    """进程 exe 全路径（一次 OpenProcess）；拿不到时返回 None。"""
    if not IS_WINDOWS:
        return None
    handle = kernel32.OpenProcess(_PROCESS_QUERY_LIMITED_INFORMATION, False, pid)
    if not handle:
        return None
    try:
        buf = ctypes.create_unicode_buffer(_MAX_PATH)
        size = wintypes.DWORD(_MAX_PATH)
        if kernel32.QueryFullProcessImageNameW(handle, 0, buf, ctypes.byref(size)):
            return buf.value or None
        return None
    except Exception:
        return None
    finally:
        kernel32.CloseHandle(handle)


# ---------------------------------------------------------------- 应用分类
#
# 启发式分类（对标 DigitalWellbeing 的 App Tagging）：按 exe 名/路径关键词
# 匹配；用户可在 config.json 的 categories 字段覆盖（{exe 名: 分类}）。
# 分类是派生数据、不落库 —— 规则或配置变更后下一次刷新即生效。

_CATEGORY_RULES: tuple[tuple[str, tuple[str, ...]], ...] = (
    ("开发", (
        "code.exe", "code-insiders", "cursor", "windsurf", "trae", "pycharm", "idea64",
        "clion", "goland", "webstorm", "rider64", "devenv.exe", "cmd.exe", "powershell",
        "pwsh", "wt.exe", "windowsterminal", "mintty", "git-bash", "bash.exe", "wsl",
        "docker", "postman", "dbeaver", "navicat", "sublime_text", "notepad++", "vim",
        "emacs", "xshell", "xftp", "putty", "winscp", "hBuilder",
    )),
    ("浏览器", (
        "chrome", "msedge", "firefox", "brave", "opera", "vivaldi", "iexplore", "360se",
        "360chrome", "qqbrowser", "sogouexplorer", "tabbit", "arc.exe", "zen.exe",
    )),
    ("通讯", (
        "wechat", "weixin", "qq.exe", "tim.exe", "dingtalk", "feishu", "lark", "slack",
        "discord", "telegram", "whatsapp", "zoom", "teams", "skype", "mstsc", "anydesk",
        "todesk", "sunlogin",
    )),
    ("媒体", (
        "potplayer", "vlc", "mpv", "kmplayer", "spotify", "cloudmusic", "netease", "foobar",
        "aimp", "audacity", "obs64", "obs.exe", "premiere", "photoshop", "gimp", "inkscape",
        "blender", "ffmpeg", "honeyview", "imageglass", "xnview", "bandicam",
    )),
    ("办公", (
        "winword", "excel", "powerpnt", "outlook", "wps", "et.exe", "wpp", "onenote",
        "notion", "obsidian", "typora", "foxit", "acrobat", "sumatrapdf", "xmind",
        "mindmaster", "evernote", "youdaonote", "wiznote",
    )),
    ("游戏", (
        "steam", "epicgames", "battle.net", "battlenet", "ubisoft", "gog", "wegame",
        "unity", "unreal", "minecraft", "genshin", "yuanshen", "riot",
    )),
    ("系统工具", (
        "explorer.exe", "taskmgr", "control.exe", "regedit", "cleanmgr", "diskmgmt",
        "devmgmt", "services.msc", "mspaint", "calc.exe", "notepad.exe", "snippingtool",
        "magnify", "perfmon", "resmon", "eventvwr", "mmc.exe",
    )),
)

_user_category_cache: tuple[float, dict[str, str]] = (0.0, {})


def _user_categories() -> dict[str, str]:
    """config.json 的 categories 覆盖表 {exe 名（小写）: 分类}，按 mtime 热重载。"""
    global _user_category_cache
    try:
        mtime = paths.config_path().stat().st_mtime
    except OSError:
        _user_category_cache = (0.0, {})
        return {}
    stamp, table = _user_category_cache
    if stamp == mtime:
        return table
    raw = paths.load_config().get("categories")
    table = {}
    if isinstance(raw, dict):
        for k, v in raw.items():
            key, val = str(k).strip().lower(), str(v).strip()
            if key and val:
                table[key] = val
    _user_category_cache = (mtime, table)
    return table


def categorize(exe_path: str | None) -> str:
    """应用的启发式分类（用户覆盖优先）；派生数据，不落库。"""
    if not exe_path:
        return "其他"
    base = os.path.basename(exe_path).lower()
    user = _user_categories()
    if base in user:
        return user[base]
    hay = exe_path.lower()
    for label, keys in _CATEGORY_RULES:
        for k in keys:
            if k in hay:
                return label
    return "其他"


def is_noise_exe(exe_path: str | None) -> bool:
    """判定某个 exe 路径是否属于噪音（系统设施 / 自己）。"""
    if not exe_path:
        return True
    p = os.path.normcase(os.path.abspath(exe_path))
    if not p.endswith(".exe"):
        return True
    if os.path.basename(p) in _NAME_BLOCK:
        return True
    for prefix in _PATH_PREFIX_BLOCK:
        if prefix and p.startswith(prefix + os.sep):
            return True
    # UWP（WindowsApps）目录权限受限，定位无意义
    if "\\windowsapps\\" in p:
        return True
    # 临时目录 / 解包目录里的 exe（安装器临时物）不算用户软件
    if "\\appdata\\local\\temp\\" in p or "\\$recycle.bin\\" in p:
        return True
    # 自身进程
    try:
        if os.path.normcase(paths.runtime_exe()) == p:
            return True
    except Exception:
        pass
    # 用户忽略列表（config.json 的 ignore 字段，按 mtime 热重载）
    for item in _user_ignores():
        if "\\" in item or "/" in item:
            if p.startswith(item.replace("/", "\\")):
                return True
        elif os.path.basename(p) == item:
            return True
    return False


# ---------------------------------------------------------------- 可见窗口

_GW_OWNER = 4
_WM_GETTEXTLENGTH = 0x000E
_SMTO_BLOCK = 0x0001
_SMTO_ABORTIFHUNG = 0x0002
_TITLE_TIMEOUT_MS = 200


def _safe_title_length(hwnd) -> int:
    """带超时读取窗口标题长度。

    GetWindowTextLengthW 是同步 SendMessage 语义：目标窗口所属线程不泵消息
    （主线程正忙 / 窗口挂起）时，调用线程会被阻塞到它恢复为止。Rust 版迁移时
    踩过同款坑（监控轮询被窗口枚举拖死）。此处用 SendMessageTimeoutW 兜底，
    超时即视为无标题 —— 个别窗口读不到标题，远好过监控线程停摆。
    """
    result = ctypes.c_size_t(0)
    ok = user32.SendMessageTimeoutW(
        hwnd, _WM_GETTEXTLENGTH, 0, 0,
        _SMTO_BLOCK | _SMTO_ABORTIFHUNG, _TITLE_TIMEOUT_MS, ctypes.byref(result),
    )
    return int(result.value) if ok else 0


def visible_window_pids(exclude_pid: int | None = None) -> set[int]:
    """返回当前拥有"可见且带标题的顶层窗口"的进程 PID 集合。

    这是"用户真的打开了这个软件"最贴近的判据：后台服务、更新器
    通常没有可见窗口，而用户点击打开的应用会有。

    exclude_pid：跳过该进程自身的窗口（监控轮询传 os.getpid()）。本进程窗口
    的标题读取会同步等待主线程，主线程忙时把轮询线程阻塞住；且"用户打开的
    软件"本来也不该算 ExeTrace 自己。
    """
    if not IS_WINDOWS:
        return set()
    pids: set[int] = set()

    @ctypes.WINFUNCTYPE(wintypes.BOOL, wintypes.HWND, wintypes.LPARAM)
    def _cb(hwnd, _lparam):  # noqa: ANN001
        try:
            if not user32.IsWindowVisible(hwnd):
                return True
            pid = wintypes.DWORD(0)
            user32.GetWindowThreadProcessId(hwnd, ctypes.byref(pid))
            if not pid.value:
                return True
            if exclude_pid is not None and int(pid.value) == exclude_pid:
                return True
            if _safe_title_length(hwnd) <= 0:
                return True  # 隐藏工具窗口 / IME 窗口没有标题
            pids.add(int(pid.value))
        except Exception:
            pass
        return True

    try:
        user32.EnumWindows(_cb, 0)
    except Exception:
        pass
    return pids


def foreground_pid() -> int | None:
    """当前前台窗口所属进程的 PID；没有前台窗口时返回 None。"""
    if not IS_WINDOWS:
        return None
    try:
        hwnd = user32.GetForegroundWindow()
        if not hwnd:
            return None
        pid = wintypes.DWORD(0)
        user32.GetWindowThreadProcessId(hwnd, ctypes.byref(pid))
        return int(pid.value) or None
    except Exception:
        return None


# ---------------------------------------------------------------- 前台事件
#
# 事件驱动（2026-10-05 实测：45s 内 23 个前台事件零遗漏、CPU 消耗 0.00s）：
# SetWinEventHook 与 GetMessage 消息泵必须在同一线程（回调经该线程消息队列
# 投递），因此封装成独立线程；回调内只做「取 PID + 交给消费者」，绝不阻塞
# （写库/日志都留给消费者线程）。同 PID 短时间内的重复事件做去重。

_EVENT_SYSTEM_FOREGROUND = 0x0003
_WINEVENT_OUTOFCONTEXT = 0x0000
_WM_QUIT = 0x0012

_WinEventProc = ctypes.WINFUNCTYPE(
    None, wintypes.HANDLE, wintypes.DWORD, wintypes.HWND,
    wintypes.LONG, wintypes.LONG, wintypes.DWORD, wintypes.DWORD,
)


class ForegroundEventHook:
    """独立线程监听前台窗口切换；on_foreground(pid) 在该线程回调，必须极快。"""

    def __init__(self, on_foreground, dedupe_seconds: float = 0.5):
        self._on_foreground = on_foreground
        self._dedupe = max(0.0, dedupe_seconds)
        self._thread: threading.Thread | None = None
        self._thread_id = 0
        self._hook = None
        self._proc = None            # 保持引用防 GC
        self._ready = threading.Event()
        self.available = False
        self.event_count = 0
        self.last_error = ""
        self._last_pid = 0
        self._last_at = 0.0

    def start(self) -> bool:
        if not IS_WINDOWS:
            return False
        self._thread = threading.Thread(target=self._run, name="ExeTrace-FgHook", daemon=True)
        self._thread.start()
        self._ready.wait(timeout=3.0)
        return self.available

    def stop(self) -> None:
        if self._thread_id:
            try:
                user32.PostThreadMessageW(self._thread_id, _WM_QUIT, 0, 0)
            except Exception:
                pass

    def _run(self) -> None:
        self._thread_id = int(kernel32.GetCurrentThreadId())
        try:
            self._proc = _WinEventProc(self._on_event)
            self._hook = user32.SetWinEventHook(
                _EVENT_SYSTEM_FOREGROUND, _EVENT_SYSTEM_FOREGROUND,
                None, self._proc, 0, 0, _WINEVENT_OUTOFCONTEXT,
            )
        except Exception as exc:  # noqa: BLE001
            self.last_error = f"{type(exc).__name__}: {exc}"
        if not self._hook:
            if not self.last_error:
                self.last_error = f"SetWinEventHook 失败 (gle={ctypes.get_last_error()})"
            self._ready.set()
            return
        self.available = True
        self._ready.set()
        msg = wintypes.MSG()
        while user32.GetMessageW(ctypes.byref(msg), None, 0, 0) > 0:
            user32.TranslateMessage(ctypes.byref(msg))
            user32.DispatchMessageW(ctypes.byref(msg))
        try:
            user32.UnhookWinEvent(self._hook)
        except Exception:
            pass

    def _on_event(self, _hook, _event, hwnd, _obj, _child, _thread, _ts) -> None:
        if not hwnd:
            return
        try:
            pid = wintypes.DWORD(0)
            user32.GetWindowThreadProcessId(hwnd, ctypes.byref(pid))
            value = int(pid.value)
        except Exception:
            return
        if not value:
            return
        now = time.time()
        if value == self._last_pid and (now - self._last_at) < self._dedupe:
            return
        self._last_pid, self._last_at = value, now
        self.event_count += 1
        try:
            self._on_foreground(value)
        except Exception as exc:  # noqa: BLE001
            self.last_error = f"回调异常: {type(exc).__name__}: {exc}"


# ---------------------------------------------------------------- 全局热键
#
# 独立线程：CreateWindowExW（STATIC 隐藏窗口）+ RegisterHotKey + GetMessage
# 消息泵（WM_HOTKEY 投递给该窗口）。回调在热键线程触发，必须极快。

_MOD_ALT = 0x0001
_MOD_CONTROL = 0x0002
_MOD_SHIFT = 0x0004
_MOD_WIN = 0x0008
_MOD_NOREPEAT = 0x4000
_WM_HOTKEY = 0x0312
_WM_CLOSE = 0x0010
_VK_OEM_3 = 0xC0  # ` 键


class GlobalHotkey:
    """注册一个全局热键；on_hotkey() 在热键线程回调，必须极快。

    默认 Ctrl+Alt+E（Win+` 在本机实测被占用，gle=1409）。
    """

    def __init__(self, on_hotkey, modifiers: int = _MOD_CONTROL | _MOD_ALT, vk: int = ord("E")):
        self._on_hotkey = on_hotkey
        self._modifiers = modifiers | _MOD_NOREPEAT
        self._vk = vk
        self._thread: threading.Thread | None = None
        self._thread_id = 0
        self._hwnd = None
        self._wndproc = None  # 保持引用防 GC
        self._ready = threading.Event()
        self.available = False
        self.last_error = ""
        self.hotkey_count = 0

    def start(self) -> bool:
        if not IS_WINDOWS:
            return False
        self._thread = threading.Thread(target=self._run, name="ExeTrace-Hotkey", daemon=True)
        self._thread.start()
        self._ready.wait(timeout=3.0)
        return self.available

    def stop(self) -> None:
        if self._hwnd:
            try:
                user32.PostMessageW(self._hwnd, _WM_CLOSE, 0, 0)
            except Exception:
                pass

    def _run(self) -> None:
        self._thread_id = int(kernel32.GetCurrentThreadId())
        try:
            wndproc_type = ctypes.WINFUNCTYPE(
                ctypes.c_ssize_t, wintypes.HWND, wintypes.UINT, ctypes.c_size_t, ctypes.c_ssize_t
            )

            @wndproc_type
            def _wndproc(hwnd, msg, wparam, lparam):  # noqa: ANN001
                if msg == _WM_HOTKEY:
                    self.hotkey_count += 1
                    try:
                        self._on_hotkey()
                    except Exception as exc:  # noqa: BLE001
                        self.last_error = f"热键回调异常: {type(exc).__name__}: {exc}"
                    return 0
                if msg == _WM_CLOSE:
                    user32.DestroyWindow(hwnd)
                    return 0
                return user32.DefWindowProcW(hwnd, msg, wparam, lparam)

            self._wndproc = _wndproc
            self._hwnd = _create_message_window(_wndproc)
            if not self._hwnd:
                self.last_error = f"CreateWindowExW 失败 (gle={ctypes.get_last_error()})"
                self._ready.set()
                return
            if not user32.RegisterHotKey(self._hwnd, 1, self._modifiers, self._vk):
                self.last_error = f"RegisterHotKey 失败 (gle={ctypes.get_last_error()})"
                user32.DestroyWindow(self._hwnd)
                self._hwnd = None
                self._ready.set()
                return
        except Exception as exc:  # noqa: BLE001
            self.last_error = f"{type(exc).__name__}: {exc}"
            self._ready.set()
            return

        self.available = True
        self._ready.set()
        msg = wintypes.MSG()
        while user32.GetMessageW(ctypes.byref(msg), None, 0, 0) > 0:
            user32.TranslateMessage(ctypes.byref(msg))
            user32.DispatchMessageW(ctypes.byref(msg))
        try:
            user32.UnregisterHotKey(self._hwnd, 1)
            user32.DestroyWindow(self._hwnd)
        except Exception:
            pass


def _create_message_window(wndproc):
    """创建一个不可见窗口用于接收消息。"""
    hinstance = kernel32.GetModuleHandleW(None)
    return user32.CreateWindowExW(
        0, "STATIC", "ExeTraceHotkey", 0,
        0, 0, 0, 0, None, None, hinstance, None,
    )


# ---------------------------------------------------------------- 平台函数签名
#
# 未声明 restype 时 ctypes 按 c_int 截断返回值；64 位上的句柄必须显式声明。

user32.DrawIconEx.argtypes = [
    wintypes.HDC, ctypes.c_int, ctypes.c_int, wintypes.HICON,
    ctypes.c_int, ctypes.c_int, wintypes.UINT, wintypes.HANDLE, wintypes.UINT,
]
user32.DrawIconEx.restype = wintypes.BOOL
user32.DestroyIcon.argtypes = [wintypes.HICON]
user32.IsWindowVisible.argtypes = [wintypes.HWND]
user32.IsWindowVisible.restype = wintypes.BOOL
user32.GetWindowTextLengthW.argtypes = [wintypes.HWND]
user32.GetWindowTextLengthW.restype = ctypes.c_int
user32.GetWindowThreadProcessId.argtypes = [wintypes.HWND, ctypes.POINTER(wintypes.DWORD)]
user32.GetWindowThreadProcessId.restype = wintypes.DWORD
user32.EnumWindows.restype = wintypes.BOOL
user32.GetForegroundWindow.argtypes = []
user32.GetForegroundWindow.restype = wintypes.HWND
user32.SetWinEventHook.argtypes = [
    wintypes.DWORD, wintypes.DWORD, wintypes.HMODULE, _WinEventProc,
    wintypes.DWORD, wintypes.DWORD, wintypes.DWORD,
]
user32.SetWinEventHook.restype = wintypes.HANDLE
user32.UnhookWinEvent.argtypes = [wintypes.HANDLE]
user32.UnhookWinEvent.restype = wintypes.BOOL
user32.GetMessageW.argtypes = [ctypes.POINTER(wintypes.MSG), wintypes.HWND, wintypes.UINT, wintypes.UINT]
user32.GetMessageW.restype = ctypes.c_int
user32.TranslateMessage.argtypes = [ctypes.POINTER(wintypes.MSG)]
user32.DispatchMessageW.argtypes = [ctypes.POINTER(wintypes.MSG)]
kernel32.GetCurrentThreadId.restype = wintypes.DWORD
user32.PostThreadMessageW.argtypes = [
    wintypes.DWORD, wintypes.UINT, ctypes.c_size_t, ctypes.c_ssize_t,
]
user32.PostThreadMessageW.restype = wintypes.BOOL
user32.CreateWindowExW.argtypes = [
    wintypes.DWORD, wintypes.LPCWSTR, wintypes.LPCWSTR, wintypes.DWORD,
    ctypes.c_int, ctypes.c_int, ctypes.c_int, ctypes.c_int,
    wintypes.HWND, wintypes.HMENU, wintypes.HINSTANCE, ctypes.c_void_p,
]
user32.CreateWindowExW.restype = wintypes.HWND
user32.RegisterHotKey.argtypes = [wintypes.HWND, ctypes.c_int, wintypes.UINT, wintypes.UINT]
user32.RegisterHotKey.restype = wintypes.BOOL
user32.UnregisterHotKey.argtypes = [wintypes.HWND, ctypes.c_int]
user32.UnregisterHotKey.restype = wintypes.BOOL
user32.DestroyWindow.argtypes = [wintypes.HWND]
user32.DestroyWindow.restype = wintypes.BOOL
user32.DefWindowProcW.argtypes = [
    wintypes.HWND, wintypes.UINT, ctypes.c_size_t, ctypes.c_ssize_t,
]
user32.DefWindowProcW.restype = ctypes.c_ssize_t
user32.PostMessageW.argtypes = [
    wintypes.HWND, wintypes.UINT, ctypes.c_size_t, ctypes.c_ssize_t,
]
user32.PostMessageW.restype = wintypes.BOOL
kernel32.GetModuleHandleW.argtypes = [wintypes.LPCWSTR]
kernel32.GetModuleHandleW.restype = wintypes.HMODULE
user32.SendMessageTimeoutW.argtypes = [
    wintypes.HWND, wintypes.UINT, ctypes.c_size_t, ctypes.c_ssize_t,
    wintypes.UINT, wintypes.UINT, ctypes.POINTER(ctypes.c_size_t),
]
user32.SendMessageTimeoutW.restype = ctypes.c_size_t

gdi32.CreateDIBSection.argtypes = [
    wintypes.HDC, ctypes.c_void_p, wintypes.UINT, ctypes.POINTER(ctypes.c_void_p), wintypes.HANDLE, wintypes.DWORD
]
gdi32.CreateDIBSection.restype = wintypes.HANDLE
gdi32.CreateCompatibleDC.argtypes = [wintypes.HDC]
gdi32.CreateCompatibleDC.restype = wintypes.HDC
gdi32.SelectObject.argtypes = [wintypes.HDC, wintypes.HANDLE]
gdi32.SelectObject.restype = wintypes.HANDLE
gdi32.DeleteObject.argtypes = [wintypes.HANDLE]
gdi32.DeleteDC.argtypes = [wintypes.HDC]

shell32.SHGetFileInfoW.argtypes = [
    wintypes.LPCWSTR, wintypes.DWORD, ctypes.c_void_p, wintypes.UINT, wintypes.UINT
]
shell32.SHGetFileInfoW.restype = ctypes.c_void_p

kernel32.CreateMutexW.argtypes = [ctypes.c_void_p, wintypes.BOOL, wintypes.LPCWSTR]
kernel32.CreateMutexW.restype = wintypes.HANDLE


# ---------------------------------------------------------------- 图标提取

class _SHFILEINFOW(ctypes.Structure):
    _fields_ = [
        ("hIcon", wintypes.HICON),
        ("iIcon", ctypes.c_int),
        ("dwAttributes", wintypes.DWORD),
        ("szDisplayName", wintypes.WCHAR * 260),
        ("szTypeName", wintypes.WCHAR * 80),
    ]


class _BITMAPINFOHEADER(ctypes.Structure):
    _fields_ = [
        ("biSize", wintypes.DWORD),
        ("biWidth", wintypes.LONG),
        ("biHeight", wintypes.LONG),
        ("biPlanes", wintypes.WORD),
        ("biBitCount", wintypes.WORD),
        ("biCompression", wintypes.DWORD),
        ("biSizeImage", wintypes.DWORD),
        ("biXPelsPerMeter", wintypes.LONG),
        ("biYPelsPerMeter", wintypes.LONG),
        ("biClrUsed", wintypes.DWORD),
        ("biClrImportant", wintypes.DWORD),
    ]


class _BITMAPINFO(ctypes.Structure):
    _fields_ = [("bmiHeader", _BITMAPINFOHEADER), ("bmiColors", wintypes.DWORD * 3)]


_SHGFI_ICON = 0x000000100
_SHGFI_LARGEICON = 0x000000000
_DIB_RGB_COLORS = 0
_DI_NORMAL = 0x0003


def _hicon_to_png(hicon: int, size: int) -> bytes | None:
    """把 HICON 画到 32bpp DIB 上（由 GDI 完成缩放/掩码/透明合成），输出 PNG 字节。"""
    from PIL import Image  # 延迟导入，避免拖慢非图标路径

    bmi = _BITMAPINFO()
    bmi.bmiHeader.biSize = ctypes.sizeof(_BITMAPINFOHEADER)
    bmi.bmiHeader.biWidth = size
    bmi.bmiHeader.biHeight = -size  # 负数 = top-down
    bmi.bmiHeader.biPlanes = 1
    bmi.bmiHeader.biBitCount = 32
    bmi.bmiHeader.biCompression = 0  # BI_RGB

    bits = ctypes.c_void_p()
    hbitmap = gdi32.CreateDIBSection(None, ctypes.byref(bmi), _DIB_RGB_COLORS, ctypes.byref(bits), None, 0)
    if not hbitmap:
        return None
    hdc = gdi32.CreateCompatibleDC(None)
    old = gdi32.SelectObject(hdc, hbitmap)
    try:
        ctypes.memset(bits, 0, size * size * 4)
        ok = user32.DrawIconEx(hdc, 0, 0, hicon, size, size, 0, None, _DI_NORMAL)
        if not ok:
            return None
        raw = ctypes.string_at(bits, size * size * 4)
    finally:
        gdi32.SelectObject(hdc, old)
        gdi32.DeleteObject(hbitmap)
        gdi32.DeleteDC(hdc)

    img = Image.frombuffer("RGBA", (size, size), raw, "raw", "BGRA", 0, 1).copy()

    # GDI 合成出的通道是预乘 alpha 的，还原为直通 alpha，避免半透明边缘发暗
    alpha = img.getchannel("A")
    if img.getextrema()[3][1] > 0:
        px = img.load()
        for y in range(size):
            for x in range(size):
                r, g, b, a = px[x, y]
                if 0 < a < 255:
                    px[x, y] = (min(255, r * 255 // a), min(255, g * 255 // a), min(255, b * 255 // a), a)
    else:
        # 极老的 16 色图标：alpha 全 0，用亮度当透明度兜底
        px = img.load()
        for y in range(size):
            for x in range(size):
                r, g, b, _a = px[x, y]
                a = 255 if (r + g + b) > 24 else 0
                px[x, y] = (r, g, b, a)
    del alpha

    out = io.BytesIO()
    img.save(out, "PNG")
    return out.getvalue()


_icon_png_cache: dict[tuple[str, int], bytes | None] = {}


def extract_icon_png(exe_path: str, size: int = 24) -> bytes | None:
    """提取 exe 内嵌图标，返回 PNG 字节；失败返回 None（调用方降级为无图标）。"""
    if not IS_WINDOWS:
        return None
    key = (os.path.normcase(exe_path), size)
    if key in _icon_png_cache:
        return _icon_png_cache[key]

    png: bytes | None = None
    shfi = _SHFILEINFOW()
    try:
        ret = shell32.SHGetFileInfoW(
            ctypes.c_wchar_p(exe_path), 0, ctypes.byref(shfi), ctypes.sizeof(shfi), _SHGFI_ICON | _SHGFI_LARGEICON
        )
        if ret and shfi.hIcon:
            png = _hicon_to_png(shfi.hIcon, size)
    except Exception:
        png = None
    finally:
        if shfi.hIcon:
            try:
                user32.DestroyIcon(shfi.hIcon)
            except Exception:
                pass

    _icon_png_cache[key] = png
    return png


# ---------------------------------------------------------------- 开机自启

_RUN_KEY = r"Software\Microsoft\Windows\CurrentVersion\Run"
_RUN_VALUE = paths.APP_NAME


def is_autostart_enabled() -> bool:
    if not IS_WINDOWS:
        return False
    import winreg

    try:
        with winreg.OpenKey(winreg.HKEY_CURRENT_USER, _RUN_KEY) as k:
            winreg.QueryValueEx(k, _RUN_VALUE)
            return True
    except OSError:
        return False


def set_autostart(enabled: bool) -> bool:
    """写入/删除 HKCU Run 键。返回操作后实际状态。"""
    if not IS_WINDOWS:
        return False
    import winreg

    try:
        with winreg.CreateKey(winreg.HKEY_CURRENT_USER, _RUN_KEY) as k:
            if enabled:
                winreg.SetValueEx(k, _RUN_VALUE, 0, winreg.REG_SZ, paths.autostart_command())
            else:
                try:
                    winreg.DeleteValue(k, _RUN_VALUE)
                except FileNotFoundError:
                    pass
    except OSError:
        pass
    return is_autostart_enabled()


# ---------------------------------------------------------------- Shell 交互


def reveal_in_explorer(path: str) -> None:
    """打开资源管理器并选中该文件。explorer 的退出码不可作为成功判据。"""
    folder = os.path.dirname(path)
    if os.path.exists(path):
        subprocess.Popen(["explorer", f"/select,{os.path.normpath(path)}"])
    elif os.path.isdir(folder):
        subprocess.Popen(["explorer", os.path.normpath(folder)])


def launch_path(path: str) -> None:
    """以系统默认方式启动（等同双击）。"""
    os.startfile(path)  # noqa: S606 - 这是产品核心功能


# ---------------------------------------------------------------- 单实例

# 环境变量可覆盖互斥体名：诊断/多实例对比测试用，正常使用不受影响
_MUTEX_NAME = os.environ.get("EXETRACE_MUTEX_NAME") or "Local\\ExeTrace_SingleInstance_9F3C"


def acquire_single_instance() -> int | None:
    """已存在实例则返回 None；否则返回需要保持存活的句柄。"""
    if not IS_WINDOWS:
        return 0
    handle = kernel32.CreateMutexW(None, False, _MUTEX_NAME)
    if not handle:
        return 0
    ERROR_ALREADY_EXISTS = 183
    if ctypes.get_last_error() == ERROR_ALREADY_EXISTS:
        return None
    return handle


def make_app_icon_image(size: int = 64):
    """程序化绘制应用图标（窗口 / 托盘 / exe 图标三处共用的唯一绘制源）。

    设计：蓝色渐变圆角方块 + 白色光标箭头 + 底部记录线 ——「定位到应用」语义。
    """
    from PIL import Image, ImageDraw

    s = max(16, int(size))
    top, bottom = (59, 130, 246), (29, 63, 168)

    img = Image.new("RGBA", (s, s), (0, 0, 0, 0))
    gd = ImageDraw.Draw(img)
    for y in range(s):
        t = y / max(1, s - 1)
        gd.line([(0, y), (s, y)], fill=(*(round(top[i] + (bottom[i] - top[i]) * t) for i in range(3)), 255))

    pad = max(0, round(s * 0.045))
    radius = max(2, round(s * 0.235))
    mask = Image.new("L", (s, s), 0)
    ImageDraw.Draw(mask).rounded_rectangle(
        [pad, pad, s - pad - 1, s - pad - 1], radius=radius, fill=255
    )
    img.putalpha(mask)

    d = ImageDraw.Draw(img)
    arrow = [
        (0.30, 0.155), (0.30, 0.705), (0.425, 0.575), (0.535, 0.825),
        (0.640, 0.770), (0.525, 0.530), (0.670, 0.510),
    ]
    d.polygon([(x * s, y * s) for x, y in arrow], fill=(255, 255, 255, 255))
    lw = max(1, round(s * 0.052))
    d.line([(0.27 * s, 0.895 * s), (0.73 * s, 0.895 * s)], fill=(255, 255, 255, 185), width=lw)
    return img
