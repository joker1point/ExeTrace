//! 构建脚本：用 mingw 的 windres 把 app.ico 编成资源段，**让 exe 文件本身带上品牌图标**。
//!
//! 说明：`src/appicon.rs` 的那套绘制是**运行时**用的（窗口图标 / 托盘图标），
//! 而 exe 在资源管理器、任务栏、Alt+Tab 里显示的图标，必须是编译进 PE 资源段的 .ico，
//! 两者都需要，视觉保持一致（都来自 video_assets/_logo/logo.png 的几何）。

use std::path::PathBuf;
use std::process::Command;

fn main() {
    let rc = PathBuf::from("assets/app.rc");
    println!("cargo:rerun-if-changed=assets/app.rc");
    println!("cargo:rerun-if-changed=assets/app.ico");
    if !rc.exists() {
        println!("cargo:warning=缺少 assets/app.rc，exe 将没有自定义图标");
        return;
    }

    let out_dir = std::env::var("OUT_DIR").unwrap_or_else(|_| ".".into());
    let obj = PathBuf::from(out_dir).join("app_icon.o");
    // windres 来自 mingw-w64；build.ps1 会把它的 bin 前置到 PATH
    let windres = std::env::var("WINDRES").unwrap_or_else(|_| "windres".into());
    let result = Command::new(windres)
        .args(["-I", "assets", "-i", "assets/app.rc", "-O", "coff", "-o"])
        .arg(&obj)
        .status();

    match result {
        Ok(st) if st.success() => {
            println!("cargo:rustc-link-arg={}", obj.display());
        }
        other => println!(
            "cargo:warning=windres 调用失败（{other:?}）：exe 图标会用默认图标"
        ),
    }
}
