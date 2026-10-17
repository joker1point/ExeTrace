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

use windows_sys::Win32::Foundation::{COLORREF, HWND, LPARAM, LRESULT, POINT, RECT, WPARAM};
use windows_sys::Win32::Graphics::Gdi::{
    CreateFontIndirectW, CreatePen, CreateSolidBrush, DeleteObject, DrawTextW, Ellipse, GetObjectW,
    GetStockObject, InvalidateRect, LineTo, LOGFONTW, MoveToEx, RoundRect, ScreenToClient,
    SelectObject, SetBkMode, SetTextColor, UpdateWindow, COLOR_WINDOW, DT_BOTTOM, DT_END_ELLIPSIS,
    DT_LEFT, DT_NOPREFIX, DT_SINGLELINE, DT_TOP, FW_BOLD, FW_NORMAL, HGDIOBJ, HFONT, NULL_PEN,
    PS_SOLID, TRANSPARENT,
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
const ID_DAYS: i32 = 1005;
const ID_VIEW: i32 = 1006;

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
const ID_MENU_GH: i32 = 1209;
const ID_MENU_UPDATE: i32 = 1210;
const ID_MENU_RESCAN: i32 = 1211;
const ID_MENU_WHITELIST: i32 = 1212;

/// 全局热键 ID（Ctrl+Alt+E 呼出主窗口）。
const HOTKEY_ID: i32 = 9001;

const TIMER_POLL: usize = 1;
const TIMER_ICONS: usize = 2;

/// 浮层卡片停留时长（秒）。到期由 `tick()`（500ms 一拍）隐藏 —— 够用，不必再开定时器。
const TOAST_SECS: f64 = 3.4;

/// 自定义消息：窗口显示后按真实 DPI 重建字体/尺寸/布局。
/// WM_CREATE 阶段窗口还没关联到显示器，GetDpiForWindow 会返回 96。
const WM_APP_RELAYOUT: u32 = WM_APP + 10;

/// 自定义消息：子类化的搜索框请求「启动当前选中项」（回车）。
const WM_APP_LAUNCH_SELECTED: u32 = WM_APP + 20;

/// 搜索防抖定时器（150ms 内的连续输入只查一次）。
const TIMER_SEARCH: usize = 3;

/// 「近 N 天」输入框的防抖定时器（500ms 内连续输入只应用一次）。
const TIMER_DAYS: usize = 4;

/// `EM_SETSEL`（文本框全选；windows-sys 未导出该消息常量）。
const EM_SETSEL: u32 = 0x00B1;

/// 帮助文本（F1 / 右键菜单「帮助」）。
const HELP_TEXT: &str = "ExeTrace —— 应用历史定位器\n\n\
【它做什么】\n\
后台记录你打开过的每一个应用，关掉软件界面后随时能在这里找回它。\n\
正在运行的应用会自动隐藏（它们不需要「找回」，关掉后即恢复显示）。\n\
首次启动会扫描系统里的历史痕迹，所以不用等下一次打开就有记录。\n\
本机散落的 git 项目会被扫描成条目（顶部「视图」可切 全部 / 应用 / 项目 / HTML）；\n\
项目行：双击打开文件夹，右键可打开主页 / 更新（git pull）。\n\
HTML 行：浏览器打开过的本地网页（双击用默认浏览器再打开）。\n\n\
【常用操作】\n\
  双击列表项                启动该应用（项目 = 打开文件夹）\n\
  右键列表项                启动 / 打开所在文件夹 / 复制路径 / 创建桌面快捷方式 /\n\
                            加入 memfuse 白名单 / 打开 GitHub 页 / 更新 / 从历史中移除 /\n\
                            重新扫描项目 / 帮助\n\
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
  ignore       忽略列表：不想被记录的 exe 文件名（如 \"spam.exe\"）\n\
  categories   分类覆盖：给某个 exe 指定分类（如 {\"code.exe\": \"开发\"}）\n\
  history_days 列表默认显示天数：只加载最近 N 天的记录，0 = 全部（默认 30；\n\
               界面左上角的「近 N 天」输入框会写回这里）\n\
  project_roots 项目扫描根目录列表（默认扫 文档 / WorkBuddy / 桌面；改完自动生效）\n\
  memfuse_script memfuse 主脚本路径（右键「加入 memfuse 白名单」用；空 = 自动探测，\n\
               也可以用环境变量 MEMFUSE_SCRIPT 指定）\n\
  改完自动生效，无需重启。";

/// `EM_SETCUEBANNER`（编辑框水印；windows-sys 未导出该消息常量）。
const EM_SETCUEBANNER: u32 = 0x1501;

// 默认排序 = 最近打开（2026-10-16 用户口径「默认按时间排序」）；
// 智能排序（frecency：打开次数 × 时间衰减）保留在下拉里，要用的自己切。
const SORTS: [(&str, &str); 4] = [
    ("最近打开", "recent"),
    ("智能排序", "smart"),
    ("打开次数", "count"),
    ("名称", "name"),
];

/// 「视图」下拉选项：索引 ↔ query 的 view 参数（见 `view_key`）。
/// 2026-10-17 加「HTML」—— 浏览器打开过的本地网页（`source='html'`）。
const VIEWS: [&str; 4] = ["全部", "应用", "项目", "HTML"];

fn view_key(idx: usize) -> &'static str {
    ["all", "app", "project", "html"][idx.min(3)]
}

/// 列定义：标题 / 初始宽度（96dpi）/ 对齐（对齐 Python v3.1 的 7 列）。
const COLS: [(&str, i32, i32); 7] = [
    ("应用", 240, LVCFMT_LEFT),
    ("位置", 340, LVCFMT_LEFT),
    ("分类", 72, LVCFMT_LEFT),
    ("次数", 52, LVCFMT_RIGHT),
    ("使用时长", 84, LVCFMT_RIGHT),
    ("最后打开", 110, LVCFMT_LEFT),
    ("桌面", 46, LVCFMT_LEFT),
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
    days_lbl1: HWND,
    days_box: HWND,
    days_lbl2: HWND,
    status: HWND,
    /// 「视图」下拉：全部 / 应用 / 项目（2026-10-15）。
    view_box: HWND,
    autostart_box: HWND,
    buttons: Vec<HWND>,
    font: HFONT,
    rows: Vec<AppRow>,
    /// 每行每列的 UTF-16（NUL 结尾）：ListView 是 Unicode 控件，
    /// LVN_GETDISPINFO 的 pszText 必须是宽字符指针，且消息返回后仍有效。
    cells: Vec<Vec<Vec<u16>>>,
    rev: u64,
    sort_idx: usize,
    /// 「近 N 天」过滤（0 = 全部）；启动时取自 config.json 的 history_days。
    days: i64,
    /// 视图过滤：0 = 全部，1 = 应用，2 = 项目（顶部「视图」下拉）。
    view_idx: usize,
    /// 后台 git / 扫描动作的完成回执：(成功, 名称, 详情)。tick 里取走展示。
    git_result: Arc<Mutex<Option<(bool, String, String)>>>,
    /// memfuse 白名单动作的完成回执：(成功, 应用名, 详情)。tick 里取走展示。
    memfuse_result: Arc<Mutex<Option<(bool, String, String)>>>,
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
    /// 钉桌面成功后的**浮层卡片**：子窗口句柄 + 两行文案 + 出现时刻。
    ///
    /// 为什么是子窗口而不是在父窗口上直接画：父窗口只靠系统画刷打底、内容全由
    /// 子控件渲染，直接画出来的东西会被 ListView 盖住。
    toast: HWND,
    toast_title: String,
    toast_body: String,
    toast_at: f64,
    total: i64,
    projects: i64,
    /// 已收录的本地网页数（`source='html'`，2026-10-17）。
    html: i64,
    today: i64,
    /// 「已在运行的应用自动屏蔽」判定数据：`run_stems` 是参与判定的应用名集合
    /// （tick 复核必须与 refresh 同基准，否则集合比较永远不等、每次白刷列表）；
    /// `run_paths` / `run_names_fallback` 语义见 `winx::running_exe_lookup`。
    run_stems: std::collections::HashSet<String>,
    run_paths: std::collections::HashSet<String>,
    run_names_fallback: std::collections::HashSet<String>,
    /// 本轮被隐藏的运行中行数（状态栏展示）与上次复核时刻（3s 节流）。
    hidden_running: usize,
    run_check_at: f64,
}

/// 窗口期望尺寸：按 DPI 缩放，但**不超过屏幕工作区**。
///
/// 为什么必须设上限（2026-10-09 实测）：本机是 200% 缩放 + 小屏，`1120×740 × 2` 算出来
/// 是 `2240×1480`，而屏幕只有 `1360×768` —— 窗口比屏幕大近 4 倍，右侧那一大片（正是浮层
/// 卡片要停的位置）完全在屏幕外，用户根本看不到。高 DPI 的大屏上缩放仍然照常生效。
fn wanted_size(scale: f32) -> (i32, i32) {
    unsafe {
        let sw = GetSystemMetrics(SM_CXSCREEN) as f32;
        let sh = GetSystemMetrics(SM_CYSCREEN) as f32;
        let w = (1120.0 * scale).min(sw * 0.92).max(640.0);
        let h = (740.0 * scale).min(sh * 0.90).max(480.0);
        (w as i32, h as i32)
    }
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
            days_lbl1: std::ptr::null_mut(),
            days_box: std::ptr::null_mut(),
            days_lbl2: std::ptr::null_mut(),
            status: std::ptr::null_mut(),
            view_box: std::ptr::null_mut(),
            autostart_box: std::ptr::null_mut(),
            buttons: Vec::new(),
            font: std::ptr::null_mut(),
            rows: Vec::new(),
            cells: Vec::new(),
            rev: 0,
            sort_idx: 0,
            days: crate::config::current().history_days,
            view_idx: 0,
            git_result: Arc::new(Mutex::new(None)),
            memfuse_result: Arc::new(Mutex::new(None)),
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
            toast: std::ptr::null_mut(),
            toast_title: String::new(),
            toast_body: String::new(),
            toast_at: 0.0,
            total: 0,
            projects: 0,
            html: 0,
            today: 0,
            run_stems: std::collections::HashSet::new(),
            run_paths: std::collections::HashSet::new(),
            run_names_fallback: std::collections::HashSet::new(),
            hidden_running: 0,
            run_check_at: 0.0,
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
            wanted_size(s0).0,
            wanted_size(s0).1,
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
                wanted_size(s).0,
                wanted_size(s).1,
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
                // 最小尺寸同样要受屏幕约束：否则小屏 + 高 DPI 下"最小"就已经比屏幕大，
                // 窗口永远缩不进去（实测 820×480 × 2 = 1640×960 > 1360×768）。
                let sw = GetSystemMetrics(SM_CXSCREEN) as f32;
                let sh = GetSystemMetrics(SM_CYSCREEN) as f32;
                (*mmi).ptMinTrackSize.x = ((820.0 * s) as i32).min((sw * 0.55) as i32).max(520);
                (*mmi).ptMinTrackSize.y = ((480.0 * s) as i32).min((sh * 0.55) as i32).max(360);
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
                    refresh(&mut *ui, false); // 隐藏期间的记录变化在这里一次性补上
                }
            }
            0
        }
        WM_DRAWITEM => {
            // 浮层卡片（STATIC + SS_OWNERDRAW）的绘制回调
            let ui = ctx(hwnd);
            if ui.is_null() {
                return 0;
            }
            let dis = lparam as *const DRAWITEMSTRUCT;
            if !dis.is_null() && (*dis).hwndItem == (*ui).toast && !(*ui).toast.is_null() {
                draw_toast(&*ui, &*dis);
                return 1;
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
            } else if wparam == TIMER_DAYS {
                // 「近 N 天」防抖到期：应用输入（校验/持久化/重建见 apply_days_input）
                KillTimer(hwnd, TIMER_DAYS);
                apply_days_input(&mut *ui);
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

    // 「视图」下拉：全部 / 应用 / 项目 / HTML
    // （项目 = 本机 git 仓库扫描，2026-10-15；HTML = 浏览器打开过的本地网页，2026-10-17）
    ui.view_box = CreateWindowExW(
        0,
        WC_COMBOBOX,
        empty.as_ptr(),
        WS_CHILD | WS_VISIBLE | WS_TABSTOP | WS_VSCROLL | CBS_DROPDOWNLIST as u32,
        0,
        0,
        0,
        0,
        ui.hwnd,
        ID_VIEW as isize as HMENU,
        hinst,
        std::ptr::null_mut(),
    );
    set_font(ui.view_box);
    for label in VIEWS {
        let l = winx::wide(label);
        SendMessageW(ui.view_box, CB_ADDSTRING, 0, l.as_ptr() as LPARAM);
    }
    SendMessageW(ui.view_box, CB_SETCURSEL, 0, 0);

    // 「近 N 天」过滤：只显示最近 N 天内打开过的记录（0 = 全部）。
    // 样式用字面量：windows-sys 未导出全部 ES_*/SS_* 常量（ES_CENTER=0x0001 / ES_NUMBER=0x2000 / SS_CENTERIMAGE=0x0200）。
    let l1 = winx::wide("近");
    ui.days_lbl1 = CreateWindowExW(
        0,
        WC_STATIC,
        l1.as_ptr(),
        WS_CHILD | WS_VISIBLE | 0x0200,
        0,
        0,
        0,
        0,
        ui.hwnd,
        std::ptr::null_mut(),
        hinst,
        std::ptr::null_mut(),
    );
    set_font(ui.days_lbl1);
    let days_text = winx::wide(&ui.days.to_string());
    ui.days_box = CreateWindowExW(
        WS_EX_CLIENTEDGE,
        WC_EDIT,
        days_text.as_ptr(),
        WS_CHILD | WS_VISIBLE | WS_TABSTOP | 0x0001 | 0x2000,
        0,
        0,
        0,
        0,
        ui.hwnd,
        ID_DAYS as isize as HMENU,
        hinst,
        std::ptr::null_mut(),
    );
    set_font(ui.days_box);
    let l2 = winx::wide("天");
    ui.days_lbl2 = CreateWindowExW(
        0,
        WC_STATIC,
        l2.as_ptr(),
        WS_CHILD | WS_VISIBLE | 0x0200,
        0,
        0,
        0,
        0,
        ui.hwnd,
        std::ptr::null_mut(),
        hinst,
        std::ptr::null_mut(),
    );
    set_font(ui.days_lbl2);

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

    // 浮层卡片（钉桌面成功的强反馈）：STATIC + SS_OWNERDRAW，交给父窗口的 WM_DRAWITEM 画。
    // 初始隐藏，由 show_toast() 显示、tick() 到点隐藏。
    let toast = CreateWindowExW(
        0,
        WC_STATIC,
        empty.as_ptr(),
        WS_CHILD | 0x0000_000D, // SS_OWNERDRAW（windows-sys 未导出该常量，直接用值）
        0,
        0,
        0,
        0,
        ui.hwnd,
        std::ptr::null_mut(),
        hinst,
        std::ptr::null_mut(),
    );
    ui.toast = toast;

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

    // 状态栏自己贴底，先让它定好高度（右格文案含「近 N 天 N 条」，留足宽度）
    SendMessageW(ui.status, WM_SIZE, 0, 0);
    let mut sr: RECT = std::mem::zeroed();
    GetWindowRect(ui.status, &mut sr);
    let sb_h = sr.bottom - sr.top;
    let mid = (w - (450.0 * s) as i32).max(w / 2);
    let parts = [mid, -1i32];
    SendMessageW(ui.status, SB_SETPARTS, 2, parts.as_ptr() as LPARAM);

    // 行 1：搜索框（弹性）＋ 排序 ＋「近 N 天」
    // （单行工具条在窗口略窄时，右对齐的按钮组会压住搜索框与排序框 —— 2026-10-14 拆两行）
    let top = pad;
    let sort_w = (120.0 * s) as i32;
    let view_w = (86.0 * s) as i32;
    let lbl1_w = (26.0 * s) as i32;
    let days_w = (56.0 * s) as i32;
    let lbl2_w = (20.0 * s) as i32;
    let fixed1 = sort_w + view_w + lbl1_w + days_w + lbl2_w + gap * 5;
    let search_w = (w - pad * 2 - fixed1).max((180.0 * s) as i32);
    let mut x = pad;
    MoveWindow(ui.search, x, top, search_w, row_h, 1);
    x += search_w + gap;
    MoveWindow(ui.view_box, x, top, view_w, row_h * 6, 1);
    x += view_w + gap;
    MoveWindow(ui.sort_box, x, top, sort_w, row_h * 6, 1);
    x += sort_w + gap;
    MoveWindow(ui.days_lbl1, x, top, lbl1_w, row_h, 1);
    x += lbl1_w;
    MoveWindow(ui.days_box, x, top, days_w, row_h, 1);
    x += days_w;
    MoveWindow(ui.days_lbl2, x, top, lbl2_w, row_h, 1);

    // 行 2：操作按钮左对齐 ＋ 开机自启右对齐
    let row2 = top + row_h + gap;
    let mut bx = pad;
    for (idx, (_, _, bw)) in BUTTONS.iter().enumerate() {
        let bw = (bw * s) as i32;
        if let Some(hb) = ui.buttons.get(idx) {
            MoveWindow(*hb, bx, row2, bw, row_h, 1);
        }
        bx += bw + gap;
    }
    let chk_w = (130.0 * s) as i32;
    MoveWindow(ui.autostart_box, w - pad - chk_w, row2, chk_w, row_h, 1);

    // 列表填满中间（列宽按当前客户区重算：「位置」列弹性）
    let list_top = row2 + row_h + pad;
    let list_h = (h - list_top - sb_h - pad).max(60);
    MoveWindow(ui.list, pad, list_top, (w - pad * 2).max(60), list_h, 1);
    apply_column_widths(ui);

    // 浮层卡片：跟随窗口尺寸（真正显示前还会再算一次，见 place_toast）
    place_toast(ui);
}

/// 按当前 DPI 应用列宽：除「位置」（第 2 列）外按 COLS 的 96dpi 基准缩放，
/// 「位置」吃掉剩余宽度。必须在真实 DPI 就绪后调用（WM_CREATE 阶段 dpi=96，
/// 显示后 WM_APP_RELAYOUT 会再走一遍 layout 校正）。
unsafe fn apply_column_widths(ui: &mut Ui) {
    if ui.list.is_null() {
        return;
    }
    let s = winx::dpi_for_window(ui.hwnd) as f32 / 96.0;
    let mut rc: RECT = std::mem::zeroed();
    GetClientRect(ui.list, &mut rc);
    let total = (rc.right - rc.left).max(120);
    let others: i32 = COLS
        .iter()
        .enumerate()
        .filter(|(i, _)| *i != 1)
        .map(|(_, (_, w, _))| (*w as f32 * s) as i32)
        .sum();
    let path_w = (total - others - 4).max((200.0 * s) as i32);
    for (i, (_, width, _)) in COLS.iter().enumerate() {
        let w = if i == 1 { path_w } else { (*width as f32 * s) as i32 };
        // LVM_SETCOLUMNWIDTH = LVM_FIRST(0x1000) + 30（字面量，避免常量导出差异）
        SendMessageW(ui.list, 0x101E, i, w as isize as LPARAM);
    }
}

/// 把浮层卡片摆到**当前客户区的居中偏上**位置。
///
/// 为什么不只在 `layout()` 里算：`layout()` 首次执行发生在 `WM_CREATE` —— 那时窗口
/// 还是初始尺寸、`GetDpiForWindow` 也还没拿到真实 DPI（代码里那条注释说得很清楚）。
/// 实测后果：卡片按 100% 尺寸算、并停在旧位置不居中。所以显示前再算一次，保证贴当前窗口。
unsafe fn place_toast(ui: &Ui) {
    if ui.toast.is_null() {
        return;
    }
    let mut rc: RECT = std::mem::zeroed();
    GetClientRect(ui.hwnd, &mut rc);
    let w = rc.right - rc.left;
    let s = winx::dpi_for_window(ui.hwnd) as f32 / 96.0;
    let pad = (8.0 * s) as i32;
    let cw = ((560.0 * s) as i32).min((w - pad * 2).max(240));
    let ch = (92.0 * s) as i32;
    // 贴在**右侧**：列表右端（「桌面」列之后）是一大片没有数据的空白区，
    // 卡片放那儿既醒目又不遮任何行内容 —— 用户口径：「右侧区域镂空，提示可以集中右侧」。
    // 不比手工居中：居中依赖客户区宽度的实时值，实测在尺寸变化后会出现"停在旧位置"。
    let cx = (w - cw - (28.0 * s) as i32).max(pad);
    let cy = (104.0 * s) as i32; // 顶部工具条之下
    log::info!(
        "toast 定位: client_w={w} dpi_s={s:.2} cw={cw} ch={ch} at=({cx},{cy}) title={:?}",
        ui.toast_title
    );
    SetWindowPos(
        ui.toast,
        std::ptr::null_mut(),
        cx,
        cy,
        cw,
        ch,
        SWP_NOZORDER | SWP_NOACTIVATE,
    );
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
        let mut ctrls = vec![
            ui.search,
            ui.sort_box,
            ui.days_lbl1,
            ui.days_box,
            ui.days_lbl2,
            ui.list,
            ui.status,
            ui.autostart_box,
        ];
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

/// 分类列文案：项目行显示档位（项目·自有 / 项目·克隆 / 项目·本地）；
/// 网页行显示「网页」；应用行走既有分类。
fn row_category(cfg: &crate::config::Config, r: &AppRow) -> String {
    if r.source == "html" {
        return "网页".to_string();
    }
    if r.source == "project" {
        let tier = match r.origin.as_str() {
            "own" => "自有",
            "clone" => "克隆",
            _ => "本地",
        };
        return format!("项目·{tier}");
    }
    crate::config::category_override_from(cfg, &r.path)
        .unwrap_or_else(|| winx::categorize(&r.path).to_string())
}

unsafe fn refresh(ui: &mut Ui, reset_icons: bool) {
    let keyword = get_window_text(ui.search);
    let sort = SORTS[ui.sort_idx.min(SORTS.len() - 1)].1;
    let days = ui.days;
    let view = view_key(ui.view_idx);
    let (rows, total, projects, html, today, rev) = match ui.store.lock() {
        Ok(st) => (
            st.query(&keyword, sort, 2000, days, view).unwrap_or_default(),
            st.total(),
            st.total_projects(),
            st.total_html(),
            st.count_today(),
            st.rev(),
        ),
        Err(_) => return,
    };

    let sel = selected_path(ui);

    // 「找回」工具的口径（2026-10-15）：正在运行的应用不需要找 —— 所有列表视图
    // （全部 / 近 N 天 / 搜索结果）统一隐藏。判定数据存进 ui，tick 里以同一
    // 基准（run_stems）做 3 秒节流复核，应用开/关后该藏该现跟着变。
    let run_stems: std::collections::HashSet<String> =
        rows.iter().map(|r| r.name.to_lowercase()).collect();
    let (run_paths, run_names_fallback) = winx::running_exe_lookup(&run_stems);
    let mut rows = rows;
    let before = rows.len();
    rows.retain(|r| {
        !run_paths.contains(&r.path.to_lowercase())
            && !run_names_fallback.contains(&r.name.to_lowercase())
    });
    ui.hidden_running = before - rows.len();
    ui.run_stems = run_stems;
    ui.run_paths = run_paths;
    ui.run_names_fallback = run_names_fallback;

    ui.rows = rows;
    ui.total = total;
    ui.projects = projects;
    ui.html = html;
    ui.today = today;
    ui.rev = rev;

    // 重建单元格文本缓冲（指针要在 LVN_GETDISPINFO 返回后仍有效 → 按行持有）
    // 分类覆盖整份取一次：category_override 会克隆整份 config，逐行调用纯属浪费。
    let cfg = crate::config::current();
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
                winx::wide(&row_category(&cfg, r)),
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

/// 「近 N 天」输入应用：解析失败（空/非法）回填当前值、不改变过滤；
/// 成功则夹取范围、持久化到 config.json 并重建列表（reset_icons：新行可能没图标）。
unsafe fn apply_days_input(ui: &mut Ui) {
    let text = get_window_text(ui.days_box);
    match text.trim().parse::<i64>() {
        Ok(n) => {
            let n = n.clamp(0, 3650);
            if n != ui.days {
                ui.days = n;
                crate::config::save_history_days(n);
                refresh(ui, true);
            }
        }
        Err(_) => {
            let cur = winx::wide(&ui.days.to_string());
            SetWindowTextW(ui.days_box, cur.as_ptr());
        }
    }
}

unsafe fn tick(ui: &mut Ui) {
    // 1) 数据版本变化才重查；窗口不可见或最小化时跳过列表重建（隐藏期间每 30s
    //    的全量重建纯属白干），呼出/还原路径会立即补一次刷新。
    let rev = ui.store.lock().map(|s| s.rev()).unwrap_or(ui.rev);
    if rev != ui.rev && IsWindowVisible(ui.hwnd) != 0 && IsIconic(ui.hwnd) == 0 {
        refresh(ui, false);
    }

    // 1b) 「运行中屏蔽」复核（3s 节流，窗口可见时才做）：应用开/关后，列表里
    //     该藏的行 / 该现的行要跟上。基准必须与 refresh 一致（ui.run_stems），
    //     只有判定结果真的变了才重建列表 —— 否则会每 3 秒白刷一次。
    if IsWindowVisible(ui.hwnd) != 0
        && IsIconic(ui.hwnd) == 0
        && !ui.run_stems.is_empty()
        && store::unix_now() - ui.run_check_at > 3.0
    {
        ui.run_check_at = store::unix_now();
        let (paths, fallback) = winx::running_exe_lookup(&ui.run_stems);
        if paths != ui.run_paths || fallback != ui.run_names_fallback {
            refresh(ui, false);
        }
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
                refresh(ui, false); // 隐藏期间的记录变化在这里一次性补上
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

    // 3b) 后台 git / 扫描动作的回执：成功一行提示，失败弹详情（2026-10-15）
    let git_done = ui.git_result.lock().ok().and_then(|mut g| g.take());
    if let Some((ok, name, detail)) = git_done {
        if ok {
            let latest = detail.contains("Already up to date")
                || detail.contains("Already up-to-date")
                || detail.contains("已经是最新");
            let msg = if name.is_empty() {
                detail
            } else if latest {
                format!("已是最新：{name}")
            } else {
                format!("已更新：{name}")
            };
            set_event(ui, &msg);
            refresh(ui, false);
        } else {
            set_event(ui, &format!("操作失败：{name}"));
            winx::message_box_on(ui.hwnd, &format!("{name}：\n{detail}"));
        }
    }

    // 3c) memfuse 白名单动作的回执：成功弹绿卡 + 一行状态，失败弹详情（2026-10-16）
    let mf_done = ui.memfuse_result.lock().ok().and_then(|mut g| g.take());
    if let Some((ok, name, detail)) = mf_done {
        if ok {
            set_event(ui, &format!("已加入 memfuse 白名单：{name}（立即生效）"));
            show_toast(
                ui,
                "已加入 memfuse 白名单",
                &format!("{name} —— 内存紧张时不会再被杀"),
                false,
            );
        } else {
            set_event(ui, &format!("加白名单失败：{name}"));
            winx::message_box_on(ui.hwnd, &format!("{name}：\n{detail}"));
        }
    }

    // 4) 事件文本 8 秒后恢复默认
    if !ui.event_text.is_empty() && store::unix_now() - ui.event_at > 8.0 {
        ui.event_text.clear();
        update_status(ui);
    }

    // 4b) 浮层卡片到点隐藏（500ms 一拍的粒度够）
    if !ui.toast.is_null() && ui.toast_at > 0.0 && store::unix_now() - ui.toast_at > TOAST_SECS {
        ShowWindow(ui.toast, SW_HIDE);
        ui.toast_at = 0.0;
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
        ID_VIEW => {
            if code == CBN_SELCHANGE {
                let i = SendMessageW(ui.view_box, CB_GETCURSEL, 0, 0);
                if i >= 0 {
                    let idx = i as usize;
                    if idx != ui.view_idx {
                        ui.view_idx = idx;
                        refresh(ui, true);
                    }
                }
            }
        }
        ID_DAYS => {
            if code == EN_CHANGE {
                // 防抖：连续输入时只应用一次（应用逻辑见 apply_days_input）
                SetTimer(ui.hwnd, TIMER_DAYS, 500, None);
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
        ID_MENU_GH => {
            if let Some(p) = selected_path(ui) {
                match crate::projects::browse_url(&p) {
                    Some(url) => {
                        winx::launch_path(&url);
                        set_event(ui, "已在浏览器打开项目主页");
                    }
                    None => winx::message_box_on(
                        ui.hwnd,
                        "这个项目没有可打开的远程页面（未配 remote，或不是 GitHub / Gitee 地址）。",
                    ),
                }
            }
        }
        ID_MENU_UPDATE => {
            if let Some(p) = selected_path(ui) {
                let name = store::file_stem(&p);
                let slot = ui.git_result.clone();
                std::thread::spawn(move || {
                    let (ok, detail) = crate::projects::git_pull(&p);
                    if let Ok(mut g) = slot.lock() {
                        *g = Some((ok, name, detail));
                    }
                });
                set_event(ui, "正在更新…（git pull）");
            }
        }
        ID_MENU_RESCAN => {
            let slot = ui.git_result.clone();
            let st = ui.store.clone();
            std::thread::spawn(move || {
                let stats = st.lock().map(|mut s| crate::projects::scan_all(&mut s));
                let (ok, name, detail) = match stats {
                    Ok(s) => (
                        true,
                        String::new(),
                        format!(
                            "项目扫描完成：发现 {} 个（自有 {} / 克隆 {} / 本地 {}）",
                            s.found, s.own, s.clone, s.local
                        ),
                    ),
                    Err(_) => (false, "项目扫描".to_string(), "存储被占用，扫描未执行".to_string()),
                };
                if let Ok(mut g) = slot.lock() {
                    *g = Some((ok, name, detail));
                }
            });
            set_event(ui, "正在扫描本机项目…");
        }
        ID_MENU_WHITELIST => {
            if let Some(p) = selected_path(ui) {
                act_whitelist(ui, &p);
            }
        }
        ID_MENU_PAUSE => toggle_pause(ui),
        ID_MENU_HELP => show_help(ui.hwnd),
        _ => {}
    }
}

/// 把这一行对应的进程名加进 memfuse 的 `protect-list.txt`（后台线程 + 回执）。
///
/// 场景（2026-10-16 用户口径）：内存紧张时应用**一开就被 memfuse 杀掉**，根本来不及
/// 打开白名单界面 —— 但它的历史记录一直在这张列表里，所以这里右键是最快的入口。
/// 动作放后台线程跑（要起一次 PowerShell，约 0.5-1.5 秒，不能堵 UI）。
unsafe fn act_whitelist(ui: &mut Ui, path: &str) {
    let name = store::file_stem(path);
    if name.is_empty() {
        return;
    }
    let Some(script) = crate::memfuse::resolve_script() else {
        winx::message_box_on(
            ui.hwnd,
            "没找到 memfuse 的 memory-guard.ps1。\n\n\
             在 config.json 里加一行指向它：\n  \
             \"memfuse_script\": \"C:\\\\...\\\\memfuse\\\\memory-guard.ps1\"\n\
             （也可以用环境变量 MEMFUSE_SCRIPT）",
        );
        return;
    };
    if crate::memfuse::is_listed(&script, &name) {
        set_event(ui, &format!("已在白名单里：{name}"));
        return;
    }
    let slot = ui.memfuse_result.clone();
    let target = name.clone();
    std::thread::spawn(move || {
        let (ok, detail) = crate::memfuse::add_to_protect_list(&script, &target);
        if let Ok(mut g) = slot.lock() {
            *g = Some((ok, target, detail));
        }
    });
    set_event(ui, &format!("正在加入白名单…{name}"));
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

// 浮层卡片配色（COLORREF 是 0x00BBGGRR）。
const TOAST_BG: COLORREF = 0x00F7FBF5; // 极浅绿
const TOAST_LINE: COLORREF = 0x0050AF4C; // 绿（和「桌面」列的勾同一套语义）
const TOAST_TITLE_COLOR: COLORREF = 0x00182818;
const TOAST_BODY_COLOR: COLORREF = 0x00606060;

/// 从系统 UI 字体派生一个变体（只改字号 / 粗细）—— 不硬编码字体名，
/// 中文与高 DPI 都跟系统走（与 `winx::system_ui_font` 同一思路）。
unsafe fn derive_font(base: HFONT, height: i32, bold: bool) -> HFONT {
    if base.is_null() {
        return std::ptr::null_mut();
    }
    let mut lf: LOGFONTW = std::mem::zeroed();
    let got = GetObjectW(
        base as HGDIOBJ,
        std::mem::size_of::<LOGFONTW>() as i32,
        &mut lf as *mut LOGFONTW as *mut c_void,
    );
    if got == 0 {
        return std::ptr::null_mut();
    }
    lf.lfHeight = -height; // 负值 = 字符高度（不含内部行距）
    lf.lfWeight = (if bold { FW_BOLD } else { FW_NORMAL }) as i32;
    CreateFontIndirectW(&lf)
}

/// 画浮层卡片（STATIC + SS_OWNERDRAW → 父窗口的 WM_DRAWITEM 里回调）。
unsafe fn draw_toast(ui: &Ui, dis: &DRAWITEMSTRUCT) {
    let hdc = dis.hDC;
    let rc = dis.rcItem;
    let s = winx::dpi_for_window(ui.hwnd) as f32 / 96.0;
    let pad = (16.0 * s) as i32;

    // 1) 卡片底：浅绿圆角 + 绿描边
    let bg = CreateSolidBrush(TOAST_BG);
    let line = CreatePen(PS_SOLID, (1.4 * s).max(1.0) as i32, TOAST_LINE);
    let old_brush = SelectObject(hdc, bg as HGDIOBJ);
    let old_pen = SelectObject(hdc, line as HGDIOBJ);
    let rad = (16.0 * s) as i32;
    RoundRect(hdc, rc.left, rc.top, rc.right - 1, rc.bottom - 1, rad, rad);

    // 2) 左侧绿圆 + 白勾（画线而不是字体符号：不依赖字体里有没有 ✓）
    let d = (38.0 * s) as i32;
    let cx = rc.left + pad + d / 2;
    let cy = (rc.top + rc.bottom) / 2;
    let green = CreateSolidBrush(TOAST_LINE);
    SelectObject(hdc, green as HGDIOBJ);
    SelectObject(hdc, GetStockObject(NULL_PEN));
    Ellipse(hdc, cx - d / 2, cy - d / 2, cx + d / 2, cy + d / 2);

    let white = CreatePen(PS_SOLID, (2.6 * s).max(2.0) as i32, 0x00FFFFFF);
    SelectObject(hdc, white as HGDIOBJ);
    let q = d / 4;
    MoveToEx(hdc, cx - q, cy, std::ptr::null_mut());
    LineTo(hdc, cx - q / 3, cy + q);
    LineTo(hdc, cx + q + q / 3, cy - q);

    // 3) 文案：主行（粗）+ 副行（文件 ── 方位）
    SetBkMode(hdc, TRANSPARENT as i32);
    let tx = cx + d / 2 + (12.0 * s) as i32;

    // 两行一次画完（`\n` 分隔、不传 DT_SINGLELINE 即多行）：
    // 之前分成两次调用时，只有第二行显示出来 —— 单次调用把"分区/裁剪"这类变量全消掉。
    let mut text = ui.toast_title.clone();
    text.push('\n');
    text.push_str(&ui.toast_body);
    let tb = winx::wide(&text);
    let f_body = derive_font(ui.font, (14.0 * s) as i32, false);
    let old_font = SelectObject(hdc, f_body as HGDIOBJ);
    SetTextColor(hdc, TOAST_TITLE_COLOR);
    let mut rb = RECT {
        left: tx,
        top: rc.top + pad,
        right: rc.right - pad,
        bottom: rc.bottom - pad,
    };
    let drawn = DrawTextW(
        hdc,
        tb.as_ptr(),
        -1,
        &mut rb,
        DT_LEFT | DT_NOPREFIX | DT_END_ELLIPSIS,
    );
    log::info!(
        "toast 文案绘制: rc=({},{},{},{}) 字体空={} 返回高={drawn}",
        rb.left,
        rb.top,
        rb.right,
        rb.bottom,
        f_body.is_null()
    );
    SelectObject(hdc, old_font);
    if !f_body.is_null() {
        DeleteObject(f_body as HGDIOBJ);
    }

    // 4) 还原并释放 GDI 对象（顺序：先还原再删，避免用着已删的句柄）
    SelectObject(hdc, old_brush);
    SelectObject(hdc, old_pen);
    DeleteObject(bg as HGDIOBJ);
    DeleteObject(line as HGDIOBJ);
    DeleteObject(green as HGDIOBJ);
    DeleteObject(white as HGDIOBJ);
}

/// 弹出浮层卡片（钉桌面成功的强反馈）。`beep=true` 时附一声系统提示音。
///
/// 允许被调用两次：第一次在按钮回调里立即弹（不含方位），拿到方位后再刷一次文字 ——
/// 这样"瞬间反馈"与"方位需要等 Explorer 刷新"两件事不用互相迁就。
unsafe fn show_toast(ui: &mut Ui, title: &str, body: &str, beep: bool) {
    ui.toast_title = title.to_string();
    ui.toast_body = body.to_string();
    ui.toast_at = store::unix_now();
    if ui.toast.is_null() {
        return;
    }
    place_toast(ui); // 先按当前窗口定好位，再显示
    SetWindowPos(
        ui.toast,
        HWND_TOP,
        0,
        0,
        0,
        0,
        SWP_NOMOVE | SWP_NOSIZE | SWP_NOACTIVATE | SWP_SHOWWINDOW,
    );
    InvalidateRect(ui.toast, std::ptr::null(), 1);
    if beep {
        success_beep();
    }
}

/// 成功提示音。
///
/// windows-sys 0.61 没有导出 `MessageBeep`（源码里搜不到），所以从 user32 动态取一次：
/// user32 本来就在本进程里，`GetProcAddress` 只是一次符号查找，无额外依赖、无体积代价。
fn success_beep() {
    use windows_sys::Win32::System::LibraryLoader::{GetModuleHandleW, GetProcAddress};
    unsafe {
        let dll = winx::wide("user32.dll");
        let hmod = GetModuleHandleW(dll.as_ptr());
        if hmod.is_null() {
            return;
        }
        let sym = b"MessageBeep\0";
        if let Some(f) = GetProcAddress(hmod, sym.as_ptr()) {
            let beep: unsafe extern "system" fn(u32) -> i32 = std::mem::transmute(f);
            beep(0x0000_0040); // MB_ICONASTERISK：系统星号提示音，不引音频资源
        }
    }
}

/// 钉完之后再读一次桌面图标坐标，找出"多出来的那个" → 它在桌面哪个方位（八区）。
///
/// 为什么用"快照 diff"而不是按序号推算：桌面图标会重排（自动排列会填到第一个空位），
/// 且很容易超出一屏 —— 实测本机 91 个图标、一屏只放得下 36 个。diff 只依赖"谁是新来的"。
/// 读不到坐标（拿不到 ListView / 权限不足 / Explorer 忙）时返回 None，调用方退化为不提方位。
fn new_icon_zone(before: Option<&[(i32, i32)]>) -> Option<&'static str> {
    // 必须重试：Explorer 刷新桌面图标列表有延迟，快捷方式落盘后**立刻**读往往还是旧列表
    // （数量没变 → diff 为空 → 只能退化成"落在桌面图标区"）。实测加到这里才拿到方位。
    // 总等待 ≈ 0.6s，用户刚点完按钮，这段短暂停顿无感；真要更久就该改成异步补写了。
    for attempt in 0..4 {
        if attempt > 0 {
            std::thread::sleep(std::time::Duration::from_millis(200));
        }
        let Some(after) = winx::desktop_icon_positions() else {
            continue;
        };
        let fresh = match before {
            Some(old) if after.len() > old.len() => {
                let old_set: std::collections::HashSet<(i32, i32)> = old.iter().copied().collect();
                after.iter().copied().find(|p| !old_set.contains(p))
            }
            // 建前快照失败：退一步用最后一个（刚加进去的通常排在那里）
            None => after.last().copied(),
            _ => None,
        };
        if let Some(p) = fresh {
            return Some(winx::zone_of(p.0, p.1, winx::primary_work_area()));
        }
    }
    None
}

/// 把某个路径钉到桌面：创建 + 立刻读回校验 + 标记「桌面」列 + 强反馈（卡片 + 方位）。
unsafe fn act_pin(ui: &mut Ui, target: &str) {
    let name = store::file_stem(target);
    // 建前快照：给"新图标落哪儿"留基线
    let before = winx::desktop_icon_positions();
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

            // 强反馈：先**立即**弹卡片（含提示音），再补方位 ——
            // 方位要等 Explorer 刷新图标列表（最多重试 ~0.6s），不值得让"瞬间反馈"等它。
            let head = format!("{base}{suffix}");
            show_toast(ui, "已创建桌面快捷方式", &head, true);
            let body = match new_icon_zone(before.as_deref()) {
                Some(zone) => format!("{head} · 在桌面{zone}"),
                None => format!("{head} · 落在桌面图标区"),
            };
            show_toast(ui, "已创建桌面快捷方式", &body, false);
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
    let p = std::path::Path::new(path);
    if p.is_file() {
        winx::launch_path(path);
        let name = store::file_stem(path);
        set_event(ui, &format!("已启动：{name}"));
    } else if p.is_dir() {
        // 项目行：双击 = 打开项目文件夹（「管理项目入口」的基本动作）
        winx::launch_path(path);
        let name = p
            .file_name()
            .map(|s| s.to_string_lossy().to_string())
            .unwrap_or_else(|| path.to_string());
        set_event(ui, &format!("已打开：{name}"));
    } else {
        winx::message_box_on(ui.hwnd, &format!("文件已不存在：\n{path}"));
    }
}

unsafe fn show_context_menu(ui: &mut Ui) {
    let Some(selected) = selected_path(ui) else { return };
    let is_project = ui
        .rows
        .iter()
        .find(|r| r.path == selected)
        .map(|r| r.source == "project")
        .unwrap_or(false);
    let hmenu = CreatePopupMenu();
    if hmenu.is_null() {
        return;
    }
    let paused_now = ui.paused.load(Ordering::Relaxed);
    let pause_label = if paused_now { "恢复记录" } else { "暂停记录" };
    // (id, 文案, 是否在该项前加分隔线)
    let items: Vec<(i32, String, bool)> = if is_project {
        vec![
            (ID_MENU_LAUNCH, "打开文件夹".to_string(), false),
            (ID_MENU_REVEAL, "打开所在文件夹".to_string(), false),
            (ID_MENU_COPY, "复制完整路径".to_string(), false),
            (ID_MENU_PIN, "创建桌面快捷方式".to_string(), false),
            (ID_MENU_GH, "打开 GitHub 页".to_string(), true),
            (ID_MENU_UPDATE, "更新（git pull）".to_string(), false),
            (ID_MENU_REMOVE, "从历史中移除".to_string(), true),
            (ID_MENU_PRUNE, "清理失效记录".to_string(), false),
            (ID_MENU_RESCAN, "重新扫描项目".to_string(), false),
            (ID_MENU_PAUSE, pause_label.to_string(), true),
            (ID_MENU_HELP, "帮助 (F1)".to_string(), false),
        ]
    } else {
        vec![
            (ID_MENU_LAUNCH, "启动".to_string(), false),
            (ID_MENU_REVEAL, "打开所在文件夹".to_string(), false),
            (ID_MENU_COPY, "复制完整路径".to_string(), false),
            (ID_MENU_PIN, "创建桌面快捷方式".to_string(), false),
            (ID_MENU_WHITELIST, "加入 memfuse 白名单".to_string(), false),
            (ID_MENU_REMOVE, "从历史中移除".to_string(), true),
            (ID_MENU_PRUNE, "清理失效记录".to_string(), false),
            (ID_MENU_RESCAN, "重新扫描项目".to_string(), false),
            (ID_MENU_PAUSE, pause_label.to_string(), true),
            (ID_MENU_HELP, "帮助 (F1)".to_string(), false),
        ]
    };
    for (id, label, sep) in &items {
        if *sep {
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
        if ui.hidden_running > 0 {
            format!(
                "没有可显示的应用 —— {} 个正在运行的应用已自动隐藏（关掉它们即恢复）",
                ui.hidden_running
            )
        } else if ui.view_idx == 2 {
            "没有扫描到项目 —— 右键菜单里可以「重新扫描项目」".to_string()
        } else {
            "没有匹配的应用（清空搜索框可看全部）".to_string()
        }
    } else {
        "后台记录中（关闭窗口 = 缩到托盘继续记录 · F1 查看帮助）".to_string()
    };
    let hidden_note = if ui.hidden_running > 0 {
        format!(" · 已隐藏 {} 个运行中", ui.hidden_running)
    } else {
        String::new()
    };
    // 网页数只在真的有记录时才显示，避免「0 个网页」占位置。
    let html_part = if ui.html > 0 {
        format!(" / {} 个网页", ui.html)
    } else {
        String::new()
    };
    let right = if ui.days > 0 {
        format!(
            "已收录 {} 个应用 / {} 个项目{} · 近 {} 天 {} 条{} · 今日打开 {}",
            ui.total,
            ui.projects,
            html_part,
            ui.days,
            ui.rows.len(),
            hidden_note,
            ui.today
        )
    } else {
        format!(
            "已收录 {} 个应用 / {} 个项目{}{} · 今日打开 {}",
            ui.total, ui.projects, html_part, hidden_note, ui.today
        )
    };
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
