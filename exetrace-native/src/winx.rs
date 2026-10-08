//! Windows 平台能力（windows-sys 直接 FFI，逻辑对齐 Python 版 winutil.py）。
//!
//! 涵盖：可见窗口枚举 / 进程快照 / exe 图标提取 / 噪音进程过滤 /
//! 开机自启（注册表）/ 单实例（互斥体）/ 启动与资源管理器定位。

use std::collections::HashSet;
use windows_sys::Win32::Foundation::{CloseHandle, GetLastError, HANDLE, HWND, LPARAM};
use windows_sys::Win32::Graphics::Gdi::{
    CreateCompatibleDC, CreateDIBSection, DeleteDC, DeleteObject, SelectObject, BITMAPINFO,
    BITMAPINFOHEADER, DIB_RGB_COLORS, HBITMAP, HDC, HGDIOBJ,
};
use windows_sys::Win32::System::Diagnostics::ToolHelp::{
    CreateToolhelp32Snapshot, Process32FirstW, Process32NextW, PROCESSENTRY32W, TH32CS_SNAPPROCESS,
};
use windows_sys::Win32::System::Threading::{
    OpenProcess, QueryFullProcessImageNameW, PROCESS_QUERY_LIMITED_INFORMATION,
};
use windows_sys::Win32::UI::Shell::{
    ShellExecuteW, SHGetFileInfoW, SHFILEINFOW, SHGFI_ICON, SHGFI_LARGEICON,
};
use windows_sys::Win32::UI::WindowsAndMessaging::{
    DestroyIcon, DrawIconEx, EnumWindows, GetWindowThreadProcessId, IsWindowVisible, MessageBoxW,
    SendMessageTimeoutW, DI_NORMAL, MB_ICONINFORMATION, MB_OK, SMTO_ABORTIFHUNG, SW_SHOWNORMAL,
    WM_GETTEXTLENGTH,
};

pub fn wide(s: &str) -> Vec<u16> {
    s.encode_utf16().chain(std::iter::once(0)).collect()
}

// ---------------------------------------------------------------- 噪音过滤

const NAME_BLOCK: &[&str] = &[
    "conhost.exe", "dllhost.exe", "backgroundtaskhost.exe", "runtimebroker.exe",
    "werfault.exe", "taskhostw.exe", "msiexec.exe", "wmiprvse.exe", "smartscreen.exe",
    "securityhealthsystray.exe", "securityhealthservice.exe", "searchindexer.exe",
    "searchapp.exe", "searchhost.exe", "sihost.exe", "ctfmon.exe", "textinputhost.exe",
    "startmenuexperiencehost.exe", "shellexperiencehost.exe", "applicationframehost.exe",
    "explorer.exe", "usoclient.exe", "sppsvc.exe", "csrss.exe", "lsass.exe", "services.exe",
    "winlogon.exe", "wininit.exe", "registry.exe", "fontdrvhost.exe", "spoolsv.exe",
    "audiodg.exe", "dwm.exe", "msedgewebview2.exe", "crashpad_handler.exe",
    "elevation_service.exe",
];

pub fn is_noise_exe(exe: Option<&str>) -> bool {
    let Some(raw) = exe else { return true };
    let p = raw.replace('/', "\\").to_lowercase();
    if !p.ends_with(".exe") {
        return true;
    }
    let base = p.rsplit('\\').next().unwrap_or("");
    if NAME_BLOCK.contains(&base) {
        return true;
    }
    let windir = std::env::var("WINDIR").unwrap_or_else(|_| "C:\\Windows".into()).to_lowercase();
    for prefix in [
        format!("{windir}\\"),
        "\\appdata\\local\\temp\\".to_string(),
        "\\$recycle.bin\\".to_string(),
        "\\windowsapps\\".to_string(),
    ] {
        if p.starts_with(&prefix) || p.contains(&prefix) {
            return true;
        }
    }
    // 自身
    if let Ok(me) = std::env::current_exe() {
        if me.to_string_lossy().to_lowercase() == p {
            return true;
        }
    }
    false
}

// ---------------------------------------------------------------- 窗口 / 进程

pub fn visible_window_pids() -> HashSet<u32> {
    let mut pids: HashSet<u32> = HashSet::new();
    let ptr = &mut pids as *mut HashSet<u32> as LPARAM;
    unsafe {
        EnumWindows(Some(enum_windows_cb), ptr);
    }
    pids
}

unsafe extern "system" fn enum_windows_cb(hwnd: HWND, lparam: LPARAM) -> i32 {
    let pids = &mut *(lparam as *mut HashSet<u32>);
    let mut pid: u32 = 0;
    GetWindowThreadProcessId(hwnd, &mut pid);
    // 跳过本进程自己的窗口：对自有窗口取文本会同步等待本进程主线程，
    // 在优化构建/重负载时序下曾让监控线程长时间阻塞（表现为记录停摆）。
    if pid == 0 || pid == std::process::id() {
        return 1;
    }
    if IsWindowVisible(hwnd) == 0 {
        return 1;
    }
    // 用带超时的调用，避免被无响应的第三方窗口拖住
    let mut len: usize = 0;
    let ok = SendMessageTimeoutW(
        hwnd,
        WM_GETTEXTLENGTH,
        0,
        0,
        SMTO_ABORTIFHUNG,
        200,
        &mut len,
    );
    if ok == 0 || len == 0 {
        return 1;
    }
    pids.insert(pid);
    1
}

#[derive(Debug, Clone)]
pub struct ProcEntry {
    pub pid: u32,
    pub ppid: u32,
    pub name: String,
}

/// 进程快照（Toolhelp；不含 exe 路径，路径按需单独查询以省开销）。
pub fn snapshot_processes() -> Vec<ProcEntry> {
    let mut out = Vec::new();
    unsafe {
        let snap = CreateToolhelp32Snapshot(TH32CS_SNAPPROCESS, 0);
        if snap.is_null() || snap as isize == -1 {
            return out;
        }
        let mut entry: PROCESSENTRY32W = std::mem::zeroed();
        entry.dwSize = std::mem::size_of::<PROCESSENTRY32W>() as u32;
        if Process32FirstW(snap, &mut entry) != 0 {
            loop {
                let name = utf16_array_to_string(&entry.szExeFile);
                out.push(ProcEntry {
                    pid: entry.th32ProcessID,
                    ppid: entry.th32ParentProcessID,
                    name,
                });
                if Process32NextW(snap, &mut entry) == 0 {
                    break;
                }
            }
        }
        CloseHandle(snap);
    }
    out
}

fn utf16_array_to_string(buf: &[u16]) -> String {
    let end = buf.iter().position(|&c| c == 0).unwrap_or(buf.len());
    String::from_utf16_lossy(&buf[..end])
}

pub fn process_exe_path(pid: u32) -> Option<String> {
    unsafe {
        let h = OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, 0, pid);
        if h.is_null() {
            return None;
        }
        let mut buf = vec![0u16; 1024];
        let mut size = buf.len() as u32;
        let ok = QueryFullProcessImageNameW(h, 0, buf.as_mut_ptr(), &mut size);
        CloseHandle(h);
        if ok == 0 {
            None
        } else {
            Some(String::from_utf16_lossy(&buf[..size as usize]))
        }
    }
}

// ---------------------------------------------------------------- 图标提取

pub fn extract_icon_rgba(exe_path: &str, size: i32) -> Option<(usize, usize, Vec<u8>)> {
    if size <= 0 || size > 512 {
        return None;
    }
    unsafe {
        let mut shfi: SHFILEINFOW = std::mem::zeroed();
        let wpath = wide(exe_path);
        let ret = SHGetFileInfoW(
            wpath.as_ptr(),
            0,
            &mut shfi,
            std::mem::size_of::<SHFILEINFOW>() as u32,
            SHGFI_ICON | SHGFI_LARGEICON,
        );
        if ret == 0 || shfi.hIcon.is_null() {
            return None;
        }
        let result = draw_icon_to_rgba(shfi.hIcon, size);
        DestroyIcon(shfi.hIcon);
        result
    }
}

unsafe fn draw_icon_to_rgba(hicon: windows_sys::Win32::UI::WindowsAndMessaging::HICON, size: i32) -> Option<(usize, usize, Vec<u8>)> {
    let mut bmi: BITMAPINFO = std::mem::zeroed();
    bmi.bmiHeader.biSize = std::mem::size_of::<BITMAPINFOHEADER>() as u32;
    bmi.bmiHeader.biWidth = size;
    bmi.bmiHeader.biHeight = -size; // top-down
    bmi.bmiHeader.biPlanes = 1;
    bmi.bmiHeader.biBitCount = 32;
    bmi.bmiHeader.biCompression = 0; // BI_RGB

    let mut bits: *mut std::ffi::c_void = std::ptr::null_mut();
    let hbitmap: HBITMAP = CreateDIBSection(
        std::ptr::null_mut() as HDC,
        &bmi,
        DIB_RGB_COLORS,
        &mut bits,
        std::ptr::null_mut(),
        0,
    );
    if hbitmap.is_null() || bits.is_null() {
        return None;
    }
    let hdc = CreateCompatibleDC(std::ptr::null_mut() as HDC);
    let old: HGDIOBJ = SelectObject(hdc, hbitmap as HGDIOBJ);
    let n = (size * size * 4) as usize;
    std::ptr::write_bytes(bits as *mut u8, 0, n);
    let ok = DrawIconEx(hdc, 0, 0, hicon, size, size, 0, std::ptr::null_mut(), DI_NORMAL);
    let mut raw = vec![0u8; n];
    if ok != 0 {
        std::ptr::copy_nonoverlapping(bits as *const u8, raw.as_mut_ptr(), n);
    }
    SelectObject(hdc, old);
    DeleteObject(hbitmap as HGDIOBJ);
    DeleteDC(hdc);
    if ok == 0 {
        return None;
    }
    // GDI 输出是预乘 alpha，还原为直通 alpha
    let mut i = 0usize;
    while i + 3 < raw.len() {
        let a = raw[i + 3] as u32;
        if a > 0 && a < 255 {
            for k in 0..3 {
                let v = raw[i + k] as u32 * 255 / a;
                raw[i + k] = v.min(255) as u8;
            }
        }
        i += 4;
    }
    Some((size as usize, size as usize, raw))
}

// ---------------------------------------------------------------- Shell 交互

pub fn launch_path(path: &str) -> bool {
    unsafe {
        let op = wide("open");
        let file = wide(path);
        let r = ShellExecuteW(
            std::ptr::null_mut(),
            op.as_ptr(),
            file.as_ptr(),
            std::ptr::null(),
            std::ptr::null(),
            SW_SHOWNORMAL,
        ) as isize;
        r > 32
    }
}

pub fn reveal_in_explorer(path: &str) {
    let folder = std::path::Path::new(path)
        .parent()
        .map(|p| p.to_path_buf())
        .unwrap_or_else(|| std::path::PathBuf::from(path));
    if std::path::Path::new(path).is_file() {
        let _ = std::process::Command::new("explorer")
            .arg(format!("/select,{}", path))
            .spawn();
    } else {
        let _ = std::process::Command::new("explorer").arg(folder).spawn();
    }
}

// ---------------------------------------------------------------- 消息框

/// 简单的模态提示（无父窗口 → 居中于屏幕；用于主窗口尚未创建的早期错误）。
pub fn message_box(text: &str) {
    message_box_on(std::ptr::null_mut(), text);
}

/// 指定父窗口的模态提示：对话框跟随父窗口居中，并始终显示在父窗口之上。
/// 主窗口被置顶时，无父窗口的对话框会被压在下面看不见。
pub fn message_box_on(parent: HWND, text: &str) {
    unsafe {
        let title = wide(crate::paths::APP_TITLE);
        let body = wide(text);
        MessageBoxW(
            parent,
            body.as_ptr(),
            title.as_ptr(),
            MB_OK | MB_ICONINFORMATION,
        );
    }
}

// ---------------------------------------------------------------- 单实例

pub fn acquire_single_instance() -> Option<HANDLE> {
    unsafe {
        let name = wide("Local\\ExeTrace_SingleInstance_9F3C");
        let h = windows_sys::Win32::System::Threading::CreateMutexW(
            std::ptr::null_mut(),
            0,
            name.as_ptr(),
        );
        if h.is_null() {
            return Some(std::ptr::null_mut());
        }
        const ERROR_ALREADY_EXISTS: u32 = 183;
        if GetLastError() == ERROR_ALREADY_EXISTS {
            return None;
        }
        Some(h)
    }
}

// ---------------------------------------------------------------- 开机自启

const RUN_KEY: &str = r"Software\Microsoft\Windows\CurrentVersion\Run";
const RUN_VALUE: &str = "ExeTrace";

pub fn is_autostart_enabled() -> bool {
    use winreg::enums::HKEY_CURRENT_USER;
    use winreg::RegKey;
    RegKey::predef(HKEY_CURRENT_USER)
        .open_subkey(RUN_KEY)
        .and_then(|k| k.get_value::<String, _>(RUN_VALUE))
        .is_ok()
}

pub fn set_autostart(enabled: bool) -> bool {
    use winreg::enums::HKEY_CURRENT_USER;
    use winreg::RegKey;
    let hkcu = RegKey::predef(HKEY_CURRENT_USER);
    let res = (|| -> std::io::Result<()> {
        let (key, _) = hkcu.create_subkey(RUN_KEY)?;
        if enabled {
            key.set_value(RUN_VALUE, &crate::paths::autostart_command())?;
        } else if key.get_value::<String, _>(RUN_VALUE).is_ok() {
            key.delete_value(RUN_VALUE)?;
        }
        Ok(())
    })();
    if res.is_err() {
        log::warn!("写自启注册表失败: {:?}", res.err());
    }
    is_autostart_enabled()
}

// ---------------------------------------------------------------- 桌面路径

pub fn desktop_dir() -> String {
    use windows_sys::Win32::UI::Shell::{
        FOLDERID_Desktop, SHGetKnownFolderPath,
    };
    unsafe {
        let mut ptr: *mut u16 = std::ptr::null_mut();
        let hr = SHGetKnownFolderPath(&FOLDERID_Desktop, 0, std::ptr::null_mut(), &mut ptr);
        if hr >= 0 && !ptr.is_null() {
            let mut len = 0usize;
            while *ptr.add(len) != 0 {
                len += 1;
            }
            let s = String::from_utf16_lossy(std::slice::from_raw_parts(ptr, len));
            windows_sys::Win32::System::Com::CoTaskMemFree(ptr as *const std::ffi::c_void);
            if !s.is_empty() {
                return s;
            }
        }
    }
    let home = std::env::var("USERPROFILE").unwrap_or_default();
    format!("{home}\\Desktop")
}

// ---------------------------------------------------------------- 托盘（自研，独立线程）
//
// 为什么不用 tray-icon：在 eframe(winit) 主线程里创建它，会让本进程的
// 监控线程卡死在窗口枚举上（实测：GUI 模式 watcher 只跑完基线就再无 tick）。
// 自研版在**独立线程**里创建隐藏窗口并跑自己的消息循环，与 winit 完全隔离
// （Python 版 pystray 就是这个思路，所以那边从没出过这个问题）。

mod tray_impl {
    use std::sync::atomic::{AtomicBool, Ordering};
    use std::sync::mpsc::Sender;
    use std::sync::Arc;

    use super::wide;
    use windows_sys::Win32::Foundation::{HWND, LPARAM, LRESULT, POINT, WPARAM};
    use windows_sys::Win32::Graphics::Gdi::{
        CreateBitmap, CreateDIBSection, DeleteObject, BITMAPINFO, BITMAPINFOHEADER, DIB_RGB_COLORS,
        HGDIOBJ,
    };
    use windows_sys::Win32::System::LibraryLoader::GetModuleHandleW;
    use windows_sys::Win32::UI::Shell::{
        Shell_NotifyIconW, NIF_ICON, NIF_MESSAGE, NIF_TIP, NIM_ADD, NIM_DELETE, NOTIFYICONDATAW,
    };
    use windows_sys::Win32::UI::WindowsAndMessaging::{
        AppendMenuW, CreateIconIndirect, CreatePopupMenu, CreateWindowExW, DefWindowProcW,
        DestroyMenu, DestroyWindow, DispatchMessageW, GetCursorPos, GetMessageW, GetWindowLongPtrW,
        PostMessageW, PostQuitMessage, RegisterClassExW, SetForegroundWindow, SetWindowLongPtrW,
        TrackPopupMenu, TranslateMessage, GWLP_USERDATA, ICONINFO, MF_CHECKED, MF_SEPARATOR,
        MF_STRING, MF_UNCHECKED, MSG, TPM_RIGHTBUTTON, WM_CLOSE, WM_COMMAND, WM_CONTEXTMENU,
        WM_DESTROY, WM_LBUTTONDBLCLK, WM_NULL, WM_RBUTTONUP, WNDCLASSEXW,
    };

    const WM_TRAY: u32 = 0x8000 + 1; // WM_APP + 1
    const TRAY_UID: u32 = 1;
    const MENU_OPEN: usize = 1;
    const MENU_AUTOSTART: usize = 2;
    const MENU_QUIT: usize = 3;
    const MENU_PAUSE: usize = 4;

    #[derive(Debug, Clone, Copy)]
    pub enum TrayEvent {
        Show,
        ToggleAutostart,
        TogglePause,
        Quit,
    }

    pub struct TrayHandle {
        hwnd: isize,
        _thread: Option<std::thread::JoinHandle<()>>,
    }

    impl TrayHandle {
        pub fn stop(&self) {
            unsafe {
                PostMessageW(self.hwnd as HWND, WM_CLOSE, 0, 0);
            }
        }
    }

    struct TrayCtx {
        tx: Sender<TrayEvent>,
        autostart: Arc<AtomicBool>,
        paused: Arc<AtomicBool>,
        hwnd: HWND,
    }

    pub fn start(
        tooltip: &str,
        rgba: (usize, usize, Vec<u8>),
        autostart: Arc<AtomicBool>,
        paused: Arc<AtomicBool>,
        tx: Sender<TrayEvent>,
    ) -> Option<TrayHandle> {
        let (w, h, bytes) = rgba;
        let tooltip = tooltip.to_string();
        let (ready_tx, ready_rx) = std::sync::mpsc::sync_channel::<Option<isize>>(1);
        let thread = std::thread::Builder::new()
            .name("ExeTrace-Tray".to_string())
            .spawn(move || {
                let hwnd =
                    unsafe { create_tray_window(&tooltip, w, h, &bytes, autostart, paused, tx) };
                let _ = ready_tx.send(hwnd.map(|h| h as isize));
                if hwnd.is_some() {
                    unsafe { message_loop() };
                }
            })
            .ok()?;
        let hwnd = ready_rx
            .recv_timeout(std::time::Duration::from_secs(5))
            .ok()
            .flatten()?;
        Some(TrayHandle { hwnd, _thread: Some(thread) })
    }

    unsafe fn create_tray_window(
        tooltip: &str,
        w: usize,
        h: usize,
        rgba: &[u8],
        autostart: Arc<AtomicBool>,
        paused: Arc<AtomicBool>,
        tx: Sender<TrayEvent>,
    ) -> Option<HWND> {
        let hicon = rgba_to_hicon(w, h, rgba)?;
        let hinst = GetModuleHandleW(std::ptr::null());
        let class_name = wide("ExeTraceTrayWnd");
        let wc = WNDCLASSEXW {
            cbSize: std::mem::size_of::<WNDCLASSEXW>() as u32,
            lpfnWndProc: Some(wndproc),
            hInstance: hinst,
            lpszClassName: class_name.as_ptr(),
            ..std::mem::zeroed()
        };
        let _ = RegisterClassExW(&wc); // 已注册会返回 0，忽略
        let title = wide("ExeTraceTray");
        let hwnd = CreateWindowExW(
            0,
            class_name.as_ptr(),
            title.as_ptr(),
            0,
            0,
            0,
            0,
            0,
            std::ptr::null_mut(),
            std::ptr::null_mut(),
            hinst,
            std::ptr::null_mut(),
        );
        if hwnd.is_null() {
            return None;
        }
        let ctx = Box::into_raw(Box::new(TrayCtx { tx, autostart, paused, hwnd }));
        SetWindowLongPtrW(hwnd, GWLP_USERDATA, ctx as isize);

        let mut nid: NOTIFYICONDATAW = std::mem::zeroed();
        nid.cbSize = std::mem::size_of::<NOTIFYICONDATAW>() as u32;
        nid.hWnd = hwnd;
        nid.uID = TRAY_UID;
        nid.uFlags = NIF_MESSAGE | NIF_ICON | NIF_TIP;
        nid.uCallbackMessage = WM_TRAY;
        nid.hIcon = hicon;
        let tip = wide(tooltip);
        let n = tip.len().min(nid.szTip.len());
        nid.szTip[..n].copy_from_slice(&tip[..n]);
        if Shell_NotifyIconW(NIM_ADD, &nid) == 0 {
            return None;
        }
        Some(hwnd)
    }

    unsafe fn rgba_to_hicon(
        w: usize,
        h: usize,
        rgba: &[u8],
    ) -> Option<windows_sys::Win32::UI::WindowsAndMessaging::HICON> {
        let mut bmi: BITMAPINFO = std::mem::zeroed();
        bmi.bmiHeader.biSize = std::mem::size_of::<BITMAPINFOHEADER>() as u32;
        bmi.bmiHeader.biWidth = w as i32;
        bmi.bmiHeader.biHeight = -(h as i32);
        bmi.bmiHeader.biPlanes = 1;
        bmi.bmiHeader.biBitCount = 32;
        bmi.bmiHeader.biCompression = 0;
        let mut bits: *mut std::ffi::c_void = std::ptr::null_mut();
        let hbm_color = CreateDIBSection(
            std::ptr::null_mut(),
            &bmi,
            DIB_RGB_COLORS,
            &mut bits,
            std::ptr::null_mut(),
            0,
        );
        if hbm_color.is_null() || bits.is_null() {
            return None;
        }
        let dst = std::slice::from_raw_parts_mut(bits as *mut u8, w * h * 4);
        for i in (0..dst.len()).step_by(4) {
            dst[i] = rgba[i + 2]; // B
            dst[i + 1] = rgba[i + 1]; // G
            dst[i + 2] = rgba[i]; // R
            dst[i + 3] = rgba[i + 3]; // A
        }
        let hbm_mask = CreateBitmap(w as i32, h as i32, 1, 1, std::ptr::null());
        let ii = ICONINFO {
            fIcon: 1,
            xHotspot: 0,
            yHotspot: 0,
            hbmMask: hbm_mask,
            hbmColor: hbm_color,
        };
        let hicon = CreateIconIndirect(&ii);
        DeleteObject(hbm_color as HGDIOBJ);
        DeleteObject(hbm_mask as HGDIOBJ);
        if hicon.is_null() {
            None
        } else {
            Some(hicon)
        }
    }

    unsafe fn ctx_of(hwnd: HWND) -> *mut TrayCtx {
        GetWindowLongPtrW(hwnd, GWLP_USERDATA) as *mut TrayCtx
    }

    unsafe extern "system" fn wndproc(hwnd: HWND, msg: u32, wparam: WPARAM, lparam: LPARAM) -> LRESULT {
        match msg {
            WM_TRAY => {
                let kind = (lparam as u32) & 0xFFFF;
                let ctx = ctx_of(hwnd);
                if !ctx.is_null() {
                    match kind {
                        WM_LBUTTONDBLCLK => {
                            let _ = (*ctx).tx.send(TrayEvent::Show);
                        }
                        WM_RBUTTONUP | WM_CONTEXTMENU => show_menu(&*ctx),
                        _ => {}
                    }
                }
                0
            }
            WM_COMMAND => {
                let id = (wparam as usize) & 0xFFFF;
                let ctx = ctx_of(hwnd);
                if !ctx.is_null() {
                    match id {
                        MENU_OPEN => {
                            let _ = (*ctx).tx.send(TrayEvent::Show);
                        }
                        MENU_AUTOSTART => {
                            let _ = (*ctx).tx.send(TrayEvent::ToggleAutostart);
                        }
                        MENU_PAUSE => {
                            let _ = (*ctx).tx.send(TrayEvent::TogglePause);
                        }
                        MENU_QUIT => {
                            let _ = (*ctx).tx.send(TrayEvent::Quit);
                        }
                        _ => {}
                    }
                }
                0
            }
            WM_CLOSE => {
                DestroyWindow(hwnd);
                0
            }
            WM_DESTROY => {
                let mut nid: NOTIFYICONDATAW = std::mem::zeroed();
                nid.cbSize = std::mem::size_of::<NOTIFYICONDATAW>() as u32;
                nid.hWnd = hwnd;
                nid.uID = TRAY_UID;
                Shell_NotifyIconW(NIM_DELETE, &nid);
                PostQuitMessage(0);
                0
            }
            _ => DefWindowProcW(hwnd, msg, wparam, lparam),
        }
    }

    unsafe fn show_menu(ctx: &TrayCtx) {
        let hmenu = CreatePopupMenu();
        if hmenu.is_null() {
            return;
        }
        let s_open = wide("打开 ExeTrace");
        let s_pause = wide(if ctx.paused.load(Ordering::Relaxed) {
            "恢复记录"
        } else {
            "暂停记录"
        });
        let s_auto = wide("开机自动启动");
        let s_quit = wide("退出");
        AppendMenuW(hmenu, MF_STRING, MENU_OPEN, s_open.as_ptr());
        AppendMenuW(hmenu, MF_SEPARATOR, 0, std::ptr::null());
        let pause_flag = if ctx.paused.load(Ordering::Relaxed) {
            MF_CHECKED
        } else {
            MF_UNCHECKED
        };
        AppendMenuW(hmenu, MF_STRING | pause_flag, MENU_PAUSE, s_pause.as_ptr());
        let flag = if ctx.autostart.load(Ordering::Relaxed) {
            MF_CHECKED
        } else {
            MF_UNCHECKED
        };
        AppendMenuW(hmenu, MF_STRING | flag, MENU_AUTOSTART, s_auto.as_ptr());
        AppendMenuW(hmenu, MF_SEPARATOR, 0, std::ptr::null());
        AppendMenuW(hmenu, MF_STRING, MENU_QUIT, s_quit.as_ptr());

        let mut pt = POINT { x: 0, y: 0 };
        GetCursorPos(&mut pt);
        SetForegroundWindow(ctx.hwnd); // 不设前台，菜单点外面不会消失
        TrackPopupMenu(hmenu, TPM_RIGHTBUTTON, pt.x, pt.y, 0, ctx.hwnd, std::ptr::null());
        PostMessageW(ctx.hwnd, WM_NULL, 0, 0);
        DestroyMenu(hmenu);
    }

    unsafe fn message_loop() {
        let mut msg: MSG = std::mem::zeroed();
        while GetMessageW(&mut msg, std::ptr::null_mut(), 0, 0) > 0 {
            TranslateMessage(&msg);
            DispatchMessageW(&msg);
        }
    }
}

pub use tray_impl::{start as start_tray, TrayEvent, TrayHandle};

// ================================================================ 原生控件版新增能力

use windows_sys::Win32::Graphics::Gdi::HFONT;
use windows_sys::Win32::UI::WindowsAndMessaging::HICON;

/// 提取 exe 的 HICON（small=true 为 16x16；调用方用完负责 DestroyIcon）。
///
/// 与 `extract_icon_rgba` 的区别：这里是**系统给的现成 HICON**，可直接喂
/// ImageList（原生 ListView 的小图标列），不必自己画 DIB。
pub fn extract_hicon(exe_path: &str, small: bool) -> Option<HICON> {
    use windows_sys::Win32::UI::Shell::{
        SHGetFileInfoW, SHFILEINFOW, SHGFI_ICON, SHGFI_LARGEICON, SHGFI_SMALLICON,
    };
    let w = wide(exe_path);
    let mut info: SHFILEINFOW = unsafe { std::mem::zeroed() };
    let flags = SHGFI_ICON | if small { SHGFI_SMALLICON } else { SHGFI_LARGEICON };
    let ret = unsafe {
        SHGetFileInfoW(
            w.as_ptr(),
            0,
            &mut info,
            std::mem::size_of::<SHFILEINFOW>() as u32,
            flags,
        )
    };
    if ret == 0 || info.hIcon.is_null() {
        None
    } else {
        Some(info.hIcon)
    }
}

/// 当前前台窗口所属进程 pid（监控判据用；无前台窗口时为 None）。
pub fn foreground_pid() -> Option<u32> {
    use windows_sys::Win32::UI::WindowsAndMessaging::GetForegroundWindow;
    unsafe {
        let hwnd = GetForegroundWindow();
        if hwnd.is_null() {
            return None;
        }
        let mut pid = 0u32;
        GetWindowThreadProcessId(hwnd, &mut pid);
        if pid == 0 {
            None
        } else {
            Some(pid)
        }
    }
}

/// 进程级 DPI 感知。**必须在创建任何窗口前调用**。
///
/// Win10 1703+ 用 Per-Monitor V2；旧系统退化 SetProcessDPIAware。
pub fn enable_dpi_awareness() -> bool {
    use windows_sys::Win32::UI::HiDpi::{
        SetProcessDpiAwarenessContext, DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2,
    };
    use windows_sys::Win32::UI::WindowsAndMessaging::SetProcessDPIAware;
    let ok = unsafe { SetProcessDpiAwarenessContext(DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2) };
    if ok == 0 {
        unsafe {
            SetProcessDPIAware();
        }
        false
    } else {
        true
    }
}

/// 窗口所在显示器的 DPI（96 = 100%）。
pub fn dpi_for_window(hwnd: HWND) -> u32 {
    use windows_sys::Win32::UI::HiDpi::GetDpiForWindow;
    let dpi = unsafe { GetDpiForWindow(hwnd) };
    if dpi == 0 {
        96
    } else {
        dpi
    }
}

/// 系统 UI 字体（SPI_GETNONCLIENTMETRICS 的消息字体）。
///
/// 原生控件的标准做法：字体由系统给出（自动适配当前 DPI 与中文区域），
/// 比硬编码「微软雅黑 / 字号」更稳。失败返回空句柄（控件用系统默认字体）。
pub fn system_ui_font(dpi: u32) -> HFONT {
    use windows_sys::Win32::Graphics::Gdi::CreateFontIndirectW;
    use windows_sys::Win32::UI::WindowsAndMessaging::{
        SystemParametersInfoW, NONCLIENTMETRICSW, SPI_GETNONCLIENTMETRICS,
    };
    let mut ncm: NONCLIENTMETRICSW = unsafe { std::mem::zeroed() };
    ncm.cbSize = std::mem::size_of::<NONCLIENTMETRICSW>() as u32;
    let ok = unsafe {
        SystemParametersInfoW(
            SPI_GETNONCLIENTMETRICS,
            ncm.cbSize,
            &mut ncm as *mut _ as *mut std::ffi::c_void,
            0,
        )
    };
    if ok == 0 {
        return std::ptr::null_mut();
    }
    // 显式按窗口 DPI 算 9pt 的像素高度：SPI 返回的 lfHeight 在不同
    // DPI awareness 配置下不一致（本机 200% 屏实测会退化成 96dpi 字号），
    // 这里统一校准，保证非 100% 缩放下界面大小正确。
    let dpi = if dpi == 0 { 96 } else { dpi } as i32;
    ncm.lfMessageFont.lfHeight = -((9 * dpi + 36) / 72);
    unsafe { CreateFontIndirectW(&ncm.lfMessageFont) }
}

// ================================================================ v3.1 对齐：分类 / 时长格式

/// 应用分类规则（对齐 Python 版 `winutil.categorize`，派生值不落库）。
/// 顺序即优先级：整条路径小写后做子串匹配，未命中回退「其他」。
const CATEGORY_RULES: [(&str, &[&str]); 7] = [
    (
        "开发",
        &[
            "code.exe", "code-insiders", "cursor", "windsurf", "trae", "pycharm", "idea64", "clion",
            "goland", "webstorm", "rider64", "devenv.exe", "cmd.exe", "powershell", "pwsh", "wt.exe",
            "windowsterminal", "mintty", "git-bash", "bash.exe", "wsl", "docker", "postman",
            "dbeaver", "navicat", "sublime_text", "notepad++", "vim", "emacs", "xshell", "xftp",
            "putty", "winscp", "hbuilder",
        ],
    ),
    (
        "浏览器",
        &[
            "chrome", "msedge", "firefox", "brave", "opera", "vivaldi", "iexplore", "360se",
            "360chrome", "qqbrowser", "sogouexplorer", "tabbit", "arc.exe", "zen.exe",
        ],
    ),
    (
        "通讯",
        &[
            "wechat", "weixin", "qq.exe", "tim.exe", "dingtalk", "feishu", "lark", "slack",
            "discord", "telegram", "whatsapp", "zoom", "teams", "skype", "mstsc", "anydesk",
            "todesk", "sunlogin",
        ],
    ),
    (
        "媒体",
        &[
            "potplayer", "vlc", "mpv", "kmplayer", "spotify", "cloudmusic", "netease", "foobar",
            "aimp", "audacity", "obs64", "obs.exe", "premiere", "photoshop", "gimp", "inkscape",
            "blender", "ffmpeg", "honeyview", "imageglass", "xnview", "bandicam",
        ],
    ),
    (
        "办公",
        &[
            "winword", "excel", "powerpnt", "outlook", "wps", "et.exe", "wpp", "onenote", "notion",
            "obsidian", "typora", "foxit", "acrobat", "sumatrapdf", "xmind", "mindmaster",
            "evernote", "youdaonote", "wiznote",
        ],
    ),
    (
        "游戏",
        &[
            "steam", "epicgames", "battle.net", "battlenet", "ubisoft", "gog", "wegame", "unity",
            "unreal", "minecraft", "genshin", "yuanshen", "riot",
        ],
    ),
    (
        "系统工具",
        &[
            "explorer.exe", "taskmgr", "control.exe", "regedit", "cleanmgr", "diskmgmt", "devmgmt",
            "services.msc", "mspaint", "calc.exe", "notepad.exe", "snippingtool", "magnify",
            "perfmon", "resmon", "eventvwr", "mmc.exe",
        ],
    ),
];

/// 应用分类（未命中回退「其他」）。
pub fn categorize(exe_path: &str) -> &'static str {
    if exe_path.is_empty() {
        return "其他";
    }
    let hay = exe_path.to_lowercase();
    for (label, keys) in CATEGORY_RULES.iter() {
        for k in keys.iter() {
            if hay.contains(k) {
                return label;
            }
        }
    }
    "其他"
}

/// 累计使用时长的人类可读格式（对齐 Python 版 ui.human_duration）。
pub fn human_duration(seconds: f64) -> String {
    let s = if seconds.is_finite() && seconds > 0.0 {
        seconds
    } else {
        0.0
    };
    if s < 10.0 {
        return "—".to_string();
    }
    if s < 60.0 {
        return format!("{}s", s as i64);
    }
    if s < 3600.0 {
        return format!("{}m", (s / 60.0) as i64);
    }
    format!("{:.1}h", s / 3600.0)
}

/// 唤醒已有实例的主窗口（单实例冲突时用）。
///
/// 为什么不能只弹一句"去托盘里找"：Win11 默认把新托盘图标折叠进
/// 「隐藏的图标」，用户很可能根本找不到，体验上等于"程序打不开"。
pub fn activate_existing_window() -> bool {
    use windows_sys::Win32::UI::WindowsAndMessaging::{
        FindWindowW, FlashWindowEx, IsIconic, SetForegroundWindow, ShowWindow, FLASHWINFO,
        FLASHW_ALL, SW_RESTORE, SW_SHOW,
    };
    let class = wide("ExeTraceNativeWnd");
    unsafe {
        let hwnd = FindWindowW(class.as_ptr(), std::ptr::null());
        if hwnd.is_null() {
            return false;
        }
        if IsIconic(hwnd) != 0 {
            ShowWindow(hwnd, SW_RESTORE);
        }
        ShowWindow(hwnd, SW_SHOW); // 缩托盘时窗口是隐藏的，先显示出来
        SetForegroundWindow(hwnd);
        // SetForegroundWindow 受前台锁定限制时，至少让任务栏闪三下提示
        let mut fwi = FLASHWINFO {
            cbSize: std::mem::size_of::<FLASHWINFO>() as u32,
            hwnd,
            dwFlags: FLASHW_ALL,
            uCount: 3,
            dwTimeout: 0,
        };
        FlashWindowEx(&mut fwi);
        true
    }
}

/// 把文本写入剪贴板（CF_UNICODETEXT）。
pub fn clipboard_set_text(text: &str) -> bool {
    use windows_sys::Win32::System::DataExchange::{
        CloseClipboard, EmptyClipboard, OpenClipboard, SetClipboardData,
    };
    use windows_sys::Win32::System::Memory::{GlobalAlloc, GlobalLock, GlobalUnlock, GMEM_MOVEABLE};
    use windows_sys::Win32::System::Ole::CF_UNICODETEXT;

    let mut buf: Vec<u16> = text.encode_utf16().collect();
    buf.push(0);
    let bytes = buf.len() * 2;

    unsafe {
        if OpenClipboard(std::ptr::null_mut()) == 0 {
            return false;
        }
        let ok = (|| {
            if EmptyClipboard() == 0 {
                return false;
            }
            let hmem = GlobalAlloc(GMEM_MOVEABLE, bytes);
            if hmem.is_null() {
                return false;
            }
            let ptr = GlobalLock(hmem) as *mut u16;
            if ptr.is_null() {
                return false;
            }
            std::ptr::copy_nonoverlapping(buf.as_ptr(), ptr, buf.len());
            GlobalUnlock(hmem);
            if SetClipboardData(CF_UNICODETEXT as u32, hmem as HANDLE).is_null() {
                return false;
            }
            true
        })();
        CloseClipboard();
        ok
    }
}
