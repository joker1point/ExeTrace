# ExeTrace v3 — 应用历史 + 一键钉到桌面

> 越来越多人能用 AI 快速做出一个小众、个性化的桌面软件，但作者往往没有编程背景：
> 不做桌面快捷方式，也不引导用户创建。用户用完、关掉界面，就再也找不到刚才那个软件在哪了。
> **ExeTrace 就是为了解决这件事**：后台记录你打开过的每一个应用，随时一键找回。

## 功能

| 能力 | 说明 |
|---|---|
| 实时记录 | 后台每 2 秒扫描新进程；凭「出现可见窗口 / 由 explorer 启动」确认真实打开，后台服务与更新器不会被误记 |
| 历史回填 | 首次启动即扫描注册表使用痕迹（FeatureUsage / MuiCache），不必等下一次打开 |
| 快速定位 | 搜索、按最近/次数/名称排序、双击直接启动、打开所在文件夹、复制路径 |
| **一键钉到桌面**（v3） | 选中历史记录 → 直接创建桌面快捷方式；自带「起始位置」与图标，同名自动编号（**绝不覆盖**已有文件），创建后立即读回校验；「桌面」列显示已钉标记 |
| 图标与失效标记 | 显示应用图标；已删除的软件灰显「文件已丢失」，可一键清理 |
| 常驻托盘 | 关闭窗口后继续记录；开机自启是可选开关（默认关闭） |

## 使用

1. 双击 `ExeTrace.exe`
2. 在列表里找到目标应用 → 双击启动，或右键「打开所在文件夹」
3. **找不到它的安装位置？选中它 → 点「创建桌面快捷方式」**（或右键同项），下次直接从桌面进
4. 关闭窗口 = 缩到托盘继续记录；右键托盘图标 → 退出

命令行（脚本/批量）：

```powershell
.\ExeTrace.exe --make-shortcut "C:\path\to\App.exe" --name "我的工具"   # 成功退出码 0，已读回校验
```

数据与日志（与 exe 存放位置无关，移动 exe 不丢历史）：

```
%LOCALAPPDATA%\ExeTrace\history.db     历史库
%LOCALAPPDATA%\ExeTrace\exetrace.log   运行日志
%LOCALAPPDATA%\ExeTrace\selftest.txt   最近一次自检报告
```

## 两个实现

| 版本 | 位置 | exe 体积 | 特点 |
|---|---|---|---|
| **Python / tkinter（主力）** | 根目录 | 20.4 MB | 功能最全（搜索排序 / 图标 / 托盘 / 一键钉桌面 / 分类 / 使用时长）；常态 CPU ≈ 2%（单核） |
| Rust / egui（实验） | `exetrace-rs/` | 6.8 MB | 冷启动快、体积小；内存偏高（约 120 MB，为 egui 框架基线） |

两个版本共用同一 SQLite 库（`%LOCALAPPDATA%\ExeTrace\history.db`，schema 一致），数据互通；不建议同时常驻（会双重记录）。

## 从源码构建

```powershell
.\build.ps1        # 干净 venv + PyInstaller onefile → dist\ExeTrace.exe
```

开发运行与自检：

```powershell
.\.venv-build\Scripts\python.exe src\main.py --no-tray
.\.venv-build\Scripts\python.exe src\main.py --selftest
```

## 验证

`--selftest` 覆盖：路径模型 / SQLite 读写与去重 / 注册表扫描 / 图标提取 / 噪音过滤 / 窗口枚举。
打包后运行 `dist\ExeTrace.exe --selftest` 验证交付形态（windowed 产物无控制台，结果落盘
`%LOCALAPPDATA%\ExeTrace\selftest.txt` 回读）。

## 已知边界

- 只能记录 ExeTrace 运行期间打开的应用；更早的历史依赖注册表痕迹（无时间戳条目标为 "—"）
- 以管理员权限运行的程序可能因权限读不到 exe 路径而漏记
- 同一次启动派生多个进程（如 Chromium 系）按 5 分钟窗口去重，只计一次
- onefile 打包是双进程模型：任务管理器里看到两个 `ExeTrace.exe` 属正常现象
- 已开始常驻后，任务栏图标消失不等于退出：右键托盘图标 → 退出

## 验收（可复跑）

```powershell
.\.venv-build\Scripts\python.exe src\main.py --selftest      # 源码形态
.\dist\ExeTrace.exe --selftest                                # 交付形态（结果落盘 selftest.txt）
.\.venv-build\Scripts\python.exe tools\e2e_check.py           # 端到端：GUI+实时记录+数据目录
```
