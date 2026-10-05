"""系统托盘：常驻入口（关闭主窗口后仍在后台记录）。

pystray 运行在独立线程（Windows 后端为纯 ctypes 消息循环）。菜单回调
发生在托盘线程，因此外部传入的 on_show / on_quit 必须自行调度回主线程。
"""
from __future__ import annotations

import logging
import threading

import paths

log = logging.getLogger("exetrace.tray")


class TrayIcon:
    def __init__(self, on_show, on_quit, is_autostart, toggle_autostart,
                 is_paused=None, toggle_pause=None):
        self._on_show = on_show
        self._on_quit = on_quit
        self._is_autostart = is_autostart
        self._toggle_autostart = toggle_autostart
        self._is_paused = is_paused
        self._toggle_pause = toggle_pause
        self._icon = None
        self.available = False

    # ------------------------------------------------------------ 生命周期

    def start(self) -> bool:
        try:
            import pystray
            import winutil

            image = winutil.make_app_icon_image(64)
            menu = pystray.Menu(
                pystray.MenuItem("打开 ExeTrace", self._handle_show, default=True),
                pystray.MenuItem(
                    "暂停记录",
                    self._handle_toggle_pause,
                    checked=lambda _item: bool(self._is_paused and self._is_paused()),
                ),
                pystray.MenuItem(
                    "开机自动启动",
                    self._handle_toggle_autostart,
                    checked=lambda _item: self._is_autostart(),
                ),
                pystray.Menu.SEPARATOR,
                pystray.MenuItem("退出", self._handle_quit),
            )
            self._icon = pystray.Icon("ExeTrace", image, paths.APP_TITLE, menu)
            threading.Thread(target=self._icon.run, name="ExeTrace-Tray", daemon=True).start()
            self.available = True
            log.info("托盘已启动")
            return True
        except Exception as exc:
            log.warning("托盘启动失败（降级为普通窗口程序）: %s", exc)
            self.available = False
            return False

    def stop(self) -> None:
        if self._icon is not None:
            try:
                self._icon.stop()
            except Exception:
                pass

    def update_menu(self) -> None:
        """外部状态变化后刷新勾选态（pystray 的 update_menu 可在任意线程调用）。"""
        if self._icon is not None:
            try:
                self._icon.update_menu()
            except Exception:
                pass

    def notify(self, message: str, title: str | None = None) -> None:
        if self._icon is None:
            return
        try:
            self._icon.notify(message, title or paths.APP_NAME)
        except Exception:
            pass

    # ------------------------------------------------------------ 菜单回调

    def _handle_show(self, _icon=None, _item=None):
        try:
            self._on_show()
        except Exception as exc:
            log.warning("显示窗口失败: %s", exc)

    def _handle_quit(self, _icon=None, _item=None):
        try:
            self._on_quit()
        except Exception as exc:
            log.warning("退出失败: %s", exc)

    def _handle_toggle_autostart(self, _icon=None, _item=None):
        try:
            self._toggle_autostart()
            if self._icon is not None:
                self._icon.update_menu()
        except Exception as exc:
            log.warning("切换自启失败: %s", exc)

    def _handle_toggle_pause(self, _icon=None, _item=None):
        if self._toggle_pause is None:
            return
        try:
            self._toggle_pause()
            self.update_menu()
        except Exception as exc:
            log.warning("切换暂停失败: %s", exc)
