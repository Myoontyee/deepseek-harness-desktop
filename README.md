# DeepSeek Harness Desktop

DeepSeek Harness 的桌面端入口：**下载、安装、双击，就是 DeepSeek Harness**，并且永远是最新版。

```
┌────────────────────────────────────────────────────────┐
│  DeepSeek Harness Desktop（本仓库）                      │
│  ├─ 桌面壳（Tauri + WebView2，约 3MB）                  │
│  ├─ 内置 Node.js 与 pnpm（随安装包分发，无前置依赖）     │
│  └─ 运行时目录 %LOCALAPPDATA%\DeepSeekHarness\runtime\  │
│     └─ DeepSeek Harness 源码（自动保持 main 分支最新）   │
└────────────────────────────────────────────────────────┘
```

应用本身不携带任何 Harness 代码快照：每次启动先检查
[deepseek-ai/deepseek-harness](https://github.com/deepseek-ai/deepseek-harness) 的
最新提交，有更新就拉取（git 增量；系统没有 git 时整包下载兜底），然后安装依赖、
构建前端、启动服务、打开窗口。**主仓更新即应用更新**，无需手动升级。

## 安装（Windows 10/11）

1. 在 [Releases](https://github.com/Myoontyee/deepseek-harness-desktop/releases) 下载
   `DeepSeek Harness Setup.exe`（约 60MB，包含 Node.js 与 pnpm）。
2. 双击安装，按提示完成（无需管理员权限，装到当前用户目录）。
3. 首次打开：应用会下载 Harness 最新代码并安装依赖，**首次需要几分钟**，
   进度与日志在启动窗口中可见；之后每次打开只需几秒。
4. 首次使用请在界面"设置"中填入你的 DeepSeek API Key
   （或设置环境变量 `DEEPSEEK_API_KEY` 后重启应用）。

依赖要求：Windows 10/11（自带 WebView2）、能访问 GitHub 与 npm registry 的网络、
约 2GB 可用磁盘空间。不需要安装 Node.js、pnpm 或 git。

## 日常使用

- 双击应用图标即可进入；服务控制台窗口（"DeepSeek Harness Server"，最小化）关闭即停止服务。
- 每次启动自动检查更新；正在运行的会话不受更新影响，下次启动生效。
- 用户数据（会话、设置）保存在 `~/.dsh`，与应用安装位置无关，卸载/升级不丢数据。

## 从源码构建

```sh
# 准备内置运行时工具（node + pnpm）
# 手动下载：nodejs.org 的 win-x64 zip → tools/node/，pnpm 的 pnpm-win32-x64.zip → tools/pnpm.exe

# 构建壳（需 Rust stable MSVC 工具链）
cd src-tauri
cargo build --release
# 产物：src-tauri/target/release/dsh-desktop.exe（把 tools/ 放在 exe 同目录即可运行）

# 生成 NSIS 安装包（需先构建好 tools/）
pnpm dlx @tauri-apps/cli build --bundles nsis
# 产物：src-tauri/target/release/bundle/nsis/DeepSeek Harness Setup.exe
```

## 环境变量（高级）

| 变量 | 作用 | 默认 |
| --- | --- | --- |
| `DSH_PORT` | 服务端口 | `3080` |
| `DSH_RUNTIME_DIR` | 运行时源码目录 | `%LOCALAPPDATA%\DeepSeekHarness\runtime` |
| `DSH_TOOLS_DIR` | 内置 Node/pnpm 目录 | exe 同目录 `tools` |
| `DSH_LOCAL_SOURCE` | 本地 git 源（开发测试） | 无 |
| `DSH_SKIP_UPDATE=1` | 跳过更新检查 | 无 |

## 已知限制

- 仅支持 Windows（使用 WebView2 与 cmd 控制台）；macOS/Linux 的壳需另行适配。
- 首次安装需要联网完成依赖安装；离线环境不可用。
- 更新依赖主仓公开可读（`deepseek-ai/deepseek-harness` 公开后匿名拉取即成立）。
- 应用未做单实例互斥：重复双击会打开多个窗口（服务复用同一端口）。

## License

MIT
