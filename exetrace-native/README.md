# ExeTrace — Rust / Win32 原生控件版

> **[→ 功能演示：6 张真实操作动图（搜索 / 右键菜单 / 一键钉桌面 / 启动 / 托盘热键 / 帮助）](docs/feature-demo.md)**
>
> **[⬇ 下载单文件版（ExeTrace-native-win64.exe，约 1.7 MB，双击即用）](https://github.com/joker1point/ExeTrace/releases/latest)**

用系统自带的原生控件（`SysListView32` / `Edit` / 菜单 / 托盘）重写的 ExeTrace——把「体积与内存」这条线做到头：**没有 UI 框架，没有 GPU 上下文，托盘常驻 ≈5 MB。**

## 实测数据（本机，Windows 11 + 200% 缩放）

| | Python / tkinter 版 | Rust / egui 版 | **本版（Rust / Win32 原生）** |
|---|---|---|---|
| exe 体积 | 20.42 MB | 6.8 MB | **1.73 MB** |
| 窗口打开 | 124 MB | 141 MB | **≈ 12 MB** |
| **托盘常驻** | 9.2 MB | 166 MB（不释放） | **≈ 5 MB** |

测法：`Get-Process` 的 `WorkingSet64`（2026-10-16 v1.1.1 实测：窗口 12.1 MB / 关窗缩托盘 5 秒后 4.3–6.6 MB，中位 5.1 MB）。常驻值在「关窗缩托盘」后 5 秒读取（本版缩托盘时会主动把空闲工作集还给系统）。

结论：egui/eframe 那条线的内存下限是**框架基线**（最小空壳即 115.6 MB，见 `../exetrace-rs/README.md`），
所以「Rust + 低内存」只能走原生控件这条路。

## 功能

与 Python 版对齐，并并入 v3.1 的能力：

- 后台记录打开过的应用（托管进程监控 + 前台窗口判据）
- 搜索实时过滤（名称 / 路径）· 排序（默认最近打开；可切 智能排序 / 打开次数 / 名称）· 分类列 · 停留时长
- 双击 / 回车启动 · 右键菜单（启动 / 打开文件夹 / 复制路径 / 钉桌面 / 加入 memfuse 白名单 / 移除 / 清理失效 / 暂停 / 帮助）
- 一键钉到桌面（创建后**读回校验**；同名自动编号，绝不覆盖）；「钉任意程序…」支持选择 exe 或**软件文件夹**（自动识别主程序，跳过 setup/uninstall/helper）
- 托盘常驻 + 全局热键 `Ctrl+Alt+E` 呼出 + 开机自启（可开关）
- 缩托盘/隐藏时主动回收工作集（上面那张表里 ≈5 MB 的来源）
- **运行中自动隐藏**（2026-10）：正在运行的应用不占列表 ——「找回」只服务"还没找到"的；应用开/关后 ≤3 秒进出列表（判定=进程在跑，按 exe 路径确认，读不出路径按同名兜底）
- **HTML 打开历史**（2026-10-17，v1.2.0）：浏览器打开过的本地 `.html / .htm / .xhtml` 也进同一列表，「视图」切到 **HTML** 即可只看网页；双击用默认浏览器再打开。数据源是 Windows 自带的「最近使用的项目」目录（`%APPDATA%\Microsoft\Windows\Recent` 的 `.lnk`），**不需要额外权限、不劫持浏览器**；目标文件已删除的条目不收录
- **本机项目管理**（2026-10）：自动扫描本机 git 仓库进同一列表（顶部「视图」切 全部 / 应用 / 项目；按「自有 / 克隆 / 本地」分档）。项目行：双击打开文件夹 · 钉到桌面 · 打开 GitHub 页 · 一键更新（`git pull --ff-only`）；扫描根目录用 `config.json` 的 `project_roots` 配置，`--export` 可导出 JSON
- **「交给 Agent」接口**（2026-10-16）：`--export`（全量 JSON：apps + projects）· `--find <词>`（stdout JSON，退出码 0=有结果 1=无结果）· `--launch <路径>` · `--whitelist <名字或 exe 路径>`（加进 memfuse 白名单）· `--agent-guide`（生成随本机路径的接口说明——整段转发给任意 Agent，它即可读数据、调能力）
- **memfuse 联动：右键加入白名单**（2026-10-16）：内存紧张时应用可能**一开就被 memfuse 杀掉**，来不及打开它的白名单界面 —— 在这里右键「加入 memfuse 白名单」即可（调 memfuse 自己的 `-AddProtect`：幂等、去重、保留注释、立即生效，因为守护每 5 秒重读名单）。脚本位置自动探测（环境变量 `MEMFUSE_SCRIPT` → `config.json` 的 `memfuse_script` → 两个常见位置）

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
