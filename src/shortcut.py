"""桌面快捷方式：创建 / 读取 .lnk（COM，ctypes 零依赖）。

从 DeskPin 项目移植（与 deskpin/src/winapi.py 同源）：
- `IShellLinkW` + `IPersistFile` 创建 .lnk，可设起始位置 / 图标 / 描述
- `SHGetKnownFolderPath` 取桌面真实路径（兼容 OneDrive 重定向）
- 名称清洗 + 桌面同名自动编号（**绝不覆盖**用户已有快捷方式）

vtable 索引按官方接口声明顺序手写，改动前对照
`shobjidl_core.h`（IShellLinkW）与 `objidl.h`（IPersistFile）。

⚠️ 本文件与 deskpin/src/winapi.py 同源，其中任一处修正时请同步另一处。
"""
from __future__ import annotations

import ctypes
import os
import sys
from ctypes import POINTER, byref, c_int, c_void_p
from ctypes import wintypes

IS_WINDOWS = sys.platform == "win32"

if IS_WINDOWS:
    ole32 = ctypes.WinDLL("ole32", use_last_error=True)
    shell32 = ctypes.WinDLL("shell32", use_last_error=True)
else:  # pragma: no cover
    ole32 = shell32 = None

HRESULT = ctypes.c_long

CLSCTX_INPROC_SERVER = 1
COINIT_APARTMENTTHREADED = 2
S_OK, S_FALSE, RPC_E_CHANGED_MODE = 0, 1, -2147417850
SW_SHOWNORMAL = 1
SLGP_RAWPATH = 4

_CLSID_SHELLLINK = "{00021401-0000-0000-C000-000000000046}"
_IID_ISHELLLINKW = "{000214F9-0000-0000-C000-000000000046}"
_IID_IPERSISTFILE = "{0000010B-0000-0000-C000-000000000046}"
_FOLDERID_DESKTOP = "{B4BFCC3A-DB2C-424C-B029-7FE99A87C641}"


class GUID(ctypes.Structure):
    _fields_ = [
        ("Data1", wintypes.DWORD),
        ("Data2", wintypes.WORD),
        ("Data3", wintypes.WORD),
        ("Data4", ctypes.c_ubyte * 8),
    ]


def _guid(text: str) -> GUID:
    ole32.CLSIDFromString.argtypes = [wintypes.LPCWSTR, POINTER(GUID)]
    ole32.CLSIDFromString.restype = HRESULT
    g = GUID()
    hr = ole32.CLSIDFromString(text, byref(g))
    if hr < 0:
        raise OSError(f"CLSIDFromString 失败: {text} (0x{hr & 0xFFFFFFFF:08X})")
    return g


if IS_WINDOWS:
    _CLSID_ShellLink = _guid(_CLSID_SHELLLINK)
    _IID_IShellLinkW = _guid(_IID_ISHELLLINKW)
    _IID_IPersistFile = _guid(_IID_IPERSISTFILE)
    _FOLDERID_Desktop = _guid(_FOLDERID_DESKTOP)
else:  # pragma: no cover
    _CLSID_ShellLink = _IID_IShellLinkW = _IID_IPersistFile = _FOLDERID_Desktop = GUID()


def _vt(ptr, index: int, restype, *argtypes):
    """按 vtable 索引取 COM 方法，返回已绑定 this 指针的可调用对象。"""
    vtbl = ctypes.cast(ptr, POINTER(POINTER(c_void_p))).contents
    proto = ctypes.WINFUNCTYPE(restype, c_void_p, *argtypes)
    fn = proto(vtbl[index])
    return lambda *args: fn(ptr, *args)


def _check(hr: int, what: str) -> None:
    if hr < 0:
        raise OSError(f"{what} 失败 (HRESULT 0x{hr & 0xFFFFFFFF:08X})")


def _release(ptr) -> None:
    if ptr:
        try:
            _vt(ptr, 2, ctypes.c_ulong)()
        except Exception:
            pass


# ---------------------------------------------------------------- COM 生命周期


def com_initialize() -> bool:
    """在当前线程初始化 COM（幂等）。"""
    if not IS_WINDOWS:
        return False
    ole32.CoInitializeEx.argtypes = [c_void_p, wintypes.DWORD]
    ole32.CoInitializeEx.restype = HRESULT
    hr = ole32.CoInitializeEx(None, COINIT_APARTMENTTHREADED)
    return hr in (S_OK, S_FALSE, RPC_E_CHANGED_MODE)


def _new_shell_link():
    ole32.CoCreateInstance.argtypes = [
        POINTER(GUID), c_void_p, wintypes.DWORD, POINTER(GUID), POINTER(c_void_p)
    ]
    ole32.CoCreateInstance.restype = HRESULT
    psl = c_void_p()
    hr = ole32.CoCreateInstance(
        byref(_CLSID_ShellLink), None, CLSCTX_INPROC_SERVER, byref(_IID_IShellLinkW), byref(psl)
    )
    _check(hr, "CoCreateInstance(ShellLink)")
    if not psl:
        raise OSError("CoCreateInstance 返回空接口")
    return psl


# IShellLinkW vtable: 3 GetPath / 7 SetDescription / 9 SetWorkingDirectory /
# 11 SetArguments / 15 SetShowCmd / 17 SetIconLocation / 20 SetPath
# IPersistFile vtable: 5 Load / 6 Save


def create_shortcut(
    lnk_path: str,
    target: str,
    workdir: str | None = None,
    arguments: str | None = None,
    icon: str | None = None,
    description: str | None = None,
    show_cmd: int = SW_SHOWNORMAL,
) -> None:
    """创建 .lnk 快捷方式。失败抛 OSError。"""
    psl = _new_shell_link()
    ppf = c_void_p()
    try:
        _check(_vt(psl, 20, HRESULT, wintypes.LPCWSTR)(str(target)), "SetPath")
        if workdir:
            _check(_vt(psl, 9, HRESULT, wintypes.LPCWSTR)(str(workdir)), "SetWorkingDirectory")
        if arguments:
            _check(_vt(psl, 11, HRESULT, wintypes.LPCWSTR)(str(arguments)), "SetArguments")
        if description:
            _check(_vt(psl, 7, HRESULT, wintypes.LPCWSTR)(str(description)), "SetDescription")
        if icon:
            _check(_vt(psl, 17, HRESULT, wintypes.LPCWSTR, c_int)(str(icon), 0), "SetIconLocation")
        _check(_vt(psl, 15, HRESULT, c_int)(int(show_cmd)), "SetShowCmd")

        _check(
            _vt(psl, 0, HRESULT, POINTER(GUID), POINTER(c_void_p))(
                byref(_IID_IPersistFile), byref(ppf)
            ),
            "QueryInterface(IPersistFile)",
        )
        _check(_vt(ppf, 6, HRESULT, wintypes.LPCWSTR, wintypes.BOOL)(str(lnk_path), True), "Save")
    finally:
        _release(ppf)
        _release(psl)


def read_shortcut(lnk_path: str) -> dict:
    """读回 .lnk 的目标 / 工作目录（校验用）。"""
    psl = _new_shell_link()
    ppf = c_void_p()
    try:
        _check(
            _vt(psl, 0, HRESULT, POINTER(GUID), POINTER(c_void_p))(
                byref(_IID_IPersistFile), byref(ppf)
            ),
            "QueryInterface(IPersistFile)",
        )
        _check(_vt(ppf, 5, HRESULT, wintypes.LPCWSTR, wintypes.DWORD)(str(lnk_path), 0), "Load")

        target = ctypes.create_unicode_buffer(1024)
        _check(
            _vt(psl, 3, HRESULT, wintypes.LPWSTR, c_int, c_void_p, wintypes.DWORD)(
                target, 1024, None, SLGP_RAWPATH
            ),
            "GetPath",
        )
        workdir = ctypes.create_unicode_buffer(1024)
        _vt(psl, 8, HRESULT, wintypes.LPWSTR, c_int)(workdir, 1024)
        return {"target": target.value, "workdir": workdir.value}
    finally:
        _release(ppf)
        _release(psl)


# ---------------------------------------------------------------- 桌面路径与命名


def desktop_dir() -> str:
    """桌面真实位置（OneDrive 重定向时也正确）。"""
    if IS_WINDOWS:
        try:
            shell32.SHGetKnownFolderPath.argtypes = [
                POINTER(GUID), wintypes.DWORD, wintypes.HANDLE, POINTER(ctypes.c_wchar_p)
            ]
            shell32.SHGetKnownFolderPath.restype = HRESULT
            ole32.CoTaskMemFree.argtypes = [c_void_p]
            buf = ctypes.c_wchar_p()
            hr = shell32.SHGetKnownFolderPath(byref(_FOLDERID_Desktop), 0, None, byref(buf))
            if hr >= 0 and buf.value:
                path = buf.value
                ole32.CoTaskMemFree(ctypes.cast(buf, c_void_p))
                return path
        except Exception:
            pass
    return os.path.join(os.environ.get("USERPROFILE", ""), "Desktop")


_ILLEGAL_NAME_CHARS = set('\\/:*?"<>|')


def sanitize_name(text: str) -> str:
    """清理 Windows 文件名非法字符。"""
    out = "".join(ch for ch in (text or "").strip() if ch not in _ILLEGAL_NAME_CHARS)
    return out.strip(" .")[:120]


def default_shortcut_name(exe_path: str) -> str:
    stem = os.path.splitext(os.path.basename(exe_path))[0]
    return sanitize_name(stem) or "快捷方式"


def unique_lnk_path(desktop: str, base_name: str) -> str:
    """桌面同名时自动编号：App.lnk → App (2).lnk → …（绝不覆盖已有文件）。"""
    candidate = os.path.join(desktop, f"{base_name}.lnk")
    if not os.path.exists(candidate):
        return candidate
    for i in range(2, 100):
        candidate = os.path.join(desktop, f"{base_name} ({i}).lnk")
        if not os.path.exists(candidate):
            return candidate
    return os.path.join(desktop, f"{base_name} ({os.getpid()}).lnk")
