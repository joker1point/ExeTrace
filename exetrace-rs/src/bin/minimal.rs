//! 最小 egui 应用（内存对照实验用）。
//!
//!   cargo run --release --bin minimal            无中文字体
//!   cargo run --release --bin minimal -- --font  加载 msyh.ttc
//!
//! 用途：分离「egui 框架基线内存」与「应用自身的内存开销」。

use eframe::egui;

struct App;

impl eframe::App for App {
    fn ui(&mut self, ui: &mut egui::Ui, _frame: &mut eframe::Frame) {
        ui.label("hello");
    }
}

fn main() -> eframe::Result {
    let with_font = std::env::args().any(|a| a == "--font");
    let options = eframe::NativeOptions {
        viewport: egui::ViewportBuilder::default().with_inner_size([1120.0, 740.0]),
        ..Default::default()
    };
    eframe::run_native(
        "minimal",
        options,
        Box::new(move |cc| {
            if std::env::args().any(|a| a == "--zoom05") {
                cc.egui_ctx.set_zoom_factor(0.5);
            }
            if with_font {
                let mut fonts = egui::FontDefinitions::default();
                if let Ok(bytes) = std::fs::read(r"C:\Windows\Fonts\msyh.ttc") {
                    fonts
                        .font_data
                        .insert("cjk".to_owned(), egui::FontData::from_owned(bytes).into());
                    fonts
                        .families
                        .entry(egui::FontFamily::Proportional)
                        .or_default()
                        .insert(0, "cjk".to_owned());
                }
                cc.egui_ctx.set_fonts(fonts);
            }
            Ok(Box::new(App))
        }),
    )
}
