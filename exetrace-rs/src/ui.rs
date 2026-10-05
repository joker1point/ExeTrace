//! 主界面（egui）—— 虚拟滚动表格 / 搜索 / 排序 / 图标 / 托盘。
//!
//! 线程模型：watcher 线程只写 store 并投递"最近事件"；
//! UI 每 POLL 检查 store.rev()，变化才重查（不做无谓重建）。

use std::collections::{HashMap, HashSet};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use chrono::Datelike;
use eframe::egui;
use egui_extras::{Column, TableBuilder};
use std::sync::atomic::{AtomicBool, Ordering};

use crate::appicon;
use crate::store::{AppRow, Store};
use crate::winx;

const POLL: Duration = Duration::from_millis(500);
const ROW_H: f32 = 30.0;
const FONT_CANDIDATES: [&str; 4] = [
    r"C:\Windows\Fonts\msyh.ttc",
    r"C:\Windows\Fonts\simhei.ttf",
    r"C:\Windows\Fonts\simsun.ttc",
    r"C:\Windows\Fonts\Deng.ttf",
];

#[derive(Clone, Copy, PartialEq, Eq)]
enum SortKey {
    Recent,
    Count,
    Name,
}

impl SortKey {
    fn label(self) -> &'static str {
        match self {
            Self::Recent => "最近打开",
            Self::Count => "打开次数",
            Self::Name => "名称",
        }
    }
    fn as_str(self) -> &'static str {
        match self {
            Self::Recent => "recent",
            Self::Count => "count",
            Self::Name => "name",
        }
    }
}

pub type SharedStore = Arc<Mutex<Store>>;
pub type LastEvent = Arc<Mutex<Option<(String, bool)>>>;

pub fn run(store: SharedStore, last_event: LastEvent, minimized: bool, tray_enabled: bool) -> Result<(), String> {
    let options = eframe::NativeOptions {
        viewport: egui::ViewportBuilder::default()
            .with_title(crate::paths::APP_TITLE)
            .with_inner_size([1120.0, 740.0])
            .with_min_inner_size([820.0, 520.0]),
        ..Default::default()
    };
    eframe::run_native(
        "ExeTrace",
        options,
        Box::new(move |cc| {
            let app = App::new(cc, store, last_event, minimized, tray_enabled);
            Ok(Box::new(app))
        }),
    )
    .map_err(|e| e.to_string())
}

struct App {
    store: SharedStore,
    last_event: LastEvent,
    rows: Vec<AppRow>,
    last_rev: u64,
    last_poll: Instant,
    keyword: String,
    sort: SortKey,
    selected: Option<String>,
    icons: HashMap<String, egui::TextureHandle>,
    icon_attempted: HashSet<String>,
    exists_cache: HashMap<String, bool>,
    total: i64,
    today: i64,
    event_text: String,
    event_at: Option<Instant>,
    tray: Option<winx::TrayHandle>,
    tray_rx: Option<std::sync::mpsc::Receiver<winx::TrayEvent>>,
    autostart_flag: Arc<AtomicBool>,
    autostart: bool,
    quitting: bool,
    start_hidden_pending: bool,
    tray_hint_shown: bool,
}

impl App {
    fn new(
        cc: &eframe::CreationContext<'_>,
        store: SharedStore,
        last_event: LastEvent,
        minimized: bool,
        tray_enabled: bool,
    ) -> Self {
        install_fonts(&cc.egui_ctx);
        install_style(&cc.egui_ctx);
        let (tray_tx, tray_rx) = std::sync::mpsc::channel::<winx::TrayEvent>();
        let autostart = winx::is_autostart_enabled();
        let autostart_flag = Arc::new(AtomicBool::new(autostart));
        let tray = if tray_enabled {
            winx::start_tray(
                crate::paths::APP_TITLE,
                appicon::make_icon_rgba(32),
                autostart_flag.clone(),
                tray_tx,
            )
        } else {
            None
        };
        let mut app = Self {
            store,
            last_event,
            rows: Vec::new(),
            last_rev: 0,
            last_poll: Instant::now(),
            keyword: String::new(),
            sort: SortKey::Recent,
            selected: None,
            icons: HashMap::new(),
            icon_attempted: HashSet::new(),
            exists_cache: HashMap::new(),
            total: 0,
            today: 0,
            event_text: "历史扫描进行中…".to_string(),
            event_at: Some(Instant::now()),
            tray,
            tray_rx: Some(tray_rx),
            autostart_flag,
            autostart,
            quitting: false,
            start_hidden_pending: minimized,
            tray_hint_shown: false,
        };
        app.refresh();
        app
    }

    fn refresh(&mut self) {
        let (rows, total, today, rev) = {
            let Ok(st) = self.store.lock() else { return };
            (
                st.query(&self.keyword, self.sort.as_str(), 2000).unwrap_or_default(),
                st.total(),
                st.count_today(),
                st.rev(),
            )
        };
        self.rows = rows;
        self.total = total;
        self.today = today;
        self.last_rev = rev;
    }

    fn path_exists(&mut self, path: &str) -> bool {
        if let Some(v) = self.exists_cache.get(path) {
            return *v;
        }
        let v = std::path::Path::new(path).is_file();
        self.exists_cache.insert(path.to_string(), v);
        v
    }

    fn icon_for(&mut self, ctx: &egui::Context, path: &str) -> Option<egui::TextureHandle> {
        if let Some(h) = self.icons.get(path) {
            return Some(h.clone());
        }
        if self.icon_attempted.contains(path) {
            return None;
        }
        self.icon_attempted.insert(path.to_string());
        if !std::path::Path::new(path).is_file() {
            return None;
        }
        let size = (18.0 * ctx.pixels_per_point()).clamp(16.0, 64.0) as i32;
        if let Some((w, h, rgba)) = winx::extract_icon_rgba(path, size) {
            let img = egui::ColorImage::from_rgba_unmultiplied([w, h], &rgba);
            let handle = ctx.load_texture(format!("icon:{path}"), img, egui::TextureOptions::LINEAR);
            self.icons.insert(path.to_string(), handle.clone());
            return Some(handle);
        }
        None
    }

    fn set_event(&mut self, text: impl Into<String>) {
        self.event_text = text.into();
        self.event_at = Some(Instant::now());
    }

    fn selected_path(&self) -> Option<String> {
        self.selected.clone()
    }

    fn launch_selected(&mut self) {
        let Some(path) = self.selected_path() else { return };
        if !std::path::Path::new(&path).is_file() {
            self.set_event(format!("文件已不存在：{}", short(&path, 60)));
            return;
        }
        if winx::launch_path(&path) {
            self.set_event(format!("已启动：{}", file_name(&path)));
        } else {
            self.set_event("启动失败（ShellExecute 返回错误）");
        }
    }

    fn reveal_selected(&mut self) {
        if let Some(path) = self.selected_path() {
            winx::reveal_in_explorer(&path);
            self.set_event("已在资源管理器中定位");
        }
    }

    fn copy_selected(&mut self, ctx: &egui::Context, full_path: bool) {
        if let Some(path) = self.selected_path() {
            let text = if full_path { path.clone() } else { file_name(&path) };
            ctx.copy_text(text);
            self.set_event("已复制到剪贴板");
        }
    }

    fn remove_selected(&mut self) {
        if let Some(path) = self.selected_path() {
            if let Ok(mut st) = self.store.lock() {
                let _ = st.remove(&path);
            }
            self.exists_cache.remove(&path);
            self.selected = None;
            self.refresh();
            self.set_event("已从历史中移除");
        }
    }

    fn prune_missing(&mut self) {
        let removed = self
            .store
            .lock()
            .map(|mut st| st.prune_missing().unwrap_or(0))
            .unwrap_or(0);
        self.exists_cache.clear();
        self.refresh();
        if removed > 0 {
            self.set_event(format!("已清理 {removed} 条失效记录"));
        } else {
            self.set_event("没有失效记录");
        }
    }

    fn toggle_autostart(&mut self) {
        self.autostart = winx::set_autostart(!self.autostart);
        self.autostart_flag.store(self.autostart, Ordering::Relaxed);
        self.set_event(if self.autostart { "已开启开机自启" } else { "已关闭开机自启" });
    }

    fn handle_tray_events(&mut self, ctx: &egui::Context) {
        let Some(rx) = &self.tray_rx else { return };
        let mut events = Vec::new();
        while let Ok(ev) = rx.try_recv() {
            events.push(ev);
        }
        for ev in events {
            match ev {
                winx::TrayEvent::Show => {
                    ctx.send_viewport_cmd(egui::ViewportCommand::Visible(true));
                    ctx.send_viewport_cmd(egui::ViewportCommand::Focus);
                }
                winx::TrayEvent::ToggleAutostart => self.toggle_autostart(),
                winx::TrayEvent::Quit => {
                    self.quitting = true;
                    ctx.send_viewport_cmd(egui::ViewportCommand::Close);
                }
            }
        }
    }

    fn poll_store(&mut self) {
        if self.last_poll.elapsed() < POLL {
            return;
        }
        self.last_poll = Instant::now();
        let rev = self.store.lock().map(|st| st.rev()).unwrap_or(self.last_rev);
        if rev != self.last_rev {
            self.refresh();
        }
        let ev = self.last_event.lock().ok().and_then(|mut e| e.take());
        if let Some((exe, counted)) = ev {
            if counted {
                self.set_event(format!("刚刚打开：{}", file_name(&exe)));
            }
        }
    }

    fn top_bar(&mut self, ui: &mut egui::Ui) {
        ui.horizontal(|ui| {
            ui.add_space(2.0);
            let search = ui.add(
                egui::TextEdit::singleline(&mut self.keyword)
                    .hint_text("搜索应用名或路径…")
                    .desired_width(360.0),
            );
            if search.changed() {
                self.refresh();
            }
            ui.add_space(12.0);
            egui::ComboBox::from_id_salt("sort_combo")
                .selected_text(self.sort.label())
                .width(110.0)
                .show_ui(ui, |ui| {
                    for key in [SortKey::Recent, SortKey::Count, SortKey::Name] {
                        if ui.selectable_label(self.sort == key, key.label()).clicked() && self.sort != key {
                            self.sort = key;
                            self.refresh();
                        }
                    }
                });
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                ui.label(
                    egui::RichText::new(format!("显示 {} / {}", self.rows.len(), self.total))
                        .weak()
                        .size(13.0),
                );
            });
        });
    }

    fn bottom_bar(&mut self, ctx: &egui::Context, ui: &mut egui::Ui) {
        ui.horizontal(|ui| {
            let has = self.selected.is_some();
            if ui.add_enabled(has, egui::Button::new("启动")).clicked() {
                self.launch_selected();
            }
            if ui.add_enabled(has, egui::Button::new("打开所在文件夹")).clicked() {
                self.reveal_selected();
            }
            if ui.add_enabled(has, egui::Button::new("复制路径")).clicked() {
                self.copy_selected(ctx, true);
            }
            if ui.add_enabled(has, egui::Button::new("移除记录")).clicked() {
                self.remove_selected();
            }
            ui.add_space(10.0);
            if ui.button("清理失效记录").clicked() {
                self.prune_missing();
            }
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                let mut auto = self.autostart;
                if ui.checkbox(&mut auto, "开机自动启动").clicked() {
                    self.toggle_autostart();
                }
                ui.label(
                    egui::RichText::new(format!("已收录 {} 个应用 · 今日打开 {}", self.total, self.today))
                        .weak()
                        .size(13.0),
                );
            });
        });
    }

    fn table(&mut self, ctx: &egui::Context, ui: &mut egui::Ui) {
        if self.rows.is_empty() {
            ui.vertical_centered(|ui| {
                ui.add_space(60.0);
                ui.label(egui::RichText::new("还没有记录").size(18.0).weak());
                ui.label(egui::RichText::new("打开任意一个软件后，它就会出现在这里").weak());
            });
            return;
        }

        // 先收集需要的信息（避免在闭包里同时借用 self 的可变与不可变）
        let rows_snapshot: Vec<(String, String, i64, Option<f64>)> = self
            .rows
            .iter()
            .map(|r| (r.path.clone(), r.name.clone(), r.launch_count, r.last_seen))
            .collect();
        let mut exists_flags: Vec<bool> = Vec::with_capacity(rows_snapshot.len());
        for (path, _, _, _) in &rows_snapshot {
            exists_flags.push(self.path_exists(path));
        }
        let mut icons: Vec<Option<egui::TextureHandle>> = Vec::with_capacity(rows_snapshot.len());
        for (path, _, _, _) in &rows_snapshot {
            icons.push(self.icon_for(ctx, path));
        }
        let mut clicked: Option<String> = None;
        let mut double_clicked: Option<String> = None;
        let mut context_for: Option<String> = None;

        TableBuilder::new(ui)
            .striped(true)
            .cell_layout(egui::Layout::left_to_right(egui::Align::Center))
            .column(Column::exact(30.0))
            .column(Column::initial(220.0).at_least(140.0).clip(true))
            .column(Column::remainder().at_least(200.0).clip(true))
            .column(Column::exact(88.0))
            .column(Column::exact(140.0))
            .header(28.0, |mut header| {
                header.col(|ui| {
                    ui.label("");
                });
                header.col(|ui| {
                    ui.strong("应用");
                });
                header.col(|ui| {
                    ui.strong("位置");
                });
                header.col(|ui| {
                    ui.strong("次数");
                });
                header.col(|ui| {
                    ui.strong("最后打开");
                });
            })
            .body(|body| {
                body.rows(ROW_H, rows_snapshot.len(), |mut row| {
                    let idx = row.index();
                    let (path, name, count, last_seen) = &rows_snapshot[idx];
                    let exists = exists_flags[idx];

                    row.col(|ui| {
                        if let Some(tex) = &icons[idx] {
                            ui.add(egui::Image::new(tex).fit_to_exact_size(egui::vec2(18.0, 18.0)));
                        }
                    });
                    row.col(|ui| {
                        let mut text = egui::RichText::new(name);
                        if !exists {
                            text = text.weak();
                        }
                        let label = ui.add(egui::Label::new(text).truncate());
                        if !exists {
                            label.on_hover_text("文件已丢失");
                        }
                    });
                    row.col(|ui| {
                        ui.add(egui::Label::new(egui::RichText::new(path).weak().size(13.0)).truncate())
                            .on_hover_text(path);
                    });
                    row.col(|ui| {
                        ui.label(if *count > 0 { count.to_string() } else { "—".to_string() });
                    });
                    row.col(|ui| {
                        ui.label(human_time(*last_seen));
                    });

                    let resp = row.response();
                    if resp.clicked() {
                        clicked = Some(path.clone());
                    }
                    if resp.double_clicked() {
                        double_clicked = Some(path.clone());
                    }
                    resp.context_menu(|ui| {
                        if ui.button("启动").clicked() {
                            double_clicked = Some(path.clone());
                            ui.close();
                        }
                        if ui.button("打开所在文件夹").clicked() {
                            winx::reveal_in_explorer(path);
                            ui.close();
                        }
                        if ui.button("复制完整路径").clicked() {
                            ctx.copy_text(path.clone());
                            ui.close();
                        }
                        if ui.button("从历史中移除").clicked() {
                            context_for = Some(path.clone());
                            ui.close();
                        }
                    });
                });
            });

        if let Some(p) = clicked {
            self.selected = Some(p);
        }
        if let Some(p) = double_clicked {
            self.selected = Some(p);
            self.launch_selected();
        }
        if let Some(p) = context_for {
            self.selected = Some(p);
            self.remove_selected();
        }
    }
}

impl eframe::App for App {
    /// 每帧先执行（窗口隐藏时也会被调用）：托盘事件、数据轮询、视图命令。
    fn logic(&mut self, ctx: &egui::Context, _frame: &mut eframe::Frame) {
        self.handle_tray_events(ctx);
        self.poll_store();

        if self.start_hidden_pending {
            self.start_hidden_pending = false;
            ctx.send_viewport_cmd(egui::ViewportCommand::Visible(false));
        }

        // 关窗口 = 缩到托盘（有托盘时）
        if self.tray.is_some() && !self.quitting && ctx.input(|i| i.viewport().close_requested()) {
            ctx.send_viewport_cmd(egui::ViewportCommand::CancelClose);
            ctx.send_viewport_cmd(egui::ViewportCommand::Visible(false));
            self.tray_hint_shown = true;
        }

        // 隐藏状态下不会有常规重绘，必须主动请求，logic 才会被周期性调用
        ctx.request_repaint_after(POLL);
    }

    fn ui(&mut self, ui: &mut egui::Ui, _frame: &mut eframe::Frame) {
        let ctx = ui.ctx().clone();

        egui::Panel::top(egui::Id::new("et_top")).show(ui, |ui| {
            ui.add_space(6.0);
            self.top_bar(ui);
            ui.add_space(6.0);
        });

        // 注意顺序：先声明的贴窗口最外缘 —— 状态栏在操作栏下方
        egui::Panel::bottom(egui::Id::new("et_status")).show(ui, |ui| {
            ui.horizontal(|ui| {
                let text = if self.rows.is_empty() && self.total == 0 {
                    "正在扫描系统历史记录…"
                } else {
                    "后台记录中（关闭窗口 = 缩到托盘继续记录）"
                };
                ui.label(egui::RichText::new(text).weak().size(13.0));
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    let mut ev = self.event_text.clone();
                    if let Some(at) = self.event_at {
                        if at.elapsed() > Duration::from_secs(8) {
                            ev.clear();
                        }
                    }
                    ui.label(egui::RichText::new(ev).weak().size(13.0));
                });
            });
        });

        egui::Panel::bottom(egui::Id::new("et_actions")).show(ui, |ui| {
            ui.add_space(6.0);
            self.bottom_bar(&ctx, ui);
            ui.add_space(6.0);
        });

        egui::CentralPanel::default().show(ui, |ui| {
            self.table(&ctx, ui);
        });

        if self.tray_hint_shown {
            self.tray_hint_shown = false;
            self.set_event("已最小化到托盘，仍在后台记录；右键托盘图标可退出");
        }
    }
}

fn install_fonts(ctx: &egui::Context) {
    let mut fonts = egui::FontDefinitions::default();
    for path in FONT_CANDIDATES {
        if let Ok(bytes) = std::fs::read(path) {
            fonts
                .font_data
                .insert("cjk".to_owned(), egui::FontData::from_owned(bytes).into());
            fonts
                .families
                .entry(egui::FontFamily::Proportional)
                .or_default()
                .insert(0, "cjk".to_owned());
            fonts
                .families
                .entry(egui::FontFamily::Monospace)
                .or_default()
                .push("cjk".to_owned());
            ctx.set_fonts(fonts);
            return;
        }
    }
    log::warn!("未找到可用的中文字体，将使用默认字体（中文可能显示为方块）");
}

fn install_style(ctx: &egui::Context) {
    ctx.all_styles_mut(|style| {
        style.text_styles = [
            (egui::TextStyle::Heading, egui::FontId::new(20.0, egui::FontFamily::Proportional)),
            (egui::TextStyle::Body, egui::FontId::new(15.5, egui::FontFamily::Proportional)),
            (egui::TextStyle::Button, egui::FontId::new(15.5, egui::FontFamily::Proportional)),
            (egui::TextStyle::Small, egui::FontId::new(13.0, egui::FontFamily::Proportional)),
            (egui::TextStyle::Monospace, egui::FontId::new(14.5, egui::FontFamily::Monospace)),
        ]
        .into();
        style.spacing.item_spacing = egui::vec2(8.0, 6.0);
        style.spacing.button_padding = egui::vec2(10.0, 5.0);
    });
}

fn human_time(ts: Option<f64>) -> String {
    let Some(ts) = ts.filter(|t| *t > 0.0) else {
        return "—".to_string();
    };
    let now = crate::store::unix_now();
    let delta = now - ts;
    if delta < 60.0 {
        return "刚刚".to_string();
    }
    if delta < 3600.0 {
        return format!("{} 分钟前", (delta / 60.0) as i64);
    }
    use chrono::{Local, TimeZone};
    let Some(dt) = Local.timestamp_opt(ts as i64, 0).single() else {
        return "—".to_string();
    };
    let today = Local::now().date_naive();
    let date = dt.date_naive();
    if date == today {
        format!("今天 {}", dt.format("%H:%M"))
    } else if (today - date).num_days() == 1 {
        format!("昨天 {}", dt.format("%H:%M"))
    } else if Datelike::year(&date) == Datelike::year(&today) {
        dt.format("%m-%d %H:%M").to_string()
    } else {
        dt.format("%Y-%m-%d").to_string()
    }
}

fn file_name(path: &str) -> String {
    std::path::Path::new(path)
        .file_name()
        .map(|s| s.to_string_lossy().to_string())
        .unwrap_or_else(|| path.to_string())
}

fn short(text: &str, max: usize) -> String {
    if text.chars().count() <= max {
        text.to_string()
    } else {
        let tail: String = text.chars().skip(text.chars().count() - max / 2).collect();
        format!("…{tail}")
    }
}
