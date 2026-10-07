r"""把「软件文件夹或 exe 路径」解析成可用于创建快捷方式的目标。

从 DeskPin 项目移植（原 deskpin/src/resolver.py，2026-10-07 随能力并入）。
文件夹内有多个 exe 时的排序策略：
1. 名字与文件夹名匹配的优先（Foo\Foo.exe > Foo\bar.exe）
2. 名字不含 setup / unins / helper 等噪音词的优先
3. 相对深度浅的优先（顶层 > 子目录）
4. 文件更大的优先（主程序通常比辅助组件大）
"""
from __future__ import annotations

import os
from dataclasses import dataclass, field

MAX_CANDIDATES = 40
MAX_SCAN_FILES = 5000
MAX_DEPTH = 2

_SKIP_DIRS = {
    "$recycle.bin", "system volume information", "__pycache__", ".git", ".svn", ".hg",
    "node_modules", "locales", "locale", "resources", "plugins", "redist", "crashpad",
    "uninstall", "uninstaller", "installer", "updater", "updates", "temp", "tmp",
    "cache", "logs", "docs", "samples", "examples", "tools", "vc_redist", "python",
}

_NOISE_WORDS = (
    "setup", "install", "unins", "uninstall", "update", "updater", "helper", "helpers",
    "crashpad", "crashreport", "crashhandler", "vcredist", "dxsetup", "repair",
    "elevate", "elevation", "watchdog", "service", "daemon", "unpacked",
)


@dataclass
class Candidate:
    exe: str
    depth: int = 0
    size: int = 0


@dataclass
class Resolution:
    ok: bool
    message: str = ""
    source: str = ""
    kind: str = ""                 # exe | folder
    target: str | None = None
    workdir: str | None = None
    icon_source: str | None = None
    default_name: str = ""
    candidates: list[Candidate] = field(default_factory=list)


def clean_input(raw: str) -> str:
    s = (raw or "").strip()
    if len(s) >= 2 and s[0] == s[-1] and s[0] in ('"', "'"):
        s = s[1:-1].strip()
    s = os.path.expandvars(s)
    if len(s) > 3 and s[-1] in "\\/":
        s = s.rstrip("\\/")  # 保留 "C:\" 这类根路径
    return os.path.expanduser(s)


def _is_noisy(filename: str) -> bool:
    stem = os.path.splitext(filename)[0].lower()
    return any(w in stem for w in _NOISE_WORDS)


def _collect_exes(folder: str) -> list[Candidate]:
    found: list[Candidate] = []
    scanned = 0
    root = os.path.normpath(folder)
    for cur, dirnames, filenames in os.walk(root):
        dirnames[:] = [
            d for d in dirnames if d.lower() not in _SKIP_DIRS and not d.startswith(".")
        ]
        depth = 0 if os.path.normcase(cur) == os.path.normcase(root) else os.path.relpath(cur, root).count(os.sep) + 1
        if depth > MAX_DEPTH:
            dirnames[:] = []
            continue
        for fn in filenames:
            scanned += 1
            if scanned > MAX_SCAN_FILES:
                return found
            if not fn.lower().endswith(".exe"):
                continue
            full = os.path.join(cur, fn)
            try:
                size = os.path.getsize(full)
            except OSError:
                size = 0
            found.append(Candidate(full, depth, size))
    return found


def _rank(folder: str, cand: Candidate) -> tuple:
    stem = os.path.splitext(os.path.basename(cand.exe))[0].lower()
    folder_name = os.path.basename(os.path.normpath(folder)).lower()
    name_match = 0 if (stem == folder_name or (len(stem) > 3 and stem in folder_name)) else 1
    noisy = 1 if _is_noisy(cand.exe) else 0
    return (noisy, name_match, cand.depth, -cand.size, stem)


def resolve(raw: str) -> Resolution:
    src = clean_input(raw)
    if not src:
        return Resolution(False, "请输入路径，或把 exe / 文件夹拖进来")
    if not os.path.exists(src):
        return Resolution(False, f"路径不存在：{src}", source=src)

    if os.path.isfile(src):
        if not src.lower().endswith(".exe"):
            return Resolution(False, "这是一个文件，但不是 .exe。请选择可执行文件或软件文件夹。", source=src)
        stem = os.path.splitext(os.path.basename(src))[0]
        return Resolution(
            ok=True, message="已识别可执行文件", source=src, kind="exe",
            target=src, workdir=os.path.dirname(src), icon_source=src, default_name=stem,
        )

    candidates = _collect_exes(src)
    if not candidates:
        return Resolution(False, "这个文件夹（含 2 层子目录）里没有找到 .exe", source=src)

    candidates.sort(key=lambda c: _rank(src, c))
    top = candidates[0]
    folder_name = os.path.basename(os.path.normpath(src))
    return Resolution(
        ok=True,
        message=f"找到 {len(candidates)} 个候选，已按主程序可能性排序" if len(candidates) > 1 else "已识别主程序",
        source=src,
        kind="folder",
        target=top.exe,
        workdir=os.path.dirname(top.exe),
        icon_source=top.exe,
        default_name=folder_name or os.path.splitext(os.path.basename(top.exe))[0],
        candidates=candidates[:MAX_CANDIDATES],
    )
