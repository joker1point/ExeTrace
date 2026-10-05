"""冷启动扫描：立刻从系统已有的「使用痕迹」里恢复历史。

数据源（全部是当前用户 HKCU，无需管理员权限）：

1. Explorer\\FeatureUsage\\AppLaunch   —— 路径 + 启动次数 + 最后启动时间（信息最全）
2. Explorer\\FeatureUsage\\AppSwitched —— 用户主动切换过的应用（说明真实使用过）
3. Shell\\MuiCache                     —— 值名本身就是 exe 完整路径（无时间信息）

只做"读"，不改注册表。扫描结果作为种子写入历史库，与实时监控互补：
扫描负责"过去打开过的"，监控负责"从现在起打开过的"。
"""
from __future__ import annotations

import logging
import os
import re
import struct
import time
import winreg

import winutil

log = logging.getLogger("exetrace.scanner")

_FEATURE_USAGE = r"Software\Microsoft\Windows\CurrentVersion\Explorer\FeatureUsage"
_MUI_CACHE = r"Software\Classes\Local Settings\Software\Microsoft\Windows\Shell\MuiCache"

_MUI_SUFFIX_RE = re.compile(
    r"\.(FriendlyAppName|ApplicationCompany|ApplicationName|AppUserModelID)$", re.IGNORECASE
)

# 合理性窗口：解析出的时间戳必须落在 2000-01-01 之后且不超过当前时间一天
_MIN_TS = 946_684_800.0
_MAX_TS_SKEW = 86_400.0


def _filetime_to_unix(ft: int) -> float | None:
    try:
        ts = ft / 10_000_000.0 - 11_644_473_600.0
    except Exception:
        return None
    if _MIN_TS < ts < time.time() + _MAX_TS_SKEW:
        return ts
    return None


def _valid_exe(path: str) -> bool:
    return (
        isinstance(path, str)
        and path.lower().endswith(".exe")
        and os.path.isfile(path)
        and not winutil.is_noise_exe(path)
    )


def _enum_values(root, subkey: str):
    """枚举注册表子键下的所有值：(name, data, type)。键不存在则静默返回空。"""
    try:
        with winreg.OpenKey(root, subkey) as key:
            i = 0
            while True:
                try:
                    yield winreg.EnumValue(key, i)
                except OSError:
                    return
                i += 1
    except OSError:
        return


def scan_feature_usage() -> list[tuple[str, int | None, float | None]]:
    """AppLaunch / AppSwitched：返回 (路径, 次数, 最后时间)。"""
    found: dict[str, tuple[int | None, float | None]] = {}

    for name, data, vtype in _enum_values(winreg.HKEY_CURRENT_USER, _FEATURE_USAGE + r"\AppLaunch"):
        if vtype != winreg.REG_BINARY or not isinstance(name, str):
            continue
        if not name.lower().endswith(".exe"):
            continue  # UWP 的 AppUserModelID 形态，跳过
        count = last = None
        try:
            if len(data) >= 8:
                last = _filetime_to_unix(struct.unpack_from("<Q", data, 0)[0])
            if len(data) >= 12:
                count = struct.unpack_from("<I", data, 8)[0]
        except struct.error:
            continue
        if not _valid_exe(name):
            continue
        prev = found.get(name)
        if prev is None:
            found[name] = (count, last)
        else:
            found[name] = (max(prev[0] or 0, count or 0) or None, max(prev[1] or 0, last or 0) or None)

    for name, data, vtype in _enum_values(winreg.HKEY_CURRENT_USER, _FEATURE_USAGE + r"\AppSwitched"):
        if not isinstance(name, str) or not name.lower().endswith(".exe"):
            continue
        if not _valid_exe(name):
            continue
        if name not in found:
            found[name] = (None, None)

    return [(p, c, t) for p, (c, t) in found.items()]


def scan_muicache() -> list[str]:
    """MuiCache：值名剥掉 .FriendlyAppName 等后缀即 exe 路径。"""
    out: set[str] = set()
    for name, _data, _vtype in _enum_values(winreg.HKEY_CURRENT_USER, _MUI_CACHE):
        if not isinstance(name, str):
            continue
        candidate = _MUI_SUFFIX_RE.sub("", name).strip()
        if candidate.lower().endswith(".exe") and _valid_exe(candidate):
            out.add(candidate)
    return sorted(out)


def scan_all(store) -> dict[str, int]:
    """执行全部扫描并写入历史库，返回统计信息。"""
    stats = {"launch": 0, "switched": 0, "muicache": 0, "total": 0}
    try:
        entries = scan_feature_usage()
        for path, count, last in entries:
            store.upsert_seed(path, launch_count=count, last_seen=last)
            stats["launch"] += 1
    except Exception as exc:  # 单源失败不影响其它源
        log.warning("FeatureUsage 扫描失败: %s", exc)

    try:
        for path, _c, _t in ((p, None, None) for p in scan_muicache()):
            store.upsert_seed(path)
            stats["muicache"] += 1
    except Exception as exc:
        log.warning("MuiCache 扫描失败: %s", exc)

    stats["total"] = len(store.query(limit=100000))
    log.info("冷启动扫描完成: %s", stats)
    return stats
