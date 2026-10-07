"""运行时路径模型：可写数据与只读资源分开定位。

打包（PyInstaller onefile）后 sys._MEIPASS 是只读临时解包目录，退出即清空，
凡是以 __file__ 为基准推导可写路径的位置全部失效。因此：

- 一切可写数据（历史库 / 日志）→ %LOCALAPPDATA%\\ExeTrace（可用环境变量覆盖）；
- 只读资源（托盘图标等）→ 打包后位于 sys._MEIPASS。

数据目录与 exe 所在位置解耦：exe 被移动到任何地方，历史都不会丢。
"""
from __future__ import annotations

import json
import os
import sys
from pathlib import Path

APP_NAME = "ExeTrace"
APP_TITLE = "ExeTrace — 应用历史定位器"
APP_VERSION = "3.1.0"

_ENV_OVERRIDE = "EXETRACE_DATA_DIR"


def is_frozen() -> bool:
    """是否运行在 PyInstaller 打包后的 exe 中。"""
    return bool(getattr(sys, "frozen", False))


def resource_dir() -> Path:
    """只读资源目录（打包后 = 解包临时目录）。"""
    if is_frozen():
        return Path(getattr(sys, "_MEIPASS", Path(sys.executable).parent))
    return Path(__file__).resolve().parent


def data_dir() -> Path:
    """可写数据目录。绝不依赖 __file__ / _MEIPASS。"""
    override = os.environ.get(_ENV_OVERRIDE)
    if override:
        d = Path(override)
    else:
        base = os.environ.get("LOCALAPPDATA") or str(Path.home() / "AppData" / "Local")
        d = Path(base) / APP_NAME
    d.mkdir(parents=True, exist_ok=True)
    return d


def db_path() -> Path:
    return data_dir() / "history.db"


def log_path() -> Path:
    return data_dir() / "exetrace.log"


def runtime_exe() -> str:
    """当前进程对应的可执行文件。（冻结后即 ExeTrace.exe 自身）"""
    return os.path.abspath(sys.executable)


def config_path() -> Path:
    return data_dir() / "config.json"


def load_config() -> dict:
    """读取用户配置；不存在或损坏时返回空 dict（绝不抛）。"""
    p = config_path()
    if not p.is_file():
        return {}
    try:
        data = json.loads(p.read_text(encoding="utf-8"))
        return data if isinstance(data, dict) else {}
    except (OSError, ValueError):
        return {}


def save_config(cfg: dict) -> None:
    config_path().write_text(
        json.dumps(cfg, ensure_ascii=False, indent=2), encoding="utf-8"
    )


def autostart_command() -> str:
    """写入 HKCU\\...\\Run 的完整命令行。"""
    if is_frozen():
        return f'"{runtime_exe()}" --minimized'
    pyw = Path(sys.executable).with_name("pythonw.exe")
    launcher = pyw if pyw.exists() else Path(sys.executable)
    entry = Path(__file__).resolve().parent / "main.py"
    return f'"{launcher}" "{entry}" --minimized'
