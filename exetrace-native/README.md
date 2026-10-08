# ExeTrace — Rust / Win32 原生控件版

> **[→ 功能演示：6 张真实操作动图（搜索 / 右键菜单 / 一键钉桌面 / 启动 / 托盘热键 / 帮助）](docs/feature-demo.md)**

用系统自带的原生控件（`SysListView32` / `Edit` / 菜单 / 托盘）重写的 ExeTrace——把「体积与内存」这条线做到头：**没有 UI 框架，没有 GPU 上下文，常驻不到 2 MB。**

## 实测数据（本机，Windows 11 + 200% 缩放）

| | Python / tkinter 版 | Rust / egui 版 | **本版（Rust / Win32 原生）** |
|---|---|---|---|
| exe 体积 | 20.42 MB | 6.8 MB | **1.61 MB** |
| 窗口打开 | 124 MB | 141 MB | **≈ 58 MB** |
| **托盘常驻** | 9.2 MB | 166 MB（不释放） | **≈ 2 MB** |

测法：`Get-Process` 的 `WorkingSet64`。常驻值在「关窗缩托盘」后 5 秒读取（本版缩托盘时会主动把空闲工作集还给系统）。

结论：egui/eframe 那条线的内存下限是**框架基线**（最小空壳即 115.6 MB，见 `../exetrace-rs/README.md`），
所以「Rust + 低内存」只能走原生控件这条路。

## 功能

与 Python 版对齐，并并入 v3.1 的能力：

- 后台记录打开过的应用（托管进程监控 + 前台窗口判据）
- 搜索实时过滤（名称 / 路径）· 智能排序（frecency）· 分类列 · 停留时长
- 双击 / 回车启动 · 右键菜单（启动 / 打开文件夹 / 复制路径 / 钉桌面 / 移除 / 清理失效 / 暂停 / 帮助）
- 一键钉到桌面（创建后**读回校验**；同名自动编号，绝不覆盖）；「钉任意程序…」支持选择 exe 或**软件文件夹**（自动识别主程序，跳过 setup/uninstall/helper）
- 托盘常驻 + 全局热键 `Ctrl+Alt+E` 呼出 + 开机自启（可开关）
- 缩托盘/隐藏时主动回收工作集（上面那张表里 2 MB 的来源）

完整交互说明与动图见 **[功能演示文档](docs/feature-demo.md)**。

## 与其它两个版本的关系

- **共用同一数据**：SQLite 库（`%LOCALAPPDATA%\ExeTrace\history.db`）与配置文件格式一致
- **共用同一单实例互斥体**：`Local\ExeTrace_SingleInstance_9F3C`——三个版本**不能同时运行**；
  自检里的 `single_instance` 项在别的版本常驻时会 FAIL（属预期）
- 本版**不含** UI 框架依赖；`../exetrace-rs/`（egui 版）原样保留做对照

## 构建（Windows + GNU 工具链）

和 egui 版同样的环境要求（无 MSVC / Windows SDK 时走 mingw-w64）：

```powershell
.\build.ps1        # cargo build --release → 自检 → 输出 dist\ExeTrace.exe
```

三个已知的坑（与 `../exetrace-rs/README.md` 相同）：
host 工具链也必须是 GNU；mingw 的 `bin` 要在 `PATH` 最前；`.cargo/config.toml` 里的 linker 路径是机器相关的，克隆后需改成你自己的。

## 演示动图怎么来的

`tools/demo_record.py`——不是截图拼帧，而是：

1. `SendInput` 注入真实键鼠事件（点击 / 输入 / 右键 / 热键）驱动真实界面；
2. `ffmpeg gdigrab` 录屏，两遍 palette 法转 GIF；
3. 每场景先重启进程、把窗口钉到屏幕左上角并置顶（否则录制期间别的窗口会挡住画面）；
4. 鼠标指针是**后期叠加**的：本机 `gdigrab -draw_mouse` 录不出光标，于是录制时
   记录注入过的指针坐标，转 GIF 时逐帧画一个带白描边的大号箭头（观众看得清点在哪里）。

```powershell
python tools/demo_record.py --list            # 场景列表
python tools/demo_record.py --scene 1-search  # 单个场景
python tools/demo_record.py --all             # 全部重录（约 3 分钟，期间会独占键鼠）
```

## 实现注记：owner-data 列表的"选中"必须自己做

`LVS_OWNERDATA` 虚拟列表**不会自己产生鼠标选中**——实测：点击能到达控件（`NM_CLICK` 会发），
但系统既不改选中状态、也不发 `LVN_ITEMCHANGED`，表现为"鼠标点行没反应"（键盘 ↑↓ 却是好的）。
且 `LVM_HITTEST` 在这类列表上恒返回 -1（342 项、坐标正确也不认）。

因此单击 / 双击 / 右键三处通知里都自己做「命中 + 选中」：命中行按**实测布局**换算
（行 0 矩形的 `top` 即表头高度、高度即行高），再用 `LVM_SETITEMSTATE` 落状态。

## 已知限制（待修）

- **弹出菜单（`TrackPopupMenu`）会吞掉注入的键盘导航**——演示脚本里改用鼠标点击菜单项；
  真人用键盘操作菜单不受影响（本条只影响自动化演示）。
