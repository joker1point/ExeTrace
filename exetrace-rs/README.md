# ExeTrace — Rust / egui 实验版

与仓库根目录 Python/tkinter 版**功能对齐**的重写实现（选型背景见根 README「两个实现」表）。

## 与 Python 版的关系

- **共用同一 SQLite 库**（`%LOCALAPPDATA%\ExeTrace\history.db`，schema 一致）——数据互通
- **共用同一单实例互斥体**（`Local\ExeTrace_SingleInstance_9F3C`）——两版**不能同时运行**（避免同一动作双重记录）
- 副作用：Python 版正常驻时，本版自检的 `single_instance` 项会 FAIL（互斥体被占用，属预期；其余项不受影响）

## 构建（Windows + GNU 工具链）

本项目在**没有 MSVC / Windows SDK** 的机器上开发，走 mingw-w64（GNU）工具链。踩过的三个坑：

1. **host 工具链也必须是 GNU**——build script / proc-macro 用 host 编译，只加 GNU target 不够；`rust-toolchain.toml` 已固定 `stable-x86_64-pc-windows-gnu`
2. **mingw 的 bin 必须在 PATH 最前**——否则 rustc 可能捡到旧版 32 位 `dlltool`，链接 x86-64 导入库时报错（`build.ps1` 已自动前置）
3. **linker 路径是机器相关的**——`.cargo/config.toml` 里写的是本机路径，**克隆后请改成你的**

准备：

```powershell
# 1) 安装 mingw-w64（如 https://winlibs.com/ 的 UCRT 版），解压到任意目录
# 2) GNU 工具链（rust-toolchain.toml 已声明，rustup 会自动装）
rustup toolchain install stable-x86_64-pc-windows-gnu
# 3) 修改 .cargo/config.toml 的 linker / ar 路径（crates.io 镜像已配 rsproxy，海外可改回官方源）
```

构建与验收（`build.ps1` = cargo build → 产物自检 → GUI 冒烟）：

```powershell
.\build.ps1        # → dist\ExeTrace.exe
```

## 已知结论（实测）

- **体积**：exe 6.8 MB（Python 版 20.4 MB）；冷启动更快
- **内存**：约 120 MB——最小 egui 空壳（一个 label）即 115.6 MB（glow 后端、200% DPI），**这是 eframe/egui 的框架基线**，不是应用代码开销（应用层约 7 MB）；跨版本对照 0.31 = 122.4 MB、wgpu 后端 = 321.5 MB，均已否决
- **LTO**：fat LTO 在内存紧张的机器上会被系统终止；改用 thin LTO + `vello_cpu` 单独降优化等级
- **托盘**：不用 `tray-icon` crate（其与 winit 消息循环冲突会卡死监控线程）——自研独立线程 + `Shell_NotifyIconW`
- **监控**：ToolHelp 快照（`CreateToolhelp32Snapshot`）一次枚举 + 按需 `QueryFullProcessImageName`，稳态 CPU 约 1.9%（单核）
- **窗口枚举**：必须跳过自身 PID + `SendMessageTimeoutW` 超时（否则主线程忙时监控线程被自身窗口阻塞；Python 版同款坑）
