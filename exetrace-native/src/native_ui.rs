//! Win32 原生控件界面（**不用任何 UI 框架**）。
//!
//! 布局：顶部一行（搜索框 / 排序下拉 / 操作按钮 / 开机自启）→ ListView → 状态栏。
//! 列表用 `LVS_OWNERDATA` 虚拟模式：几万行也只渲染可见项，无逐行控件开销。
//! 数据流：`Arc<Mutex<Store>>`；WM_TIMER(500ms) 检查 rev，变了才重查。
//!
//! 与 egui 版的区别（这正是内存目标的来源）：
//! - 没有 GPU 渲染栈 / 事件循环框架 / 字体图集 —— 全部由系统公共控件承担
//! - 图标走 ImageList（系统管理），不产生逐帧纹理上传

use std::collections::HashMap;
use std::ffi::c_void;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::Receiver;
use std::sync::{Arc, Mutex};

use windows_sys::Win32::Foundation::{HWND, LPARAM, LRESULT, POINT, RECT, WPARAM};
use windows_sys::Win32::Graphics::Gdi::{
    DeleteObject, InvalidateRect, ScreenToClient, UpdateWindow, COLOR_WINDOW, HGDIOBJ, HFONT,
};
use windows_sys::Win32::System::LibraryLoader::GetModuleHandleW;
use windows_sys::Win32::UI::Controls::*;
use windows_sys::Win32::UI::HiDpi::GetDpiForSystem;
use windows_sys::Win32::UI::Input::KeyboardAndMouse::{
    EnableWindow, GetKeyState, RegisterHotKey, SetFocus, UnregisterHotKey, MOD_ALT, MOD_CONTROL,
    MOD_NOREPEAT, VK_CONTROL, VK_DELETE, VK_ESCAPE, VK_F1, VK_F5, VK_RETURN,
};
use windows_sys::Win32::UI::WindowsAndMessaging::*;

use crate::appicon;
use crate::resolver;
use crate::shortcut;
use crate::store::{self, AppRow, Store};
use crate::winx;

/// 监控线程把「刚刚记录」事件投到这里，UI 轮询显示（main.rs 与 egui 版同款契约）。
pub type LastEvent = Arc<Mutex<Option<(String, bool)>>>;

// ------------------------------------------------------------------ 控件 ID / 定时器

const ID_SEARCH: i32 = 1001;
const ID_SORT: i32 = 1002;
const ID_LIST: i32 = 1003;
const ID_STATUS: i32 = 1004;

const ID_BTN_LAUNCH: i32 = 1101;
const ID_BTN_REVEAL: i32 = 1102;
const ID_BTN_COPY: i32 = 1103;
const ID_BTN_REMOVE: i32 = 1104;
const ID_BTN_PRUNE: i32 = 1105;
const ID_CHK_AUTOSTART: i32 = 1106;
const ID_BTN_PIN: i32 = 1107;
const ID_BTN_PIN_ANY: i32 = 1108;

const ID_MENU_LAUNCH: i32 = 1201;
const ID_MENU_REVEAL: i32 = 1202;
const ID_MENU_COPY: i32 = 1203;
const ID_MENU_REMOVE: i32 = 1204;
const ID_MENU_PIN: i32 = 1205;
const ID_MENU_PRUNE: i32 = 1206;
const ID_MENU_PAUSE: i32 = 1207;
const ID_MENU_HELP: i32 = 1208;

/// 全局热键 ID（Ctrl+Alt+E 呼出主窗口）。
const HOTKEY_ID: i32 = 9001;

const TIMER_POLL: usize = 1;
const TIMER_ICONS: usize = 2;

/// 自定义消息：窗口显示后按真实 DPI 重建字体/尺寸/布局。
/// WM_CREATE 阶段窗口还没关联到显示器，GetDpiForWindow 会返回 96。
const WM_APP_RELAYOUT: u32 = WM_APP + 10;

/// 自定义消息：子类化的搜索框请求「启动当前选中项」（回车）。
const WM_APP_LAUNCH_SELECTED: u32 = WM_APP + 20;

/// 搜索防抖定时器（150ms 内的连续输入只查一次）。
const TIMER_SEARCH: usize = 3;

/// `EM_SETSEL`（文本框全选；windows-sys 未导出该消息常量）。
const EM_SETSEL: u32 = 0x00B1;

/// 帮助文本（F1 / 右键菜单「帮助」）。
const HELP_TEXT: &str = "ExeTrace —— 应用历史定位器\n\n\
【它做什么】\n\
后台记录你打开过的每一个应用，关掉软件界面后随时能在这里找回它。\n\
首次启动会扫描系统里的历史痕迹，所以不用等下一次打开就有记录。\n\n\
【常用操作】\n\
  双击列表项                启动该应用\n\
  右键列表项                启动 / 打开所在文件夹 / 复制路径 / 创建桌面快捷方式 /\n\
                            从历史中移除 / 清理失效记录 / 暂停记录 / 帮助\n\
  底部「创建桌面快捷方式」   把选中项钉到桌面（同名自动编号，绝不覆盖）\n\
  底部「钉任意程序…」       选 exe 或软件文件夹 → 自动识别主程序 → 钉到桌面\n\n\
【快捷键】\n\
  回车        启动选中项（在搜索框里按也一样）\n\
  Delete      从历史中移除选中项\n\
  Ctrl+C      复制选中项路径\n\
  Ctrl+F      聚焦搜索框\n\
  Esc         清空搜索（焦点在搜索框时）\n\
  F5          刷新列表\n\
  F1          打开本帮助\n\
  Ctrl+Alt+E  全局呼出此窗口（任何程序里都能用）\n\n\
【托盘】\n\
  关闭窗口 = 缩到托盘继续记录；右键托盘图标可退出 / 暂停记录 / 开关自启。\n\
  Win11 默认把托盘图标折叠进「隐藏的图标」，可从那里拖出来固定显示。\n\n\
【配置】%LOCALAPPDATA%\\ExeTrace\\config.json\n\
  ignore      忽略列表：不想被记录的 exe 文件名（如 \"spam.exe\"）\n\
  categories  分类覆盖：给某个 exe 指定分类（如 {\"code.exe\": \"开发\"}）\n\
  改完自动生效，无需重启。";

/// `EM_SETCUEBANNER`（编辑框水印；windows-sys 未导出该消息常量）。
const EM_SETCUEBANNER: u32 = 0x1501;

const SORTS: [(&str, &str); 4] = [
    ("智能排序", "smart"),
    ("最近打开", "recent"),
    ("打开次数", "count"),
    ("名称", "name"),
];

/// 列定义：标题 / 初始宽度（96dpi）/ 对齐（对齐 Python v3.1 的 7 列）。
const COLS: [(&str, i32, i32); 7] = [
    ("应用", 250, LVCFMT_LEFT),
    ("位置", 360, LVCFMT_LEFT),
    ("分类", 80, LVCFMT_LEFT),
    ("次数", 56, LVCFMT_RIGHT),
    ("使用时长", 80, LVCFMT_RIGHT),
    ("最后打开", 120, LVCFMT_LEFT),
    ("桌面", 50, LVCFMT_LEFT),
];

/// 顶部按钮：控件 ID / 文案 / 宽度（96dpi）。
/// 「移除记录 / 清理失效记录」移到右键菜单，给 v3.1 的钉桌面能力腾位置。
const BUTTONS: [(i32, &str, f32); 5] = [
    (ID_BTN_LAUNCH, "启动", 70.0),
    (ID_BTN_REVEAL, "打开所在文件夹", 140.0),
    (ID_BTN_COPY, "复制路径", 96.0),
    (ID_BTN_PIN, "创建桌面快捷方式", 150.0),
    (ID_BTN_PIN_ANY, "钉任意程序…", 120.0),
];

// ------------------------------------------------------------------ 状态

struct Ui {
    store: Arc<Mutex<Store>>,
    last_event: LastEvent,
    hwnd: HWND,
    list: HWND,
    search: HWND,
    sort_box: HWND,
    status: HWND,
    autostart_box: HWND,
    buttons: Vec<HWND>,
    font: HFONT,
    rows: Vec<AppRow>,
    /// 每行每列的 UTF-16（NUL 结尾）：ListView 是 Unicode 控件，
    /// LVN_GETDISPINFO 的 pszText 必须是宽字符指针，且消息返回后仍有效。
    cells: Vec<Vec<Vec<u16>>>,
    rev: u64,
    sort_idx: usize,
    himl: HIMAGELIST,
    icon_index: HashMap<String, i32>,
    icon_cursor: usize,
    icon_done: bool,
    /// 待加载图标的行号队列（由 `LVN_ODCACHEHINT` 驱动：只加载将可见的行，
    /// 避免 300+ 个图标全量进 ImageList 造成几十 MB 内存）。
    icon_queue: Vec<usize>,
    icon_queued: std::collections::HashSet<usize>,
    /// 高 DPI 下图标用 32x32（SHGFI_LARGEICON），否则 16x16。
    icon_large: bool,
    tray: Option<winx::TrayHandle>,
    tray_rx: Option<Receiver<winx::TrayEvent>>,
    autostart: Arc<AtomicBool>,
    paused: Arc<AtomicBool>,
    hotkey_ok: bool,
    /// 启动时刻 / 是否已做过启动后的工作集回收（一次性）。
    started_at: f64,
    trimmed_once: bool,
    quitting: bool,
    event_text: String,
    event_at: f64,
    total: i64,
    today: i64,
}

// ------------------------------------------------------------------ 入口

pub fn run(
    store: Arc<Mutex<Store>>,
    last_event: LastEvent,
    minimized: bool,
    tray_enabled: bool,
    paused: Arc<AtomicBool>,
) -> Result<(), String> {
    unsafe {
        // 托盘 icon 与窗口都要正确的 DPI 行为，必须在建窗前设置。
        let pmv2 = winx::enable_dpi_awareness();

        let icc = INITCOMMONCONTROLSEX {
            dwSize: std::mem::size_of::<INITCOMMONCONTROLSEX>() as u32,
            dwICC: ICC_LISTVIEW_CLASSES | ICC_BAR_CLASSES,
        };
        InitCommonControlsEx(&icc);

        let (tx, rx) = std::sync::mpsc::channel();
        let autostart = Arc::new(AtomicBool::new(winx::is_autostart_enabled()));
        let tray = if tray_enabled {
            winx::start_tray(
                "ExeTrace — 应用历史定位器",
                appicon::make_icon_rgba(32),
                autostart.clone(),
                paused.clone(),
                tx,
            )
        } else {
            None
        };

        let state = Box::new(Ui {
            store,
            last_event,
            hwnd: std::ptr::null_mut(),
            list: std::ptr::null_mut(),
            search: std::ptr::null_mut(),
            sort_box: std::ptr::null_mut(),
            status: std::ptr::null_mut(),
            autostart_box: std::ptr::null_mut(),
            buttons: Vec::new(),
            font: std::ptr::null_mut(),
            rows: Vec::new(),
            cells: Vec::new(),
            rev: 0,
            sort_idx: 0,
            himl: 0,
            icon_index: HashMap::new(),
            icon_cursor: 0,
            icon_done: false,
            icon_queue: Vec::new(),
            icon_queued: std::collections::HashSet::new(),
            icon_large: false,
            tray,
            tray_rx: Some(rx),
            autostart,
            paused,
            hotkey_ok: false,
            started_at: crate::store::unix_now(),
            trimmed_once: false,
            quitting: false,
            event_text: String::new(),
            event_at: 0.0,
            total: 0,
            today: 0,
        });
        let state_ptr = Box::into_raw(state);

        let hinst = GetModuleHandleW(std::ptr::null());
        let class_name = winx::wide("ExeTraceNativeWnd");
        let wc = WNDCLASSEXW {
            cbSize: std::mem::size_of::<WNDCLASSEXW>() as u32,
            style: CS_HREDRAW | CS_VREDRAW,
            lpfnWndProc: Some(wndproc),
            hInstance: hinst,
            hCursor: LoadCursorW(std::ptr::null_mut(), IDC_ARROW),
            hbrBackground: (COLOR_WINDOW + 1) as usize as *mut c_void,
            lpszClassName: class_name.as_ptr(),
            ..std::mem::zeroed()
        };
        if RegisterClassExW(&wc) == 0 {
            drop(Box::from_raw(state_ptr));
            return Err(format!("RegisterClassExW 失败（err={}）", windows_sys::Win32::Foundation::GetLastError()));
        }

        let title = winx::wide(crate::paths::APP_TITLE);
        // 创建窗口前拿不到窗口 DPI，用系统 DPI 预估初始尺寸（按物理像素）。
        let s0 = {
            let d = GetDpiForSystem();
            if d == 0 {
                1.0f32
            } else {
                d as f32 / 96.0
            }
        };
        let hwnd = CreateWindowExW(
            0,
            class_name.as_ptr(),
            title.as_ptr(),
            WS_OVERLAPPEDWINDOW | WS_CLIPCHILDREN,
            CW_USEDEFAULT,
            CW_USEDEFAULT,
            (1120.0 * s0) as i32,
            (740.0 * s0) as i32,
            std::ptr::null_mut(),
            std::ptr::null_mut(),
            hinst,
            state_ptr as *const c_void,
        );
        if hwnd.is_null() {
            drop(Box::from_raw(state_ptr));
            return Err("CreateWindowExW 失败".into());
        }

        // CreateWindowExW 的尺寸按物理像素算；Per-Monitor V2 下必须乘 DPI 比例，
        // 否则 200% 缩放的显示器上窗口只有预期逻辑尺寸的一半（还会被 MinTrackSize 兜住）。
        let s = winx::dpi_for_window(hwnd) as f32 / 96.0;
        let mut rc_before: RECT = std::mem::zeroed();
        GetWindowRect(hwnd, &mut rc_before);
        let mut sp_ok = 1;
        if (s - 1.0).abs() > 0.01 {
            sp_ok = SetWindowPos(
                hwnd,
                std::ptr::null_mut(),
                0,
                0,
                (1120.0 * s) as i32,
                (740.0 * s) as i32,
                SWP_NOMOVE | SWP_NOZORDER,
            );
        }
        let mut rc_after: RECT = std::mem::zeroed();
        GetWindowRect(hwnd, &mut rc_after);
        log::info!(
            "DPI 调整: s={:.2} setpos_ok={} before={}x{} after={}x{}",
            s,
            sp_ok,
            rc_before.right - rc_before.left,
            rc_before.bottom - rc_before.top,
            rc_after.right - rc_after.left,
            rc_after.bottom - rc_after.top
        );

        if minimized {
            ShowWindow(hwnd, SW_HIDE);
        } else {
            ShowWindow(hwnd, SW_SHOW);
            UpdateWindow(hwnd);
        }
        log::info!(
            "主窗口就绪: hwnd={:?} visible={} title_len={} pmv2={} dpi_system={} dpi_window={} scale={:.2}",
            hwnd,
            IsWindowVisible(hwnd),
            GetWindowTextLengthW(hwnd),
            pmv2,
            GetDpiForSystem(),
            winx::dpi_for_window(hwnd),
            s
        );

        // 全局热键 Ctrl+Alt+E：呼出主窗口并聚焦搜索框（被占用时静默降级）
        let hotkey_ok = RegisterHotKey(
            hwnd,
            HOTKEY_ID,
            MOD_CONTROL | MOD_ALT | MOD_NOREPEAT,
            'E' as u32,
        ) != 0;
        {
            let ui_ptr = ctx(hwnd);
            if !ui_ptr.is_null() {
                (*ui_ptr).hotkey_ok = hotkey_ok;
            }
        }
        log::info!("全局热键 Ctrl+Alt+E 注册: {hotkey_ok}");

        // 窗口显示后按真实 DPI 校正（WM_CREATE 阶段的 DPI 不可靠）
        if !minimized {
            PostMessageW(hwnd, WM_APP_RELAYOUT, 0, 0);
        }

        let mut msg: MSG = std::mem::zeroed();
        while GetMessageW(&mut msg, std::ptr::null_mut(), 0, 0) > 0 {
            TranslateMessage(&msg);
            DispatchMessageW(&msg);
        }
        Ok(())
    }
}

unsafe fn ctx(hwnd: HWND) -> *mut Ui {
    GetWindowLongPtrW(hwnd, GWLP_USERDATA) as *mut Ui
}

/// 搜索框子类化前的原窗口过程（未处理的按键必须转交回去）。
static EDIT_OLD_PROC: std::sync::atomic::AtomicIsize = std::sync::atomic::AtomicIsize::new(0);

/// 搜索框子类化过程：Esc 清空、回车 = 启动选中项。
unsafe extern "system" fn search_edit_proc(
    hwnd: HWND,
    msg: u32,
    wparam: WPARAM,
    lparam: LPARAM,
) -> LRESULT {
    if msg == WM_KEYDOWN {
        let vk = wparam as u16;
        let parent = GetParent(hwnd);
        match vk {
            VK_ESCAPE => {
                let empty = winx::wide("");
                SetWindowTextW(hwnd, empty.as_ptr());
                return 0; // 清空后 EN_CHANGE 会走防抖刷新
            }
            VK_RETURN => {
                PostMessageW(parent, WM_APP_LAUNCH_SELECTED, 0, 0);
                return 0;
            }
            // 这些键在文本框里没有意义 → 转交主窗口处理（帮助 / 刷新）
            VK_F1 | VK_F5 => {
                SendMessageW(parent, WM_KEYDOWN, wparam, lparam);
                return 0;
            }
            _ => {
                let ctrl = GetKeyState(VK_CONTROL as i32) < 0;
                if ctrl && vk == b'F' as u16 {
                    SendMessageW(parent, WM_KEYDOWN, wparam, lparam);
                    return 0;
                }
                // Ctrl+C 与 Delete 归文本框自己（复制选中文本 / 删字）
            }
        }
    }
    let old: unsafe extern "system" fn(HWND, u32, WPARAM, LPARAM) -> LRESULT =
        std::mem::transmute(EDIT_OLD_PROC.load(Ordering::Relaxed));
    CallWindowProcW(Some(old), hwnd, msg, wparam, lparam)
}

/// 统一快捷键分发（主窗口 / 列表 / 搜索框三个入口都汇到这里）。
/// 返回 true 表示已处理（吞掉该键）。
unsafe fn handle_shortcut(ui: &mut Ui, vk: u16) -> bool {
    let ctrl = GetKeyState(VK_CONTROL as i32) < 0;
    match vk {
        VK_F1 => {
            show_help(ui.hwnd);
            true
        }
        VK_F5 => {
            refresh(ui, false);
            set_event(ui, "已刷新");
            true
        }
        VK_RETURN => {
            let target = selected_path(ui).or_else(|| ui.rows.first().map(|r| r.path.clone()));
            if let Some(p) = target {
                act_launch(ui, &p);
            }
            true
        }
        VK_DELETE => {
            if let Some(p) = selected_path(ui) {
                if let Ok(mut st) = ui.store.lock() {
                    let _ = st.remove(&p);
                }
                set_event(ui, "已从历史中移除");
                refresh(ui, false);
            }
            true
        }
        _ if ctrl && vk == b'C' as u16 => {
            let target = selected_path(ui).or_else(|| ui.rows.first().map(|r| r.path.clone()));
            if let Some(p) = target {
                if winx::clipboard_set_text(&p) {
                    set_event(ui, "完整路径已复制到剪贴板");
                }
            }
            true
        }
        _ if ctrl && vk == b'F' as u16 => {
            SetFocus(ui.search);
            SendMessageW(ui.search, EM_SETSEL, 0, -1); // 全选：输入即替换
            true
        }
        _ => false,
    }
}

unsafe fn show_help(parent: HWND) {
    log::info!("打开帮助面板");
    winx::message_box_on(parent, HELP_TEXT);
}

/// 把空闲工作集还给系统（窗口隐藏/最小化后调用；再显示时会按需换回）。
/// 不做这一步，任务管理器会长期显示"窗口期"的几十 MB 占用。
unsafe fn trim_working_set() {
    use windows_sys::Win32::System::Threading::{GetCurrentProcess, SetProcessWorkingSetSize};
    let ok = SetProcessWorkingSetSize(GetCurrentProcess(), usize::MAX, usize::MAX);
    log::info!("工作集回收: ok={}", ok);
}

// ------------------------------------------------------------------ 窗口过程

unsafe extern "system" fn wndproc(hwnd: HWND, msg: u32, wparam: WPARAM, lparam: LPARAM) -> LRESULT {
    match msg {
        WM_NCCREATE => {
            let cs = lparam as *const CREATESTRUCTW;
            if !cs.is_null() {
                SetWindowLongPtrW(hwnd, GWLP_USERDATA, (*cs).lpCreateParams as isize);
            }
            // 必须交给 DefWindowProcW：窗口标题（CREATESTRUCT.lpszName）在这一步
            // 才被系统写进窗口（自己 return 1 的话标题会一直是空的）。
            DefWindowProcW(hwnd, msg, wparam, lparam)
        }
        WM_CREATE => {
            let ui = ctx(hwnd);
            if ui.is_null() {
                return -1;
            }
            (*ui).hwnd = hwnd;
            let hinst = GetModuleHandleW(std::ptr::null());
            create_children(&mut *ui, hinst);
            layout(&mut *ui);
            refresh(&mut *ui, true);
            SetTimer(hwnd, TIMER_POLL, 500, None);
            SetTimer(hwnd, TIMER_ICONS, 120, None);
            0
        }
        WM_SIZE => {
            let ui = ctx(hwnd);
            if !ui.is_null() {
                layout(&mut *ui);
            }
            if wparam == SIZE_MINIMIZED as WPARAM {
                trim_working_set();
            }
            0
        }
        WM_GETMINMAXINFO => {
            // 先让默认处理填满 MINMAXINFO：自己 zeroed 的话 ptMaxTrackSize 等为 0，
            // 会让之后的 SetWindowPos 放大窗口被拒绝（窗口卡在初始尺寸）。
            let r = DefWindowProcW(hwnd, msg, wparam, lparam);
            let mmi = lparam as *mut MINMAXINFO;
            if !mmi.is_null() {
                let s = winx::dpi_for_window(hwnd) as f32 / 96.0;
                (*mmi).ptMinTrackSize.x = (820.0 * s) as i32;
                (*mmi).ptMinTrackSize.y = (480.0 * s) as i32;
            }
            r
        }
        WM_APP_RELAYOUT => {
            let ui = ctx(hwnd);
            if !ui.is_null() {
                relayout_for_dpi(&mut *ui);
            }
            0
        }
        WM_KEYDOWN => {
            let ui = ctx(hwnd);
            if ui.is_null() {
                return DefWindowProcW(hwnd, msg, wparam, lparam);
            }
            if handle_shortcut(&mut *ui, wparam as u16) {
                return 0;
            }
            DefWindowProcW(hwnd, msg, wparam, lparam)
        }
        WM_APP_LAUNCH_SELECTED => {
            let ui = ctx(hwnd);
            if !ui.is_null() {
                let ui = &mut *ui;
                let target = selected_path(ui).or_else(|| ui.rows.first().map(|r| r.path.clone()));
                if let Some(p) = target {
                    act_launch(ui, &p);
                }
            }
            0
        }
        WM_HOTKEY => {
            if wparam as i32 == HOTKEY_ID {
                let ui = ctx(hwnd);
                if !ui.is_null() {
                    ShowWindow(hwnd, SW_SHOW);
                    ShowWindow(hwnd, SW_RESTORE);
                    SetForegroundWindow(hwnd);
                    SetFocus((*ui).search); // 直接聚焦搜索框，呼出即用
                }
            }
            0
        }
        WM_TIMER => {
            let ui = ctx(hwnd);
            if ui.is_null() {
                return 0;
            }
            if wparam == TIMER_ICONS {
                load_icons_step(&mut *ui, 24);
            } else if wparam == TIMER_POLL {
                tick(&mut *ui);
            } else if wparam == TIMER_SEARCH {
                // 防抖到期：真正执行搜索
                KillTimer(hwnd, TIMER_SEARCH);
                refresh(&mut *ui, false);
            }
            0
        }
        WM_NOTIFY => {
            let ui = ctx(hwnd);
            if ui.is_null() {
                return 0;
            }
            let ui = &mut *ui;
            let hdr = lparam as *const NMHDR;
            if hdr.is_null() || (*hdr).hwndFrom != ui.list {
                return 0;
            }
            match (*hdr).code {
                LVN_GETDISPINFOW => {
                    fill_dispinfo(ui, &mut *(lparam as *mut NMLVDISPINFOW));
                }
                NM_DBLCLK => {
                    // 双击启动。这里同样要补一次命中+选中：owner-data 虚拟列表既不自己
                    // 产生选中，双击时第一击的 NM_CLICK 通知也晚于 NM_DBLCLK 到达，
                    // 不补的话 selected_path 是 None → 双击没反应。
                    let idx = hit_row_at_cursor(ui);
                    if idx >= 0 {
                        set_item_selected(ui.list, idx);
                        update_selection_ui(ui);
                    }
                    if let Some(p) = selected_path(ui) {
                        act_launch(ui, &p);
                    }
                }
                NM_RCLICK => {
                    // 右键也要自己处理：ListView 不会发 WM_CONTEXTMENU，
                    // 也不会替我们选中光标下的行（不补的话"未选中时右键"会被菜单守卫挡掉）。
                    let idx = hit_row_at_cursor(ui);
                    if idx >= 0 {
                        set_item_selected(ui.list, idx);
                        update_selection_ui(ui);
                    }
                    show_context_menu(ui);
                    return 0;
                }
                NM_CLICK => {
                    // 左键点击：owner-data 虚拟列表**不会自己产生选中**——实测点击能到达
                    // （NM_CLICK 会发），但系统既不改选中、也不发 ITEMCHANGED，表现为
                    // "鼠标点行没反应"（键盘 ↓ 却是好的）。所以这里补一次命中 + 选中。
                    let idx = hit_row_at_cursor(ui);
                    if idx >= 0 {
                        set_item_selected(ui.list, idx);
                        update_selection_ui(ui);
                    }
                }
                LVN_ITEMCHANGED => {
                    update_selection_ui(ui);
                }
                LVN_ODSTATECHANGED => {
                    update_selection_ui(ui);
                }
                LVN_KEYDOWN => {
                    // 列表控件里的按键（回车 / Delete / Ctrl+C 等）从这里来
                    let kd = lparam as *const NMLVKEYDOWN;
                    if !kd.is_null() {
                        handle_shortcut(ui, (*kd).wVKey);
                    }
                }
                LVN_ODCACHEHINT => {
                    // owner-data 列表告知「即将显示的范围」——只加载这些行的图标
                    let hint = lparam as *const NMLVCACHEHINT;
                    if !hint.is_null() {
                        let from = (*hint).iFrom.max(0) as usize;
                        let to = (*hint).iTo.max(0) as usize;
                        queue_icons(ui, from, to);
                    }
                }
                _ => {}
            }
            0
        }
        WM_COMMAND => {
            let ui = ctx(hwnd);
            if ui.is_null() {
                return 0;
            }
            on_command(&mut *ui, wparam);
            0
        }
        WM_CLOSE => {
            let ui = ctx(hwnd);
            if !ui.is_null() {
                let ui = &mut *ui;
                // 有托盘：关闭 = 隐藏（仍在后台记录）；退出走托盘菜单。
                if ui.tray.is_some() && !ui.quitting {
                    ShowWindow(hwnd, SW_HIDE);
                    set_event(ui, "已最小化到托盘，仍在后台记录；右键托盘图标可退出");
                    trim_working_set();
                    return 0;
                }
            }
            DestroyWindow(hwnd);
            0
        }
        WM_DESTROY => {
            PostQuitMessage(0);
            0
        }
        WM_NCDESTROY => {
            let ptr = ctx(hwnd);
            if !ptr.is_null() {
                SetWindowLongPtrW(hwnd, GWLP_USERDATA, 0);
                let ui = Box::from_raw(ptr);
                if let Some(t) = &ui.tray {
                    t.stop();
                }
                if ui.hotkey_ok {
                    UnregisterHotKey(hwnd, HOTKEY_ID);
                }
                if !ui.font.is_null() {
                    DeleteObject(ui.font as HGDIOBJ);
                }
                if ui.himl != 0 {
                    ImageList_Destroy(ui.himl);
                }
                drop(ui);
            }
            DefWindowProcW(hwnd, msg, wparam, lparam)
        }
        _ => DefWindowProcW(hwnd, msg, wparam, lparam),
    }
}

// ------------------------------------------------------------------ 子控件

unsafe fn create_children(ui: &mut Ui, hinst: windows_sys::Win32::Foundation::HINSTANCE) {
    let font: HFONT = winx::system_ui_font(winx::dpi_for_window(ui.hwnd));
    ui.font = font;
    let set_font = |h: HWND| {
        if !font.is_null() {
            SendMessageW(h, WM_SETFONT, font as usize, 1);
        }
    };
    let empty = winx::wide("");

    // 搜索框（带水印）
    ui.search = CreateWindowExW(
        WS_EX_CLIENTEDGE,
        WC_EDIT,
        empty.as_ptr(),
        WS_CHILD | WS_VISIBLE | WS_TABSTOP | ES_AUTOHSCROLL as u32,
        0,
        0,
        0,
        0,
        ui.hwnd,
        ID_SEARCH as isize as HMENU,
        hinst,
        std::ptr::null_mut(),
    );
    set_font(ui.search);
    let hint = winx::wide("输入应用名或路径过滤…");
    SendMessageW(ui.search, EM_SETCUEBANNER, 1, hint.as_ptr() as LPARAM);
    // 子类化：Esc 清空搜索、回车启动选中项（Edit 不会把这两个键转发给父窗口）
    let old = SetWindowLongPtrW(ui.search, GWLP_WNDPROC, search_edit_proc as usize as isize);
    EDIT_OLD_PROC.store(old, Ordering::Relaxed);

    // 排序下拉
    ui.sort_box = CreateWindowExW(
        0,
        WC_COMBOBOX,
        empty.as_ptr(),
        WS_CHILD | WS_VISIBLE | WS_TABSTOP | WS_VSCROLL | CBS_DROPDOWNLIST as u32,
        0,
        0,
        0,
        0,
        ui.hwnd,
        ID_SORT as isize as HMENU,
        hinst,
        std::ptr::null_mut(),
    );
    set_font(ui.sort_box);
    for (label, _) in SORTS {
        let l = winx::wide(label);
        SendMessageW(ui.sort_box, CB_ADDSTRING, 0, l.as_ptr() as LPARAM);
    }
    SendMessageW(ui.sort_box, CB_SETCURSEL, 0, 0);

    // ListView（虚拟列表）
    ui.list = CreateWindowExW(
        WS_EX_CLIENTEDGE,
        WC_LISTVIEWW,
        empty.as_ptr(),
        WS_CHILD | WS_VISIBLE | WS_TABSTOP | LVS_REPORT | LVS_SINGLESEL | LVS_SHOWSELALWAYS | LVS_OWNERDATA,
        0,
        0,
        0,
        0,
        ui.hwnd,
        ID_LIST as isize as HMENU,
        hinst,
        std::ptr::null_mut(),
    );
    set_font(ui.list);
    let ex_style = LVS_EX_FULLROWSELECT | LVS_EX_DOUBLEBUFFER;
    SendMessageW(
        ui.list,
        LVM_SETEXTENDEDLISTVIEWSTYLE,
        ex_style as WPARAM,
        ex_style as LPARAM,
    );
    for (i, (name, width, align)) in COLS.iter().enumerate() {
        let mut col: LVCOLUMNW = std::mem::zeroed();
        col.mask = LVCF_TEXT | LVCF_WIDTH | LVCF_SUBITEM | LVCF_FMT;
        col.fmt = *align;
        col.cx = *width;
        let t = winx::wide(name);
        col.pszText = t.as_ptr() as *mut u16;
        col.iSubItem = i as i32;
        SendMessageW(ui.list, LVM_INSERTCOLUMNW, i, &col as *const _ as LPARAM);
    }
    ui.himl = ImageList_Create(16, 16, ILC_COLOR32, 64, 128);
    if ui.himl != 0 {
        SendMessageW(ui.list, LVM_SETIMAGELIST, LVSIL_SMALL as WPARAM, ui.himl as LPARAM);
    }

    // 操作按钮
    for (id, label, _) in BUTTONS {
        let l = winx::wide(label);
        let h = CreateWindowExW(
            0,
            WC_BUTTON,
            l.as_ptr(),
            WS_CHILD | WS_VISIBLE | WS_TABSTOP | BS_PUSHBUTTON as u32,
            0,
            0,
            0,
            0,
            ui.hwnd,
            id as isize as HMENU,
            hinst,
            std::ptr::null_mut(),
        );
        set_font(h);
        EnableWindow(h, 0);
        ui.buttons.push(h);
    }

    // 开机自启
    let l = winx::wide("开机自动启动");
    ui.autostart_box = CreateWindowExW(
        0,
        WC_BUTTON,
        l.as_ptr(),
        WS_CHILD | WS_VISIBLE | WS_TABSTOP | BS_AUTOCHECKBOX as u32,
        0,
        0,
        0,
        0,
        ui.hwnd,
        ID_CHK_AUTOSTART as isize as HMENU,
        hinst,
        std::ptr::null_mut(),
    );
    set_font(ui.autostart_box);
    if ui.autostart.load(Ordering::Relaxed) {
        SendMessageW(ui.autostart_box, BM_SETCHECK, BST_CHECKED as WPARAM, 0);
    }

    // 状态栏
    ui.status = CreateWindowExW(
        0,
        STATUSCLASSNAMEW,
        empty.as_ptr(),
        WS_CHILD | WS_VISIBLE,
        0,
        0,
        0,
        0,
        ui.hwnd,
        ID_STATUS as isize as HMENU,
        hinst,
        std::ptr::null_mut(),
    );
    set_font(ui.status);

    // 按钮默认禁用（未选中行）；清理失效始终可用
    update_selection_ui(ui);
}

// ------------------------------------------------------------------ 布局

unsafe fn layout(ui: &mut Ui) {
    let mut rc: RECT = std::mem::zeroed();
    GetClientRect(ui.hwnd, &mut rc);
    let w = rc.right - rc.left;
    let h = rc.bottom - rc.top;
    let s = winx::dpi_for_window(ui.hwnd) as f32 / 96.0;
    let pad = (8.0 * s) as i32;
    let gap = (6.0 * s) as i32;
    let row_h = (26.0 * s) as i32;

    // 状态栏自己贴底，先让它定好高度
    SendMessageW(ui.status, WM_SIZE, 0, 0);
    let mut sr: RECT = std::mem::zeroed();
    GetWindowRect(ui.status, &mut sr);
    let sb_h = sr.bottom - sr.top;
    let mid = (w - (350.0 * s) as i32).max(w / 2);
    let parts = [mid, -1i32];
    SendMessageW(ui.status, SB_SETPARTS, 2, parts.as_ptr() as LPARAM);

    let top = pad;
    // 搜索框 + 排序
    let search_w = (320.0 * s) as i32;
    MoveWindow(ui.search, pad, top, search_w, row_h, 1);
    let sort_w = (120.0 * s) as i32;
    MoveWindow(ui.sort_box, pad + search_w + gap, top, sort_w, row_h * 6, 1);

    // 按钮右对齐（先放最右的自启，再倒序放按钮）
    let mut x = w - pad;
    let chk_w = (130.0 * s) as i32;
    x -= chk_w;
    MoveWindow(ui.autostart_box, x, top, chk_w, row_h, 1);
    x -= gap * 3;
    for idx in (0..BUTTONS.len()).rev() {
        let (_, _, bw) = BUTTONS[idx];
        let bw = (bw * s) as i32;
        x -= bw;
        if let Some(hb) = ui.buttons.get(idx) {
            MoveWindow(*hb, x, top, bw, row_h, 1);
        }
        x -= gap;
    }

    // 列表填满中间
    let list_top = top + row_h + pad;
    let list_h = (h - list_top - sb_h - pad).max(60);
    MoveWindow(ui.list, pad, list_top, (w - pad * 2).max(60), list_h, 1);
}

/// 窗口显示后按真实 DPI 校正：重建字体、窗口尺寸、图标图集，然后重新布局。
///
/// 必要性：`WM_CREATE` 阶段窗口尚未关联到显示器，`GetDpiForWindow` 返回 96，
/// 子控件会按 100% 尺寸创建；窗口显示拿到真实 DPI（如 200%）后必须重做一遍，
/// 否则在高 DPI 屏上界面会偏小、图标发糊。
unsafe fn relayout_for_dpi(ui: &mut Ui) {
    let dpi = winx::dpi_for_window(ui.hwnd);
    let s = dpi as f32 / 96.0;

    // 1) 字体按真实 DPI 重建
    let new_font = winx::system_ui_font(dpi);
    if !new_font.is_null() {
        if !ui.font.is_null() {
            DeleteObject(ui.font as HGDIOBJ);
        }
        ui.font = new_font;
        let mut ctrls = vec![ui.search, ui.sort_box, ui.list, ui.status, ui.autostart_box];
        ctrls.extend(ui.buttons.iter().copied());
        for h in ctrls {
            SendMessageW(h, WM_SETFONT, new_font as usize, 1);
        }
    }

    // 2) 窗口尺寸校正（只在明显不符时动，避免覆盖用户手动调整过的尺寸）
    let mut rc: RECT = std::mem::zeroed();
    GetWindowRect(ui.hwnd, &mut rc);
    let cur_w = rc.right - rc.left;
    let want_w = (1120.0 * s) as i32;
    if (cur_w - want_w).abs() > 60 {
        SetWindowPos(
            ui.hwnd,
            std::ptr::null_mut(),
            0,
            0,
            want_w,
            (740.0 * s) as i32,
            SWP_NOMOVE | SWP_NOZORDER,
        );
    }

    // 3) 图标图集按 DPI 重建（16x16 在 200% 下会糊，用 32x32）
    let icon_px = (16.0 * s).round() as i32;
    ui.icon_large = icon_px > 20;
    let px = if ui.icon_large { 32 } else { 16 };
    if ui.himl != 0 {
        ImageList_Destroy(ui.himl);
    }
    ui.himl = ImageList_Create(px, px, ILC_COLOR32, 64, 128);
    if ui.himl != 0 {
        SendMessageW(ui.list, LVM_SETIMAGELIST, LVSIL_SMALL as WPARAM, ui.himl as LPARAM);
    }
    ui.icon_index.clear();

    // 4) 重新布局 + 触发图标重载
    layout(ui);
    refresh(ui, true);
}

// ------------------------------------------------------------------ 数据刷新

unsafe fn refresh(ui: &mut Ui, reset_icons: bool) {
    let keyword = get_window_text(ui.search);
    let sort = SORTS[ui.sort_idx.min(SORTS.len() - 1)].1;
    let (rows, total, today, rev) = match ui.store.lock() {
        Ok(st) => (
            st.query(&keyword, sort, 2000).unwrap_or_default(),
            st.total(),
            st.count_today(),
            st.rev(),
        ),
        Err(_) => return,
    };

    let sel = selected_path(ui);
    ui.rows = rows;
    ui.total = total;
    ui.today = today;
    ui.rev = rev;

    // 重建单元格文本缓冲（指针要在 LVN_GETDISPINFO 返回后仍有效 → 按行持有）
    ui.cells = ui
        .rows
        .iter()
        .map(|r| {
            let count = if r.launch_count > 0 {
                r.launch_count.to_string()
            } else {
                "—".to_string()
            };
            vec![
                winx::wide(&r.name),
                winx::wide(&r.path),
                winx::wide(
                    &crate::config::category_override(&r.path)
                        .unwrap_or_else(|| winx::categorize(&r.path).to_string()),
                ),
                winx::wide(&count),
                winx::wide(&winx::human_duration(r.total_seconds)),
                winx::wide(&human_time(r.last_seen)),
                winx::wide(if r.pinned { "✔" } else { "" }),
            ]
        })
        .collect();

    // LVSICF_NOINVALIDATEALL(1) | LVSICF_NOSCROLL(2)
    SendMessageW(ui.list, LVM_SETITEMCOUNT, ui.rows.len(), 3);
    InvalidateRect(ui.list, std::ptr::null(), 0);

    if let Some(p) = sel {
        if let Some(i) = ui.rows.iter().position(|r| r.path == p) {
            set_item_selected(ui.list, i as i32);
            SendMessageW(ui.list, LVM_ENSUREVISIBLE, i, 0);
        }
    }
    if reset_icons {
        // 首屏先行；ListView 随后会发 LVN_ODCACHEHINT 精确告知可见范围
        ui.icon_queue.clear();
        ui.icon_queued.clear();
        if !ui.rows.is_empty() {
            queue_icons(ui, 0, 40.min(ui.rows.len() - 1));
        }
    }
    update_selection_ui(ui);
    update_status(ui);
}

unsafe fn tick(ui: &mut Ui) {
    // 1) 数据版本变化才重查
    let rev = ui.store.lock().map(|s| s.rev()).unwrap_or(ui.rev);
    if rev != ui.rev {
        refresh(ui, false);
    }

    // 2) 托盘事件：先取走本轮到的事件（结束对 ui.tray_rx 的借用，再动 ui 的其他字段）
    let mut events: Vec<winx::TrayEvent> = Vec::new();
    if let Some(rx) = &ui.tray_rx {
        while let Ok(ev) = rx.try_recv() {
            events.push(ev);
        }
    }
    let mut quit = false;
    for ev in events {
        match ev {
            winx::TrayEvent::Show => {
                ShowWindow(ui.hwnd, SW_SHOW);
                ShowWindow(ui.hwnd, SW_RESTORE);
                SetForegroundWindow(ui.hwnd);
            }
            winx::TrayEvent::ToggleAutostart => {
                let cur = ui.autostart.load(Ordering::Relaxed);
                let now = winx::set_autostart(!cur);
                ui.autostart.store(now, Ordering::Relaxed);
                SendMessageW(
                    ui.autostart_box,
                    BM_SETCHECK,
                    if now { BST_CHECKED as WPARAM } else { BST_UNCHECKED as WPARAM },
                    0,
                );
                set_event(ui, if now { "已开启开机自动启动" } else { "已关闭开机自动启动" });
            }
            winx::TrayEvent::TogglePause => {
                toggle_pause(ui);
            }
            winx::TrayEvent::Quit => quit = true,
        }
    }
    if quit {
        ui.quitting = true;
        DestroyWindow(ui.hwnd);
        return;
    }

    // 3) 监控线程投来的「刚刚打开」（先取出字符串，结束对 mutex 的借用）
    let recorded = ui.last_event.lock().ok().and_then(|mut g| g.take());
    if let Some((exe, _counted)) = recorded {
        let name = store::file_stem(&exe);
        set_event(ui, &format!("刚刚打开：{name}"));
    }

    // 4) 事件文本 8 秒后恢复默认
    if !ui.event_text.is_empty() && store::unix_now() - ui.event_at > 8.0 {
        ui.event_text.clear();
        update_status(ui);
    }

    // 5) 启动 6 秒后回收一次工作集：把首启扫描 + 首屏图标加载的峰值页
    //    还给系统（之后进入稳态，不会再回到峰值）
    if !ui.trimmed_once && store::unix_now() - ui.started_at > 6.0 {
        ui.trimmed_once = true;
        trim_working_set();
    }
}

/// 把 `[from, to]` 行加入图标加载队列（去重）。
fn queue_icons(ui: &mut Ui, from: usize, to: usize) {
    if ui.rows.is_empty() {
        return;
    }
    let last = ui.rows.len() - 1;
    let to = to.min(last);
    for i in from..=to {
        if ui.icon_index.contains_key(&ui.rows[i].path) {
            continue;
        }
        if ui.icon_queued.insert(i) {
            ui.icon_queue.push(i);
        }
    }
}

/// 从队列取若干行加载图标（ImageList 只能在 UI 线程访问，所以分批做）。
unsafe fn load_icons_step(ui: &mut Ui, budget: usize) {
    if ui.icon_queue.is_empty() || ui.himl == 0 {
        return;
    }
    let take = budget.min(ui.icon_queue.len());
    let batch: Vec<usize> = ui.icon_queue.drain(..take).collect();
    for i in batch {
        ui.icon_queued.remove(&i);
        let Some(row) = ui.rows.get(i) else { continue };
        let path = row.path.clone();
        if ui.icon_index.contains_key(&path) {
            continue;
        }
        let idx = match winx::extract_hicon(&path, !ui.icon_large) {
            Some(hicon) => {
                let image = ImageList_ReplaceIcon(ui.himl, -1, hicon);
                DestroyIcon(hicon);
                image
            }
            None => -1,
        };
        ui.icon_index.insert(path, idx);
    }
    InvalidateRect(ui.list, std::ptr::null(), 0);
}

unsafe fn fill_dispinfo(ui: &mut Ui, di: &mut NMLVDISPINFOW) {
    let idx = di.item.iItem;
    if idx < 0 || idx as usize >= ui.rows.len() {
        return;
    }
    let idx = idx as usize;
    if di.item.mask & LVIF_TEXT != 0 {
        let sub = di.item.iSubItem.max(0) as usize;
        if let Some(c) = ui.cells.get(idx).and_then(|row| row.get(sub)) {
            di.item.pszText = c.as_ptr() as *mut u16;
            di.item.cchTextMax = c.len() as i32;
        }
    }
    if di.item.mask & LVIF_IMAGE != 0 {
        let ii = if di.item.iSubItem == 0 {
            ui.icon_index
                .get(&ui.rows[idx].path)
                .copied()
                .unwrap_or(-1)
        } else {
            -1
        };
        di.item.iImage = ii;
    }
}

// ------------------------------------------------------------------ 命令与动作

unsafe fn on_command(ui: &mut Ui, wparam: WPARAM) {
    let id = (wparam & 0xFFFF) as i32;
    let code = ((wparam >> 16) & 0xFFFF) as u32;
    match id {
        ID_SEARCH => {
            if code == EN_CHANGE {
                // 防抖：150ms 内的连续输入只查一次（不再逐键全量重建列表）
                SetTimer(ui.hwnd, TIMER_SEARCH, 150, None);
            }
        }
        ID_SORT => {
            if code == CBN_SELCHANGE {
                let i = SendMessageW(ui.sort_box, CB_GETCURSEL, 0, 0);
                if i >= 0 {
                    ui.sort_idx = i as usize;
                    refresh(ui, false);
                }
            }
        }
        ID_BTN_LAUNCH => {
            if let Some(p) = selected_path(ui) {
                act_launch(ui, &p);
            }
        }
        ID_BTN_REVEAL => {
            if let Some(p) = selected_path(ui) {
                winx::reveal_in_explorer(&p);
                set_event(ui, "已在资源管理器中定位");
            }
        }
        ID_BTN_COPY => {
            if let Some(p) = selected_path(ui) {
                if winx::clipboard_set_text(&p) {
                    set_event(ui, "完整路径已复制到剪贴板");
                }
            }
        }
        ID_BTN_REMOVE => {
            if let Some(p) = selected_path(ui) {
                if let Ok(mut st) = ui.store.lock() {
                    let _ = st.remove(&p);
                }
                set_event(ui, "已从历史中移除");
                refresh(ui, false);
            }
        }
        ID_BTN_PIN => {
            if let Some(p) = selected_path(ui) {
                act_pin(ui, &p);
            }
        }
        ID_BTN_PIN_ANY => {
            act_pin_any(ui);
        }
        ID_BTN_PRUNE => {
            let n = ui
                .store
                .lock()
                .map(|mut st| st.prune_missing().unwrap_or(0))
                .unwrap_or(0);
            let msg = if n > 0 {
                format!("已清理 {n} 条失效记录")
            } else {
                "没有失效记录".to_string()
            };
            set_event(ui, &msg);
            refresh(ui, false);
        }
        ID_CHK_AUTOSTART => {
            let checked =
                SendMessageW(ui.autostart_box, BM_GETCHECK, 0, 0) == BST_CHECKED as isize;
            let now = winx::set_autostart(checked);
            ui.autostart.store(now, Ordering::Relaxed);
            set_event(ui, if now { "已开启开机自动启动" } else { "已关闭开机自动启动" });
        }
        ID_MENU_LAUNCH | ID_MENU_REVEAL | ID_MENU_COPY | ID_MENU_REMOVE => {
            dispatch_menu_action(ui, id);
        }
        _ => {}
    }
}

unsafe fn dispatch_menu_action(ui: &mut Ui, id: i32) {
    match id {
        ID_MENU_LAUNCH => {
            if let Some(p) = selected_path(ui) {
                act_launch(ui, &p);
            }
        }
        ID_MENU_REVEAL => {
            if let Some(p) = selected_path(ui) {
                winx::reveal_in_explorer(&p);
                set_event(ui, "已在资源管理器中定位");
            }
        }
        ID_MENU_COPY => {
            if let Some(p) = selected_path(ui) {
                if winx::clipboard_set_text(&p) {
                    set_event(ui, "完整路径已复制到剪贴板");
                }
            }
        }
        ID_MENU_PIN => {
            if let Some(p) = selected_path(ui) {
                act_pin(ui, &p);
            }
        }
        ID_MENU_REMOVE => {
            if let Some(p) = selected_path(ui) {
                if let Ok(mut st) = ui.store.lock() {
                    let _ = st.remove(&p);
                }
                set_event(ui, "已从历史中移除");
                refresh(ui, false);
            }
        }
        ID_MENU_PRUNE => {
            let n = ui
                .store
                .lock()
                .map(|mut st| st.prune_missing().unwrap_or(0))
                .unwrap_or(0);
            let msg = if n > 0 {
                format!("已清理 {n} 条失效记录")
            } else {
                "没有失效记录".to_string()
            };
            set_event(ui, &msg);
            refresh(ui, false);
        }
        ID_MENU_PAUSE => toggle_pause(ui),
        ID_MENU_HELP => show_help(ui.hwnd),
        _ => {}
    }
}

/// 切换「暂停记录」（主菜单与托盘菜单共用）。
unsafe fn toggle_pause(ui: &mut Ui) {
    let now = !ui.paused.load(Ordering::Relaxed);
    ui.paused.store(now, Ordering::Relaxed);
    set_event(
        ui,
        if now { "已暂停记录（不再写入历史）" } else { "已恢复记录" },
    );
    update_status(ui);
}

/// 把某个路径钉到桌面：创建 + 立刻读回校验 + 标记「桌面」列。
unsafe fn act_pin(ui: &mut Ui, target: &str) {
    let name = store::file_stem(target);
    match shortcut::pin_to_desktop(target, &name) {
        Ok((lnk, verified)) => {
            if let Ok(mut st) = ui.store.lock() {
                let _ = st.mark_pinned(target);
            }
            let base = std::path::Path::new(&lnk)
                .file_name()
                .map(|s| s.to_string_lossy().to_string())
                .unwrap_or_else(|| lnk.clone());
            let suffix = if verified { "（已校验）" } else { "（读回不一致）" };
            set_event(ui, &format!("已创建桌面快捷方式：{base}{suffix}"));
            refresh(ui, false);
        }
        Err(e) => {
            log::warn!("钉桌面失败: {e}");
            winx::message_box_on(ui.hwnd, &format!("创建桌面快捷方式失败：\n{e}"));
        }
    }
}

/// 「钉任意程序…」：先选「程序 exe」还是「软件文件夹」→ 识别主程序 → 创建。
unsafe fn act_pin_any(ui: &mut Ui) {
    let hmenu = CreatePopupMenu();
    if hmenu.is_null() {
        return;
    }
    let a = winx::wide("选择程序 exe…");
    let b = winx::wide("选择软件文件夹…");
    AppendMenuW(hmenu, MF_STRING, 1, a.as_ptr());
    AppendMenuW(hmenu, MF_STRING, 2, b.as_ptr());
    let mut pt: POINT = std::mem::zeroed();
    if let Some(hb) = ui.buttons.last() {
        let mut r: RECT = std::mem::zeroed();
        GetWindowRect(*hb, &mut r);
        pt.x = r.left;
        pt.y = r.bottom;
    } else {
        GetCursorPos(&mut pt);
    }
    SetForegroundWindow(ui.hwnd);
    let cmd = TrackPopupMenu(
        hmenu,
        TPM_RIGHTBUTTON | TPM_RETURNCMD,
        pt.x,
        pt.y,
        0,
        ui.hwnd,
        std::ptr::null(),
    );
    DestroyMenu(hmenu);

    let path = match cmd {
        1 => pick_file_dialog(),
        2 => pick_folder_dialog(),
        _ => None,
    };
    let Some(path) = path else { return };
    let res = resolver::resolve(&path);
    match (res.ok, res.target) {
        (true, Some(target)) => act_pin(ui, &target),
        _ => winx::message_box_on(ui.hwnd, &format!("无法识别程序：\n{}", res.message)),
    }
}

/// 文件夹选择器（SHBrowseForFolderW）。取消返回 None。
unsafe fn pick_folder_dialog() -> Option<String> {
    use windows_sys::Win32::UI::Shell::{SHBrowseForFolderW, SHGetPathFromIDListW, BROWSEINFOW};
    let title = winx::wide("选择软件所在的文件夹");
    let mut bi: BROWSEINFOW = std::mem::zeroed();
    bi.lpszTitle = title.as_ptr();
    // BIF_RETURNONLYFSDIRS(0x1) | BIF_NEWDIALOGSTYLE(0x40)
    bi.ulFlags = 0x0001 | 0x0040;
    let pidl = SHBrowseForFolderW(&bi);
    if pidl.is_null() {
        return None;
    }
    let mut buf = [0u16; 1024];
    let ok = SHGetPathFromIDListW(pidl, buf.as_mut_ptr());
    windows_sys::Win32::System::Com::CoTaskMemFree(pidl as *const c_void);
    if ok == 0 {
        return None;
    }
    let n = buf.iter().position(|&c| c == 0).unwrap_or(0);
    if n == 0 {
        None
    } else {
        Some(String::from_utf16_lossy(&buf[..n]))
    }
}

/// 文件选择器（只选 .exe）。取消返回 None。
unsafe fn pick_file_dialog() -> Option<String> {
    use windows_sys::Win32::UI::Controls::Dialogs::{
        GetOpenFileNameW, OPENFILENAMEW, OFN_FILEMUSTEXIST, OFN_NOCHANGEDIR, OFN_PATHMUSTEXIST,
    };
    let mut buf = [0u16; 1024];
    let filter: Vec<u16> = "程序 (*.exe)\0*.exe\0所有文件 (*.*)\0*.*\0\0"
        .encode_utf16()
        .collect();
    let title = winx::wide("选择程序 exe");
    let mut ofn: OPENFILENAMEW = std::mem::zeroed();
    ofn.lStructSize = std::mem::size_of::<OPENFILENAMEW>() as u32;
    ofn.lpstrFilter = filter.as_ptr();
    ofn.lpstrFile = buf.as_mut_ptr();
    ofn.nMaxFile = buf.len() as u32;
    ofn.lpstrTitle = title.as_ptr();
    // OFN_NOCHANGEDIR：不改进程当前目录（否则相对路径全乱）
    ofn.Flags = OFN_FILEMUSTEXIST | OFN_PATHMUSTEXIST | OFN_NOCHANGEDIR;
    if GetOpenFileNameW(&mut ofn) == 0 {
        return None;
    }
    let n = buf.iter().position(|&c| c == 0).unwrap_or(0);
    if n == 0 {
        None
    } else {
        Some(String::from_utf16_lossy(&buf[..n]))
    }
}

unsafe fn act_launch(ui: &mut Ui, path: &str) {
    if std::path::Path::new(path).is_file() {
        winx::launch_path(path);
        let name = store::file_stem(path);
        set_event(ui, &format!("已启动：{name}"));
    } else {
        winx::message_box_on(ui.hwnd, &format!("文件已不存在：\n{path}"));
    }
}

unsafe fn show_context_menu(ui: &mut Ui) {
    if selected_path(ui).is_none() {
        return;
    }
    let hmenu = CreatePopupMenu();
    if hmenu.is_null() {
        return;
    }
    let paused_now = ui.paused.load(Ordering::Relaxed);
    let items = [
        (ID_MENU_LAUNCH, "启动".to_string()),
        (ID_MENU_REVEAL, "打开所在文件夹".to_string()),
        (ID_MENU_COPY, "复制完整路径".to_string()),
        (ID_MENU_PIN, "创建桌面快捷方式".to_string()),
        (ID_MENU_REMOVE, "从历史中移除".to_string()),
        (ID_MENU_PRUNE, "清理失效记录".to_string()),
        (
            ID_MENU_PAUSE,
            if paused_now { "恢复记录".to_string() } else { "暂停记录".to_string() },
        ),
        (ID_MENU_HELP, "帮助 (F1)".to_string()),
    ];
    for (i, (id, label)) in items.iter().enumerate() {
        if i == 4 {
            AppendMenuW(hmenu, MF_SEPARATOR, 0, std::ptr::null());
        }
        let l = winx::wide(label);
        AppendMenuW(hmenu, MF_STRING, *id as usize, l.as_ptr());
    }
    let mut pt: POINT = std::mem::zeroed();
    GetCursorPos(&mut pt);
    SetForegroundWindow(ui.hwnd); // 不设前台，点菜单外面不会消失
    let cmd = TrackPopupMenu(
        hmenu,
        TPM_RIGHTBUTTON | TPM_RETURNCMD,
        pt.x,
        pt.y,
        0,
        ui.hwnd,
        std::ptr::null(),
    );
    DestroyMenu(hmenu);
    if cmd != 0 {
        dispatch_menu_action(ui, cmd as i32);
    }
}

// ------------------------------------------------------------------ 小工具

unsafe fn selected_path(ui: &Ui) -> Option<String> {
    let i = SendMessageW(ui.list, LVM_GETNEXTITEM, usize::MAX, LVNI_SELECTED as LPARAM);
    if i < 0 {
        return None;
    }
    ui.rows.get(i as usize).map(|r| r.path.clone())
}

/// 光标位置 → 列表行号。
///
/// owner-data 虚拟列表上 `LVM_HITTEST` 一律返回 -1（实测：342 项、坐标正确也不认），
/// 所以改用实测布局换算：行 0 矩形的 top 即表头高度，高度即行高（本列表固定行高）。
unsafe fn hit_row_at_cursor(ui: &Ui) -> i32 {
    let mut pt: POINT = std::mem::zeroed();
    GetCursorPos(&mut pt);
    ScreenToClient(ui.list, &mut pt);
    let count = SendMessageW(ui.list, LVM_GETITEMCOUNT, 0, 0) as i32;
    if count <= 0 {
        return -1;
    }
    let mut row0: RECT = std::mem::zeroed();
    let ok = SendMessageW(ui.list, LVM_GETITEMRECT, 0, &mut row0 as *mut _ as LPARAM) as i32;
    if ok == 0 {
        return -1;
    }
    let header_h = row0.top;
    let row_h = row0.bottom - row0.top;
    let y = pt.y - header_h;
    if row_h <= 0 || y < 0 {
        return -1;
    }
    let row = y / row_h;
    if row >= 0 && row < count {
        row
    } else {
        -1
    }
}

unsafe fn set_item_selected(list: HWND, idx: i32) {
    let mut item: LVITEMW = std::mem::zeroed();
    item.stateMask = LVIS_SELECTED | LVIS_FOCUSED;
    item.state = LVIS_SELECTED | LVIS_FOCUSED;
    SendMessageW(list, LVM_SETITEMSTATE, idx as usize, &mut item as *mut _ as LPARAM);
}

unsafe fn update_selection_ui(ui: &mut Ui) {
    let has = selected_path(ui).is_some();
    // 前四个按钮需要选中行；"清理失效记录"始终可用
    for (i, _) in BUTTONS.iter().enumerate().take(4) {
        if let Some(hb) = ui.buttons.get(i) {
            EnableWindow(*hb, has as i32);
        }
    }
}

unsafe fn update_status(ui: &mut Ui) {
    let left = if !ui.event_text.is_empty() {
        ui.event_text.clone()
    } else if ui.paused.load(Ordering::Relaxed) {
        "已暂停记录（右键菜单或托盘菜单可恢复）".to_string()
    } else if ui.total == 0 {
        "还没有记录 —— 打开任意软件后它会出现在这里；也可以点「钉任意程序…」直接钉到桌面"
            .to_string()
    } else if ui.rows.is_empty() {
        "没有匹配的应用（清空搜索框可看全部）".to_string()
    } else {
        "后台记录中（关闭窗口 = 缩到托盘继续记录 · F1 查看帮助）".to_string()
    };
    let right = format!("已收录 {} 个应用 · 今日打开 {}", ui.total, ui.today);
    set_status_text(ui.status, 0, &left);
    set_status_text(ui.status, 1, &right);
}

unsafe fn set_status_text(status: HWND, part: usize, text: &str) {
    let t = winx::wide(text);
    SendMessageW(status, SB_SETTEXTW, part, t.as_ptr() as LPARAM);
}

unsafe fn set_event(ui: &mut Ui, text: &str) {
    ui.event_text = text.to_string();
    ui.event_at = store::unix_now();
    update_status(ui);
}

unsafe fn get_window_text(h: HWND) -> String {
    let len = GetWindowTextLengthW(h);
    if len <= 0 {
        return String::new();
    }
    let mut buf = vec![0u16; len as usize + 1];
    let n = GetWindowTextW(h, buf.as_mut_ptr(), buf.len() as i32);
    if n <= 0 {
        return String::new();
    }
    String::from_utf16_lossy(&buf[..n as usize])
}

fn human_time(ts: Option<f64>) -> String {
    use chrono::{Datelike, Local, TimeZone};
    let Some(t) = ts else {
        return "—".into();
    };
    if t <= 0.0 {
        return "—".into();
    }
    let Some(dt) = Local.timestamp_opt(t as i64, 0).single() else {
        return "—".into();
    };
    let d = store::unix_now() - t;
    if d < 0.0 {
        return "刚刚".into();
    }
    if d < 60.0 {
        return "刚刚".into();
    }
    if d < 3600.0 {
        return format!("{} 分钟前", (d / 60.0) as i64);
    }
    let now = Local::now();
    if dt.date_naive() == now.date_naive() {
        return dt.format("今天 %H:%M").to_string();
    }
    let yesterday = (now - chrono::Duration::days(1)).date_naive();
    if dt.date_naive() == yesterday {
        return dt.format("昨天 %H:%M").to_string();
    }
    if dt.year() == now.year() {
        return dt.format("%m-%d %H:%M").to_string();
    }
    dt.format("%Y-%m-%d").to_string()
}
