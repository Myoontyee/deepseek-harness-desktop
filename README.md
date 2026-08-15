<h1 align="center">
  <img src="src-tauri/icons/icon.png" width="96" alt="DeepSeek Harness Desktop" />
  <br />
  DeepSeek Harness Desktop
</h1>

<p align="center">
  DeepSeek Harness 的桌面端入口——<strong>下载、安装、双击，就是 DeepSeek Harness</strong>，而且永远是最新版。
</p>

<p align="center">
  基于 <a href="https://github.com/deepseek-ai/deepseek-harness">deepseek-ai/deepseek-harness</a> 官方项目的非官方桌面壳（Tauri 2 + WebView2），自动同步官方 <code>master</code> 分支。
</p>

<p align="center">
  <a href="https://github.com/Myoontyee/deepseek-harness-desktop/releases/latest"><img alt="Latest release" src="https://img.shields.io/github/v/release/Myoontyee/deepseek-harness-desktop?style=flat-square&color=171513" /></a>
  <a href="LICENSE"><img alt="License: MIT" src="https://img.shields.io/badge/License-MIT-171513.svg?style=flat-square" /></a>
  <a href="https://github.com/Myoontyee/deepseek-harness-desktop/actions/workflows/release.yml"><img alt="Release build" src="https://github.com/Myoontyee/deepseek-harness-desktop/actions/workflows/release.yml/badge.svg" /></a>
  <img alt="Windows" src="https://img.shields.io/badge/Windows-x64-171513.svg?style=flat-square" />
  <img alt="macOS" src="https://img.shields.io/badge/macOS-Apple%20Silicon-171513.svg?style=flat-square" />
  <img alt="Linux" src="https://img.shields.io/badge/Linux-x64-171513.svg?style=flat-square" />
</p>

<p align="center">
  <a href="https://myoontyee.github.io/deepseek-harness-desktop"><strong>官方网站</strong></a>
  &nbsp;·&nbsp;
  <a href="https://github.com/deepseek-ai/deepseek-harness"><strong>DeepSeek Harness 官方源码仓</strong></a>
</p>

<p align="center"><strong>简体中文</strong> · <a href="README.en.md">English</a></p>

---

## 📸 预览

<img src="docs/app-ui.png" alt="主界面" />

## ⬇️ 下载

| 平台 | 架构 | 安装包 | 下载 |
| --- | --- | --- | --- |
| Windows | x64 | Setup 安装器（95MB，内置运行环境与源码快照） | [下载](https://github.com/Myoontyee/deepseek-harness-desktop/releases/latest/download/DeepSeek.Harness_0.1.0-rc.5_x64-setup.exe) |
| macOS | Apple Silicon | DMG（73MB） | [下载](https://github.com/Myoontyee/deepseek-harness-desktop/releases/latest/download/DeepSeek.Harness_0.1.0-rc.5_aarch64.dmg) |
| Linux | x64 | AppImage（141MB） | [下载](https://github.com/Myoontyee/deepseek-harness-desktop/releases/latest/download/DeepSeek.Harness_0.1.0-rc.5_amd64.AppImage) |
| Debian / Ubuntu | x64 | deb（82MB） | [下载](https://github.com/Myoontyee/deepseek-harness-desktop/releases/latest/download/DeepSeek.Harness_0.1.0-rc.5_amd64.deb) |

所有历史版本见 [GitHub Releases](https://github.com/Myoontyee/deepseek-harness-desktop/releases)。

> [!IMPORTANT]
> 这是非官方社区包装，早期项目。Windows 构建未做商业代码签名，macOS 构建未做 Apple notarization。

## ✨ 为什么有这个项目

DeepSeek Harness 已经提供了完整的 Agent 运行时与 Web UI。本项目提供桌面产品所需的宿主能力：

- **零前置依赖**：内置便携版 Node.js 与 pnpm，目标机器无需安装任何运行时
- **离线优先**：安装包内置官方源码快照（18MB），**首次运行零网络初始化**，网络不可用时照常工作
- **永远最新**：联网时自动同步官方 `master` 分支，主仓更新即应用更新
- **中国网络适配**：自动读取系统代理（Clash 等）注入 git，多级降级（快照 → clone → ZIP）不假死
- **系统托盘常驻**：关闭窗口隐藏到托盘，服务持续运行；单实例互斥
- **故障自愈**：Web 启动失败自动重载，端口冲突自动切换

## 🧱 架构

```
┌──────────────────────────────────────────────────────────────┐
│  DeepSeek Harness Desktop（本仓库）                           │
│  ├─ 桌面壳（Tauri 2 + WebView2，约 3MB，Rust）                │
│  ├─ 内置运行时工具 tools/（随安装包分发）                     │
│  │   ├─ node/    便携版 Node.js                               │
│  │   └─ pnpm     便携版 pnpm                                  │
│  ├─ 内置源码快照 dsh-runtime-snapshot.zip（离线初始化）        │
│  └─ 运行时目录 %LOCALAPPDATA%\DeepSeekHarness\runtime\        │
│      └─ DeepSeek Harness 官方源码（自动保持 master 最新）     │
└──────────────────────────────────────────────────────────────┘
         │ 启动流程
         ▼
┌──────────────────────────────────────────────────────────────┐
│  1. 校验 tools/（Node + pnpm）                                │
│  2. 同步 runtime（内置快照 → git clone → ZIP，三级降级）      │
│  3. pnpm install（依赖变化时才执行）                          │
│  4. pnpm run build（版本变化时才执行）                        │
│  5. 等待服务就绪（含插件端点探活）                            │
│  6. 窗口加载本地服务（启动页 → Web UI，失败自动重载）         │
└──────────────────────────────────────────────────────────────┘
```

## 🚀 快速开始

1. 从上方下载对应平台的安装包
2. 双击安装并打开——**首次启动使用内置代码初始化，无需联网**；联网时自动更新到官方最新版
3. 在界面「设置」中填入你的 DeepSeek API Key（或设置环境变量 `DEEPSEEK_API_KEY` 后重启应用）

### 依赖要求

- Windows 10/11（自带 WebView2）· macOS 11+ · Linux（WebKitGTK 4.1）
- 约 2GB 可用磁盘空间
- 在线更新需要能访问 GitHub 与 npm registry（首次离线初始化不需要）

## 🔨 从源码构建

### 前置条件

- Rust stable 工具链（Windows: MSVC；macOS: Xcode CLT；Linux: gcc）
- 便携版运行时工具 `tools/`（构建脚本自动从官方下载；Windows 也可手动准备）

### 构建

```sh
# 生成官方源码快照（发布时 CI 自动执行）
git -C <官方仓> archive --format=zip -o src-tauri/dsh-runtime-snapshot.zip HEAD

# 构建桌面壳 + 安装包
pnpm dlx @tauri-apps/cli@2 build
# 产物：src-tauri/target/release/bundle/{nsis,dmg,appimage,deb}/
```

### 图标

`src-tauri/icons/` 由 `generate-icon.ps1` 从 `assets/whale.svg`（官方白底黑鲸鱼）生成；
修改后运行 `pnpm dlx @tauri-apps/cli icon app-icon.png` 刷新全套尺寸。

## 🏷️ 版本策略

桌面壳版本号与官方代码仓版本号**一一对应**：

| 位置 | 版本来源 |
| --- | --- |
| 官方代码仓 `deepseek-ai/deepseek-harness` | `package.json` 的 `version`（如 `0.1.0-rc.5`） |
| 本仓库 git tag | `v<官方版本>` |
| 桌面壳 `tauri.conf.json` / `Cargo.toml` | 与官方版本一致 |
| 启动页封面 | 动态显示 `Harness <官方版本> · Desktop <本仓版本>` |

**发版流程**：打 `v*` tag → CI 自动构建 Windows/macOS/Linux 安装包 → 发布 GitHub Release。

## ⚙️ 环境变量

| 变量 | 作用 | 默认 |
| --- | --- | --- |
| `DSH_PORT` | 服务端口（被占用时自动切换） | `3080` |
| `DSH_RUNTIME_DIR` | 运行时源码目录 | `%LOCALAPPDATA%\DeepSeekHarness\runtime` |
| `DSH_TOOLS_DIR` | 内置 Node/pnpm 目录 | exe 同目录 `tools` |
| `DSH_LOCAL_SOURCE` | 本地 git 源（开发测试） | 无 |
| `DSH_SKIP_UPDATE=1` | 跳过更新检查，直接使用现有运行时 | 无 |

## 📁 目录结构

```
deepseek-harness-desktop/
├── assets/               # 图标源（whale.svg 等）
├── ui/                   # 内置启动页（白底杂志风，嵌入 exe）
├── docs/                 # README 截图
├── tools/                # 便携式运行时（Node + pnpm，随安装包分发）
├── src-tauri/
│   ├── src/main.rs       # 桌面壳主体（启动编排/服务生命周期/故障自愈）
│   ├── tauri.conf.json   # Tauri 配置（窗口/打包/标识/安全）
│   ├── icons/            # 应用图标全套
│   └── capabilities/     # Tauri 权限
├── generate-icon.ps1     # 图标生成脚本
└── main/                 # 便携部署产物（不入库）
```

## ❓ 常见问题

- **首次启动很慢？** 内置快照初始化只需几秒（带百分比进度条）；在线更新时取决于网络。
- **启动后白屏/插件未激活？** 应用内置自动重载（最多 3 次）；仍失败查看 `server.log` 定位。
- **端口被占用？** 自动切换到空闲端口；服务复用检测可识别真正的 dsh 实例。
- **离线环境可用吗？** 首次安装后设置 `DSH_SKIP_UPDATE=1` 即可完全离线使用。
- **重复双击会怎样？** 单实例互斥——第二个启动会自动聚焦已有窗口。

## 已知限制

- macOS Intel 构建暂未发布（GitHub runner 资源限制，可手动构建）
- 构建未签名：Windows SmartScreen / macOS Gatekeeper 可能提示，选择"仍要运行"即可

## License

[MIT](LICENSE)
