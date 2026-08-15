// DeepSeek Harness desktop entry — an always-latest runtime host.
//
// The shell owns a local copy of the DeepSeek Harness source tree (the "runtime"),
// keeps it at the latest commit of the main repository, installs its dependencies
// with the bundled pnpm, builds the web frontend, then serves the GUI in a
// WebView2 window. Nothing is prebuilt or frozen: the harness code always comes
// from the repository's current main branch.
//
// Layout (portable, no admin):
//   <exe dir>/tools/node/     bundled portable Node.js
//   <exe dir>/tools/pnpm.exe  bundled pnpm
//   %LOCALAPPDATA%/DeepSeekHarness/runtime/   the harness checkout
//
// Env overrides (also documented in README):
//   DSH_PORT            port (default 3080)
//   DSH_RUNTIME_DIR     runtime checkout location
//   DSH_TOOLS_DIR       bundled tools location (default: next to the exe)
//   DSH_LOCAL_SOURCE    use a local git source instead of GitHub (testing)
//   DSH_SKIP_UPDATE=1   never fetch or clone; use the existing checkout as-is
#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

use std::io::{BufRead, BufReader, Read, Write};
use std::net::TcpStream;
#[cfg(windows)]
use std::os::windows::process::CommandExt;
#[cfg(not(windows))]
use std::os::unix::process::CommandExt as UnixCommandExt;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::thread;
use std::time::{Duration, Instant};

use tauri::menu::{Menu, MenuItem};
use tauri::tray::{MouseButton, MouseButtonState, TrayIconBuilder, TrayIconEvent};
use tauri::{AppHandle, Manager, WebviewWindow, WindowEvent};

const SOURCE_URL: &str = "https://github.com/deepseek-ai/deepseek-harness.git";
const SOURCE_BRANCH: &str = "master";
const API_LATEST: &str = "https://api.github.com/repos/deepseek-ai/deepseek-harness/commits/master";
const APP_DIR_NAME: &str = "DeepSeekHarness";
const RUNTIME_DIR_NAME: &str = "runtime";
const DEFAULT_PORT: u16 = 3080;
const STARTUP_TIMEOUT: Duration = Duration::from_secs(120);
const USER_AGENT: &str = "deepseek-harness-desktop/0.1";

#[derive(PartialEq)]
enum RuntimeState {
    Fresh,
    Updated,
    Unchanged,
}

fn env_or(name: &str, default: &str) -> String {
    std::env::var(name).ok().filter(|v| !v.is_empty()).unwrap_or_else(|| default.to_string())
}

fn env_flag(name: &str) -> bool {
    std::env::var(name).map(|v| v == "1").unwrap_or(false)
}

fn port() -> u16 {
    env_or("DSH_PORT", &DEFAULT_PORT.to_string()).parse().unwrap_or(DEFAULT_PORT)
}

fn runtime_dir() -> PathBuf {
    if let Ok(dir) = std::env::var("DSH_RUNTIME_DIR") {
        if !dir.is_empty() {
            return PathBuf::from(dir);
        }
    }
    let base = std::env::var_os("LOCALAPPDATA")
        .map(PathBuf::from)
        .unwrap_or_else(std::env::temp_dir);
    base.join(APP_DIR_NAME).join(RUNTIME_DIR_NAME)
}

fn tools_dir() -> PathBuf {
    if let Ok(dir) = std::env::var("DSH_TOOLS_DIR") {
        if !dir.is_empty() {
            return PathBuf::from(dir);
        }
    }
    std::env::current_exe()
        .ok()
        .and_then(|exe| exe.parent().map(Path::to_path_buf))
        .unwrap_or_else(std::env::temp_dir)
        .join("tools")
}

fn local_source() -> Option<PathBuf> {
    std::env::var("DSH_LOCAL_SOURCE").ok().filter(|v| !v.is_empty()).map(PathBuf::from)
}

fn skip_update() -> bool {
    env_flag("DSH_SKIP_UPDATE")
}

fn find_on_path(name: &str) -> Option<PathBuf> {
    std::env::var_os("PATH").and_then(|paths| {
        std::env::split_paths(&paths).map(|dir| dir.join(name)).find(|p| p.exists())
    })
}

fn find_git() -> Option<PathBuf> {
    #[cfg(windows)]
    let candidates: &[&str] = &[
        r"C:\Program Files\Git\cmd\git.exe",
        r"C:\Program Files (x86)\Git\cmd\git.exe",
        r"C:\Program Files\Git\bin\git.exe",
    ];
    #[cfg(not(windows))]
    let candidates: &[&str] = &[
        "/usr/bin/git",
        "/usr/local/bin/git",
        "/opt/homebrew/bin/git",
        "/opt/local/bin/git",
    ];
    let name = if cfg!(windows) { "git.exe" } else { "git" };
    if let Some(git) = find_on_path(name) {
        return Some(git);
    }
    candidates
        .iter()
        .map(PathBuf::from)
        .find(|p| p.exists())
}

fn prepend_path(tools: &Path) -> String {
    let mut paths = vec![tools.to_path_buf(), tools.join("node")];
    if let Some(p) = std::env::var_os("PATH") {
        paths.extend(std::env::split_paths(&p));
    }
    std::env::join_paths(paths)
        .map(|p| p.to_string_lossy().into_owned())
        .unwrap_or_default()
}

/// Bundled pnpm executable: `pnpm.exe` on Windows, `pnpm` elsewhere.
fn pnpm_bin(tools: &Path) -> PathBuf {
    #[cfg(windows)]
    let name = "pnpm.exe";
    #[cfg(not(windows))]
    let name = "pnpm";
    tools.join(name)
}

/// Bundled node executable: `node\node.exe` on Windows, `node\bin\node` on
/// macOS/Linux (the official tarball layout).
fn node_bin(tools: &Path) -> PathBuf {
    #[cfg(windows)]
    let rel = ["node", "node.exe"];
    #[cfg(not(windows))]
    let rel = ["node", "bin", "node"];
    rel.iter().fold(tools.to_path_buf(), |acc, part| acc.join(part))
}

/// Run a git command in `cwd`; return trimmed stdout on success.
fn git_out(git: &Path, cwd: &Path, args: &[&str]) -> Option<String> {
    let output = Command::new(git).args(args).current_dir(cwd)
        .stdout(Stdio::piped()).stderr(Stdio::null())
        .output().ok()?;
    if output.status.success() {
        Some(String::from_utf8_lossy(&output.stdout).trim().to_string())
    } else {
        None
    }
}

/// Read the Windows WinINET system proxy (what Clash Verge sets when
/// "系统代理" is enabled): HKCU\...\Internet Settings\ProxyEnable + ProxyServer.
/// Returns e.g. "http://127.0.0.1:7897" so git subprocesses can honor it even
/// when no git-level proxy is configured. On macOS/Linux the standard
/// HTTP_PROXY/HTTPS_PROXY environment variables are inherited by subprocesses
/// automatically, so no registry lookup is needed there.
#[cfg(windows)]
fn system_proxy_url() -> Option<String> {
    use winreg::enums::HKEY_CURRENT_USER;
    use winreg::RegKey;
    let hkcu = RegKey::predef(HKEY_CURRENT_USER);
    let settings = hkcu
        .open_subkey(r"Software\Microsoft\Windows\CurrentVersion\Internet Settings")
        .ok()?;
    let enabled: u32 = settings.get_value("ProxyEnable").ok()?;
    if enabled == 0 {
        return None;
    }
    let server: String = settings.get_value("ProxyServer").ok()?;
    if server.is_empty() {
        return None;
    }
    // ProxyServer may be "host:port" or a per-protocol list like
    // "http=host:port;https=host:port". Prefer the plain form.
    if server.contains('=') {
        for entry in server.split(';') {
            if let Some((proto, addr)) = entry.split_once('=') {
                if proto.eq_ignore_ascii_case("http") || proto.eq_ignore_ascii_case("https") {
                    return Some(format!("http://{addr}"));
                }
            }
        }
        return None;
    }
    Some(format!("http://{server}"))
}

#[cfg(not(windows))]
fn system_proxy_url() -> Option<String> {
    None
}

/// Run a git command with a hard deadline: a slow/unreachable remote must not
/// leave the boot flow hanging forever. Kills the child and reports the
/// timeout when the deadline passes. Injects the Windows system proxy (Clash
/// etc.) into the child's environment so git can reach GitHub without manual
/// git-level proxy configuration.
fn git_out_timeout(
    git: &Path,
    cwd: &Path,
    args: &[&str],
    timeout: Duration,
) -> Result<String, String> {
    let mut command = Command::new(git);
    command
        .args(args)
        .current_dir(cwd)
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    if let Some(proxy) = system_proxy_url() {
        command
            .env("HTTPS_PROXY", &proxy)
            .env("HTTP_PROXY", &proxy)
            .env("https_proxy", &proxy)
            .env("http_proxy", &proxy);
    }
    let mut child = command
        .spawn()
        .map_err(|error| format!("无法启动 git：{error}"))?;
    let start = Instant::now();
    loop {
        match child.try_wait() {
            Ok(Some(status)) => {
                let mut stdout = String::new();
                let mut stderr = String::new();
                if let Some(handle) = child.stdout.as_mut() {
                    let _ = handle.read_to_string(&mut stdout);
                }
                if let Some(handle) = child.stderr.as_mut() {
                    let _ = handle.read_to_string(&mut stderr);
                }
                return if status.success() {
                    Ok(stdout.trim().to_string())
                } else {
                    Err(stderr.trim().to_string())
                };
            }
            Ok(None) => {}
            Err(error) => {
                let _ = child.kill();
                return Err(format!("等待 git 失败：{error}"));
            }
        }
        if start.elapsed() > timeout {
            let _ = child.kill();
            return Err(format!("git 操作超时（{} 秒）", timeout.as_secs()));
        }
        thread::sleep(Duration::from_millis(500));
    }
}

/// Latest harness revision: the local source's HEAD in test mode, else the GitHub API.
fn latest_sha(git: Option<&Path>) -> Option<String> {
    if local_source().is_some() {
        let git = git?;
        let src = local_source()?;
        return git_out(git, src.parent()?, &["-C", src.to_str()?, "rev-parse", "HEAD"]);
    }
    let response = ureq::get(API_LATEST)
        .set("User-Agent", USER_AGENT)
        .timeout(Duration::from_secs(20))
        .call()
        .ok()?;
    let json: serde_json::Value = response.into_json().ok()?;
    json.get("sha")?.as_str().map(str::to_string)
}

/// SHA recorded for a ZIP-installed runtime (git checkouts read HEAD instead).
fn recorded_sha(runtime: &Path) -> Option<String> {
    std::fs::read_to_string(runtime.join(".dsh-version"))
        .ok()
        .map(|s| s.trim().to_string())
}

fn current_sha(git: Option<&Path>, runtime: &Path) -> Option<String> {
    if let Some(git) = git {
        if runtime.join(".git").exists() {
            return git_out(git, runtime, &["rev-parse", "HEAD"]);
        }
    }
    recorded_sha(runtime)
}

/// Path to the official-source snapshot bundled next to the exe, if present.
fn bundled_snapshot_path() -> Option<PathBuf> {
    let exe_dir = std::env::current_exe().ok()?.parent().map(Path::to_path_buf)?;
    let snapshot = exe_dir.join("dsh-runtime-snapshot.zip");
    snapshot.exists().then_some(snapshot)
}

/// Extract a source ZIP into `runtime`. Handles both codeload-style archives
/// (one inner directory) and git-archive-style flat archives. Reports
/// extraction progress (0-100) through `on_progress` when provided.
fn extract_zip_into(
    zip_path: &Path,
    parent: &Path,
    runtime: &Path,
    on_progress: Option<&dyn Fn(u32)>,
) -> Result<(), String> {
    let file = std::fs::File::open(zip_path).map_err(|e| format!("打开内置代码失败：{e}"))?;
    let mut archive = zip::ZipArchive::new(file).map_err(|e| format!("解析内置代码失败：{e}"))?;
    let extract_dir = parent.join("_dsh_snapshot_extract");
    if extract_dir.exists() {
        let _ = std::fs::remove_dir_all(&extract_dir);
    }
    std::fs::create_dir_all(&extract_dir).map_err(|e| format!("创建临时目录失败：{e}"))?;

    // Progress: total uncompressed size vs bytes written so far.
    let mut total: u64 = 0;
    for i in 0..archive.len() {
        if let Ok(entry) = archive.by_index(i) {
            total += entry.size();
        }
    }
    let mut done: u64 = 0;
    for i in 0..archive.len() {
        let mut entry = archive.by_index(i).map_err(|e| format!("读取内置代码条目失败：{e}"))?;
        let size = entry.size();
        if !entry.is_dir() {
            if let Some(name) = entry.enclosed_name() {
                let out_path = extract_dir.join(name);
                if let Some(parent_dir) = out_path.parent() {
                    std::fs::create_dir_all(parent_dir)
                        .map_err(|e| format!("创建目录失败：{e}"))?;
                }
                let mut out = std::fs::File::create(&out_path)
                    .map_err(|e| format!("写入内置代码失败：{e}"))?;
                std::io::copy(&mut entry, &mut out)
                    .map_err(|e| format!("写入内置代码失败：{e}"))?;
            }
        }
        done += size;
        if let Some(report) = on_progress {
            let pct = if total > 0 { (done * 100 / total) as u32 } else { 100 };
            report(pct.min(100));
        }
    }

    let entries: Vec<_> = std::fs::read_dir(&extract_dir)
        .map_err(|e| format!("读取临时目录失败：{e}"))?
        .collect::<Result<_, _>>()
        .map_err(|e| format!("读取临时目录失败：{e}"))?;
    let inner = if entries.len() == 1 && entries[0].file_type().map(|t| t.is_dir()).unwrap_or(false) {
        Some(entries[0].path())
    } else {
        None
    };
    let source = inner.unwrap_or_else(|| extract_dir.clone());
    if runtime.exists() {
        let _ = std::fs::remove_dir_all(runtime);
    }
    std::fs::rename(&source, runtime).map_err(|e| format!("整理目录失败：{e}"))?;
    let _ = std::fs::write(runtime.join(".dsh-version"), "snapshot");
    Ok(())
}

/// Download the repository as a ZIP (codeload) and extract it into `runtime`,
/// recording the revision so later launches can skip the download.
fn download_zip(runtime: &Path, sha: &str) -> Result<(), String> {
    let parent = runtime.parent().ok_or("runtime 路径没有父目录")?;
    let url = format!(
        "https://codeload.github.com/deepseek-ai/deepseek-harness/zip/refs/heads/{SOURCE_BRANCH}"
    );
    let response = ureq::get(&url)
        .set("User-Agent", USER_AGENT)
        .timeout(Duration::from_secs(120))
        .call()
        .map_err(|e| format!("下载失败：{e}"))?;
    let mut bytes = Vec::new();
    response
        .into_reader()
        .take(2 * 1024 * 1024 * 1024)
        .read_to_end(&mut bytes)
        .map_err(|e| format!("读取下载内容失败：{e}"))?;
    if bytes.is_empty() {
        return Err("下载内容为空（网络可能不可达）".into());
    }

    let zip_path = runtime.with_extension("zip");
    std::fs::write(&zip_path, &bytes).map_err(|e| format!("写入临时文件失败：{e}"))?;
    let file = std::fs::File::open(&zip_path).map_err(|e| format!("打开临时文件失败：{e}"))?;
    let mut archive = zip::ZipArchive::new(file).map_err(|e| format!("解压失败：{e}"))?;
    let inner_name = archive
        .by_index(0)
        .map(|f| f.name().to_string())
        .unwrap_or_default();
    archive.extract(parent).map_err(|e| format!("解压失败：{e}"))?;
    let _ = std::fs::remove_file(&zip_path);

    let inner = parent.join(inner_name.trim_end_matches('/'));
    if runtime.exists() {
        let _ = std::fs::remove_dir_all(runtime);
    }
    std::fs::rename(&inner, runtime).map_err(|e| format!("整理目录失败：{e}"))?;
    let _ = std::fs::write(runtime.join(".dsh-version"), sha);
    Ok(())
}

/// Clone or update the runtime checkout so it points at the latest revision.
fn ensure_runtime(
    git: Option<&Path>,
    runtime: &Path,
    on_status: &dyn Fn(&str),
    on_progress: &dyn Fn(u32),
) -> Result<RuntimeState, String> {
    let source = local_source()
        .map(|p| p.to_string_lossy().to_string())
        .unwrap_or_else(|| SOURCE_URL.to_string());

    if runtime.join(".git").exists() {
        if skip_update() {
            return Ok(RuntimeState::Unchanged);
        }
        let git =
            git.ok_or("更新需要 git，但系统中没有找到 git（可在 https://git-scm.com 安装）")?;
        on_status("正在检查 DeepSeek Harness 最新版本…");
        git_out(git, runtime, &["fetch", "--depth", "1", "origin", SOURCE_BRANCH])
            .ok_or("检查更新失败（网络或仓库不可达）")?;
        let head = git_out(git, runtime, &["rev-parse", "HEAD"]);
        let fetched = git_out(git, runtime, &["rev-parse", "FETCH_HEAD"]);
        if head.is_some() && fetched.is_some() && head != fetched {
            on_status("发现新版本，正在更新…");
            git_out(git, runtime, &["reset", "--hard", "FETCH_HEAD"])
                .ok_or("更新代码失败（git reset）")?;
            Ok(RuntimeState::Updated)
        } else {
            Ok(RuntimeState::Unchanged)
        }
    } else if runtime.join("package.json").exists() {
        // ZIP-installed runtime (no git): compare the recorded revision
        // against the latest one and re-download only on change.
        if skip_update() {
            return Ok(RuntimeState::Unchanged);
        }
        on_status("正在检查 DeepSeek Harness 最新版本…");
        let Some(sha) = latest_sha(git) else {
            return Ok(RuntimeState::Unchanged); // cannot check right now; keep the local copy
        };
        if recorded_sha(runtime) == Some(sha.clone()) {
            return Ok(RuntimeState::Unchanged);
        }
        on_status("发现新版本，正在下载…");
        download_zip(runtime, &sha)?;
        Ok(RuntimeState::Fresh)
    } else {
        if skip_update() {
            return Err("DSH_SKIP_UPDATE 已设置，但本地没有可用的运行环境".into());
        }
        // Bundled snapshot: the installer ships the official source as a ZIP
        // next to the exe, so a fresh machine works even with no network.
        if let Some(snapshot) = bundled_snapshot_path() {
            let parent = runtime.parent().ok_or("runtime 路径没有父目录")?;
            if runtime.exists() {
                let _ = std::fs::remove_dir_all(runtime);
            }
            match extract_zip_into(&snapshot, parent, runtime, Some(on_progress)) {
                Ok(()) => return Ok(RuntimeState::Fresh),
                Err(error) => {
                    on_status(&format!("内置代码初始化失败（{error}），尝试在线下载…"));
                    let _ = std::fs::remove_dir_all(runtime);
                }
            }
        }

        on_status("首次运行：正在下载 DeepSeek Harness 最新代码…");
        // Prefer git clone (keeps the checkout updateable), but GitHub can be
        // slow or unreachable from some networks. Retry once, then fall back
        // to the ZIP download (codeload) before giving up.
        let mut clone_error = String::new();
        if let Some(git) = git {
            for attempt in 0..2 {
                if runtime.exists() {
                    let _ = std::fs::remove_dir_all(runtime);
                }
                let parent = runtime.parent().ok_or("runtime 路径没有父目录")?;
                let clone_target = runtime.to_str().unwrap_or(RUNTIME_DIR_NAME);
                match git_out_timeout(
                    git,
                    parent,
                    &["clone", "--depth", "1", "--branch", SOURCE_BRANCH, &source, clone_target],
                    Duration::from_secs(240),
                ) {
                    Ok(_) => return Ok(RuntimeState::Fresh),
                    Err(error) => {
                        clone_error = error;
                        on_status(&format!(
                            "git 克隆失败（{}），正在尝试备用下载…",
                            if attempt == 0 { "第 1 次" } else { "第 2 次" }
                        ));
                        thread::sleep(Duration::from_secs(3));
                    }
                }
            }
        }
        // ZIP fallback: works without git and survives git-specific failures.
        on_status("正在通过备用通道下载…");
        let sha = latest_sha(git).unwrap_or_default();
        match download_zip(runtime, &sha) {
            Ok(()) => Ok(RuntimeState::Fresh),
            Err(error) => Err(format!(
                "下载代码失败：git clone 失败（{}），备用下载也失败（{}）",
                if clone_error.is_empty() { "系统未安装 git" } else { &clone_error },
                error
            )),
        }
    }
}

/// Run a long step (install/build) streaming its output to the UI log.
///
/// Reader threads are detached, never joined: a grandchild inheriting the
/// pipe keeps it open after the direct child exits, so waiting for EOF would
/// hang the boot flow forever.
fn run_step(
    program: &Path,
    args: &[&str],
    cwd: &Path,
    tools: &Path,
    window: &WebviewWindow,
) -> Result<bool, String> {
    let mut child = Command::new(program)
        .args(args)
        .current_dir(cwd)
        .env("PATH", prepend_path(tools))
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .map_err(|e| format!("无法启动 {}：{e}", program.display()))?;
    let stdout = child.stdout.take().expect("stdout piped");
    let stderr = child.stderr.take().expect("stderr piped");
    let out_window = window.clone();
    thread::spawn(move || {
        for line in BufReader::new(stdout).lines().map_while(Result::ok) {
            append_log(&out_window, &line);
        }
    });
    let err_window = window.clone();
    thread::spawn(move || {
        for line in BufReader::new(stderr).lines().map_while(Result::ok) {
            append_log(&err_window, &line);
        }
    });
    let status = child.wait().map_err(|e| format!("进程执行失败：{e}"))?;
    // Let the readers flush trailing lines; never join them (see above).
    thread::sleep(Duration::from_millis(300));
    Ok(status.success())
}

fn install_deps(
    pnpm: &Path,
    runtime: &Path,
    tools: &Path,
    window: &WebviewWindow,
) -> Result<(), String> {
    if !run_step(pnpm, &["install", "--frozen-lockfile"], runtime, tools, window)? {
        // Lockfile drift (e.g. a mid-flight change in the repo): plain install.
        run_step(pnpm, &["install"], runtime, tools, window)?;
    }
    Ok(())
}

/// Build the repo (libs + web frontend) unless the marker matches the current revision.
fn build_repo(
    pnpm: &Path,
    runtime: &Path,
    tools: &Path,
    sha: &str,
    window: &WebviewWindow,
) -> Result<(), String> {
    let marker = runtime.join(".dsh-build");
    let up_to_date = std::fs::read_to_string(&marker)
        .map(|s| s.trim() == sha)
        .unwrap_or(false)
        && runtime
            .join("apps")
            .join("web")
            .join("dist")
            .join("index.html")
            .exists();
    if up_to_date {
        return Ok(());
    }
    if !run_step(pnpm, &["run", "build"], runtime, tools, window)? {
        return Err("构建失败，详情见下方日志".into());
    }
    let _ = std::fs::write(&marker, sha);
    Ok(())
}

/// The web boot's immediate tier loads client plugin bundles from /plugins
/// right after the root page answers. On a freshly started server the root can
/// 200 before those routes are fully live, and a first-load module failure
/// leaves the whole plugin graph pending on base services (connection, typert,
/// remote, ...). Poll a known plugin bundle until it answers before navigating.
fn server_plugins_ready(port: u16) -> bool {
    const PLUGIN_PATH: &str = "/plugins/@deepseek-ai/dsh-typert-registry/client.js";
    let deadline = Instant::now() + Duration::from_secs(60);
    while Instant::now() < deadline {
        if let Ok(mut stream) = TcpStream::connect(("127.0.0.1", port)) {
            let _ = stream.set_read_timeout(Some(Duration::from_secs(2)));
            let request = format!(
                "GET {PLUGIN_PATH} HTTP/1.0\r\nHost: 127.0.0.1:{port}\r\n\r\n"
            );
            if stream.write_all(request.as_bytes()).is_ok() {
                let mut buf = [0u8; 64];
                if stream.read(&mut buf).is_ok()
                    && String::from_utf8_lossy(&buf[..]).contains("200")
                {
                    return true;
                }
            }
        }
        thread::sleep(Duration::from_millis(750));
    }
    false
}

/// Inject the update panel (floating button + status card) into the loaded
/// page. The page is a plain http origin without Tauri IPC, so the panel
/// talks to the shell through the loopback bridge (port 3091) and the shell
/// pushes progress via this same eval channel. Re-injection is idempotent
/// (the script guards itself), so reloads are covered.
fn inject_update_panel(window: &WebviewWindow) {
    let script = r#"
(() => {
  if (window.__DSH_UPDATE_PANEL__) return;
  window.__DSH_UPDATE_PANEL__ = true;
  const BRIDGE = 'http://127.0.0.1:3091';
  const host = document.createElement('div');
  host.id = 'dsh-update-panel';
  const style = document.createElement('style');
  style.textContent = `
    #dsh-update-panel{position:fixed;right:16px;bottom:16px;z-index:99999;font-family:"Segoe UI","Microsoft YaHei",sans-serif;color:#111;user-select:none}
    #dsh-update-panel .fab{width:42px;height:42px;border-radius:50%;background:#fafafa;border:1px solid #ddd;box-shadow:0 2px 10px rgba(0,0,0,.12);cursor:pointer;display:flex;align-items:center;justify-content:center;font-size:18px;transition:box-shadow .2s}
    #dsh-update-panel .fab:hover{box-shadow:0 4px 16px rgba(0,0,0,.18)}
    #dsh-update-panel .fab .dot{position:absolute;top:8px;right:8px;width:8px;height:8px;border-radius:50%;background:#e5484d;display:none}
    #dsh-update-panel .card{display:none;position:absolute;right:0;bottom:54px;width:300px;background:#fafafa;border:1px solid #e4e4e4;border-radius:8px;padding:16px;box-shadow:0 8px 28px rgba(0,0,0,.12)}
    #dsh-update-panel .card.open{display:block}
    #dsh-update-panel .title{font-size:13px;letter-spacing:.08em;margin-bottom:12px;border-bottom:1px solid #e4e4e4;padding-bottom:8px}
    #dsh-update-panel .row{display:flex;justify-content:space-between;font-size:12px;line-height:2}
    #dsh-update-panel .row .cur{color:#666}
    #dsh-update-panel .row .new{color:#e5484d;font-weight:600}
    #dsh-update-panel .row .ok{color:#2e7d32}
    #dsh-update-panel .btn{margin-top:12px;width:100%;padding:8px;background:#111;color:#fff;border:none;border-radius:4px;font-size:12px;cursor:pointer;letter-spacing:.08em}
    #dsh-update-panel .btn:disabled{background:#bbb;cursor:default}
    #dsh-update-panel .btn.ghost{background:#fafafa;color:#111;border:1px solid #ddd;margin-top:6px}
    #dsh-update-panel .progress{margin-top:10px;height:3px;background:#e4e4e4;border-radius:2px;overflow:hidden;display:none}
    #dsh-update-panel .progress i{display:block;height:100%;background:#111;width:0}
    #dsh-update-panel .status{font-size:11px;color:#666;margin-top:8px;min-height:16px}
  `;
  host.innerHTML = `
    <div class="fab" id="dsh-up-fab" title="检查更新">&#9881;<span class="dot" id="dsh-up-dot"></span></div>
    <div class="card" id="dsh-up-card">
      <div class="title">更新</div>
      <div class="row"><span>桌面壳</span><span id="dsh-up-shell"></span></div>
      <div class="row"><span>Harness 代码</span><span id="dsh-up-harness"></span></div>
      <button class="btn" id="dsh-up-btn" disabled>检查中…</button>
      <button class="btn ghost" id="dsh-up-release" style="display:none">下载桌面壳新版本</button>
      <div class="progress" id="dsh-up-progress"><i></i></div>
      <div class="status" id="dsh-up-status"></div>
    </div>`;
  document.documentElement.appendChild(style);
  document.body.appendChild(host);
  const $ = id => host.querySelector('#' + id);
  let shellUpdate = false;
  let harnessUpdate = false;
  $('dsh-up-fab').addEventListener('click', () => $('dsh-up-card').classList.toggle('open'));
  $('dsh-up-btn').addEventListener('click', () => {
    if (harnessUpdate) {
      fetch(BRIDGE + '/upgrade-harness', { method: 'POST' }).catch(() => {});
      $('dsh-up-btn').disabled = true;
      $('dsh-up-btn').textContent = '升级中…';
    } else if (shellUpdate) {
      fetch(BRIDGE + '/open-release', { method: 'POST' }).catch(() => {});
    }
  });
  $('dsh-up-release').addEventListener('click', () => {
    fetch(BRIDGE + '/open-release', { method: 'POST' }).catch(() => {});
  });
  async function refresh() {
    try {
      const r = await fetch(BRIDGE + '/status', { cache: 'no-store' });
      const s = await r.json();
      shellUpdate = s.shellLatest && s.shellCurrent !== s.shellLatest;
      harnessUpdate = s.harnessLatest && s.harnessCurrent && s.harnessCurrent !== s.harnessLatest;
      $('dsh-up-shell').innerHTML = s.shellCurrent + (shellUpdate ? ' → <span class="new">' + s.shellLatest + '</span>' : ' <span class="ok">✓</span>');
      $('dsh-up-harness').innerHTML = s.harnessCurrent.slice(0, 8) + (harnessUpdate ? ' → <span class="new">' + s.harnessLatest.slice(0, 8) + '</span>' : (s.harnessLatest ? ' <span class="ok">✓</span>' : ''));
      $('dsh-up-dot').style.display = (shellUpdate || harnessUpdate) ? 'block' : 'none';
      $('dsh-up-release').style.display = shellUpdate ? 'block' : 'none';
      const btn = $('dsh-up-btn');
      if (s.upgrading) {
        btn.disabled = true;
        btn.textContent = '升级中…';
        $('dsh-up-progress').style.display = 'block';
        $('dsh-up-progress').querySelector('i').style.width = s.progress + '%';
        $('dsh-up-status').textContent = s.status;
      } else {
        btn.disabled = !(shellUpdate || harnessUpdate);
        btn.textContent = harnessUpdate ? '一键升级 Harness' : (shellUpdate ? '查看新版本' : '已是最新');
        $('dsh-up-progress').style.display = 'none';
        $('dsh-up-status').textContent = s.status || '';
      }
    } catch (e) {
      $('dsh-up-status').textContent = '无法连接更新服务';
    }
  }
  refresh();
  setInterval(refresh, 5000);
})();
"#;
    let window = window.clone();
    std::thread::spawn(move || {
        // Poll until the page is ready, then keep refreshing every few seconds
        // so the panel survives page reloads.
        loop {
            thread::sleep(Duration::from_secs(4));
            eval_js(&window, &format!("if (document.body) {{ {} }}", script));
            eval_js(&window, &format!("if (document.body) {{ {} }}", CONTEXT_MENU_SCRIPT));
        }
    });
}

/// Session-tree context menu (Codex-style): right-click a conversation row to
/// pin / rename / archive / copy working dir / copy session id / copy deep
/// link / fork. Talks to the official HTTP API; session and workspace ids are
/// resolved by matching titles against `list` responses.
const CONTEXT_MENU_SCRIPT: &str = r#"
(() => {
  if (window.__DSH_CONTEXT_MENU__) return;
  window.__DSH_CONTEXT_MENU__ = true;
  const CSS = `
    #dsh-ctx-menu{position:fixed;z-index:999999;min-width:190px;background:#fafafa;border:1px solid #e4e4e4;border-radius:8px;padding:6px;box-shadow:0 8px 28px rgba(0,0,0,.14);font-family:"Segoe UI","Microsoft YaHei",sans-serif;color:#111;font-size:12px;display:none}
    #dsh-ctx-menu .item{padding:7px 10px;border-radius:5px;cursor:pointer;display:flex;align-items:center;gap:8px;letter-spacing:.02em}
    #dsh-ctx-menu .item:hover{background:#ececec}
    #dsh-ctx-menu .sep{height:1px;background:#e8e8e8;margin:4px 6px}
    #dsh-ctx-menu .head{padding:5px 10px 8px;font-size:10px;letter-spacing:.1em;color:#999;border-bottom:1px solid #eee;margin-bottom:4px;overflow:hidden;text-overflow:ellipsis;white-space:nowrap;max-width:220px}
  `;
  const style = document.createElement('style');
  style.textContent = CSS;
  document.documentElement.appendChild(style);
  const menu = document.createElement('div');
  menu.id = 'dsh-ctx-menu';
  document.body.appendChild(menu);

  const rpc = async (channel, method, payload) => {
    const full = channel + '.' + method;
    const res = await fetch('/api/' + full, {
      method: 'POST',
      headers: { 'content-type': 'application/json' },
      body: JSON.stringify({ type: 'client-request', rpcId: crypto.randomUUID(), method: full, payload: payload || {} }),
    });
    const json = await res.json();
    return json.result;
  };
  const copy = (text) => navigator.clipboard.writeText(text).catch(() => {});

  let current = null; // { sessionId, title, cwd, workspaceId }

  async function resolveSession(row) {
    const titleEl = row.querySelector('.qM_tdG_title');
    const title = titleEl ? titleEl.textContent.trim() : '';
    // workspace = nearest ancestor project row
    let proj = row.parentElement;
    let workspaceTitle = '';
    while (proj && !proj.classList.contains('qM_tdG_projectRow')) proj = proj.parentElement;
    if (proj) workspaceTitle = proj.querySelector('.qM_tdG_title')?.textContent.trim() || '';
    try {
      const [w, s] = await Promise.all([
        rpc('workspace', 'list', {}),
        rpc('session', 'list', {}),
      ]);
      const ws = (w.items || []).find(x => x.title === workspaceTitle) || (w.items || [])[0];
      const ids = ws ? ws.sessionIds : [];
      const session = (s.items || []).find(x => x.sessionId && ids.includes(x.sessionId) && (x.title || '').trim() === title)
        || (s.items || []).find(x => (x.title || '').trim() === title);
      if (!session) return null;
      return { sessionId: session.sessionId, title, cwd: ws ? ws.path : session.cwd, workspaceId: ws ? ws.workspaceId : undefined };
    } catch (e) {
      return null;
    }
  }

  function showMenu(x, y, info) {
    current = info;
    const items = [
      { label: '置顶聊天', fn: () => rpc('workspace', 'insertSessionBefore', { workspaceId: info.workspaceId, sessionId: info.sessionId }) },
      { label: '重命名聊天', fn: () => { const t = prompt('重命名会话', info.title); if (t && t.trim()) return rpc('session', 'rename', { sessionId: info.sessionId, title: t.trim() }); } },
      { label: '归档聊天', fn: () => rpc('workspace', 'archiveSession', { sessionId: info.sessionId }) },
      { label: '创建聊天分支', fn: () => rpc('session', 'fork', { sessionId: info.sessionId }) },
      { sep: true },
      { label: '复制工作目录', fn: () => info.cwd ? copy(info.cwd) : null },
      { label: '复制会话 ID', fn: () => copy(info.sessionId) },
      { label: '复制深度链接', fn: () => copy('dsh-desktop://session/' + info.sessionId) },
      { label: '在新窗口中打开', fn: () => copy('http://' + location.host + '/?session=' + info.sessionId) },
    ];
    menu.innerHTML = '<div class="head">' + (info.title || '') + '</div>' +
      items.map(it => it.sep
        ? '<div class="sep"></div>'
        : '<div class="item">' + it.label + '</div>').join('');
    menu.style.display = 'block';
    menu.style.left = Math.min(x, window.innerWidth - 210) + 'px';
    menu.style.top = Math.min(y, window.innerHeight - menu.offsetHeight - 8) + 'px';
    const handlers = [];
    let idx = 0;
    for (const it of items) {
      if (it.sep) continue;
      const el = menu.children[idx + 1];
      handlers.push(el.addEventListener('click', () => { hide(); Promise.resolve(it.fn()).then(() => setTimeout(() => location.reload(), 300)).catch(() => {}); }));
      idx++;
    }
  }
  function hide() { menu.style.display = 'none'; current = null; }

  document.addEventListener('contextmenu', (e) => {
    const row = e.target.closest('.qM_tdG_sessionRow');
    if (!row) return;
    e.preventDefault();
    resolveSession(row).then(info => {
      if (info) showMenu(e.clientX, e.clientY, info);
    });
  }, true);
  document.addEventListener('click', (e) => {
    if (!menu.contains(e.target)) hide();
  });
  window.addEventListener('blur', hide);
  window.addEventListener('resize', hide);
})();
"#;

/// Web boot watchdog: if the first page load races the plugin-module serving
/// and the boot fails (whole plugin graph pending), the failure report is
/// rendered by AppRoot with the literal "Failed to load plugins" title. Reload
/// the page — the retry lands on a warm server. At most a few attempts.
fn watch_boot_failure(window: &WebviewWindow) {
    let window = window.clone();
    thread::spawn(move || {
        let mut reloads = 0u8;
        loop {
            thread::sleep(Duration::from_secs(4));
            if reloads >= 3 {
                return;
            }
            eval_js(
                &window,
                "if (document.body && document.body.innerText \
                 && document.body.innerText.indexOf('Failed to load plugins') !== -1) \
                 { location.reload() }",
            );
            reloads += 1;
            // Give a reloaded page time to boot before the next probe.
            thread::sleep(Duration::from_secs(20));
        }
    });
}

/// True when a dsh service answers on the port. Requires both HTTP 200 and
/// the harness boot manifest marker, so a foreign web server squatting on the
/// port is NOT mistaken for our service (and triggers port fallback instead).
fn server_ready(port: u16) -> bool {
    let Ok(mut stream) = TcpStream::connect(("127.0.0.1", port)) else {
        return false;
    };
    let _ = stream.set_read_timeout(Some(Duration::from_secs(2)));
    if stream
        .write_all(format!("GET / HTTP/1.0\r\nHost: 127.0.0.1:{port}\r\n\r\n").as_bytes())
        .is_err()
    {
        return false;
    }
    let mut buf = [0u8; 2048];
    let Ok(n) = stream.read(&mut buf) else {
        return false;
    };
    let head = String::from_utf8_lossy(&buf[..n]);
    head.contains("200") && (head.contains("__DSH_BOOT__") || head.contains("DeepSeek Harness"))
}

/// Whether the preferred port is free to bind (nothing else is listening).
fn port_free(port: u16) -> bool {
    std::net::TcpListener::bind(("127.0.0.1", port)).is_ok()
}

/// First free port starting at `preferred` (up to +20), or `preferred` if
/// everything in range is taken.
fn fallback_port(preferred: u16) -> u16 {
    (preferred..preferred + 20).find(|p| port_free(*p)).unwrap_or(preferred)
}

/// Start `pnpm dsh web`, redirecting its output to `server.log` for
/// diagnostics.
///
/// On Windows the server runs in its own console window that is hidden right
/// after startup: `CREATE_NO_WINDOW` would silently break native host dialogs
/// (the workspace directory picker spawns a worker whose `IFileOpenDialog`
/// cannot show without a real console/desktop environment), while a visible
/// console is ugly. The console title doubles as the finder key for hiding.
/// On macOS/Linux the server runs in the foreground process group (no extra
/// console concept exists there).
///
/// The server is spawned directly (no cmd/`start` indirection, so no shell
/// quoting pitfalls) and its lifecycle is tied to the app: when the app
/// exits, the whole process tree is killed.
fn start_server(pnpm: &Path, runtime: &Path, tools: &Path, port: u16) -> Option<std::process::Child> {
    let log = std::fs::File::create(runtime.join("server.log")).ok()?;
    let stdout = Stdio::from(log.try_clone().ok()?);
    let stderr = Stdio::from(log);
    let mut command = Command::new(pnpm);
    command
        .args(["dsh", "web", "--port", &port.to_string()])
        .current_dir(runtime)
        .env("PATH", prepend_path(tools))
        .stdout(stdout)
        .stderr(stderr);
    #[cfg(windows)]
    {
        command.creation_flags(0x0000_0010); // CREATE_NEW_CONSOLE: own console we can hide
    }
    #[cfg(not(windows))]
    command.process_group(0); // own process group so kill_tree can stop the tree
    let child = command.spawn().ok()?;
    #[cfg(windows)]
    hide_server_console(child.id());
    Some(child)
}

/// Hide the server's console window shortly after spawn. The window title is
/// the pnpm/node process name, so find the console by the process's window
/// via `GetConsoleWindow` in a child helper is fragile; instead we locate the
/// top-level window whose process id matches the server pid.
#[cfg(windows)]
fn hide_server_console(pid: u32) {
    std::thread::spawn(move || {
        std::thread::sleep(Duration::from_millis(800));
        unsafe {
            use windows_sys::Win32::Foundation::HWND;
            use windows_sys::Win32::UI::WindowsAndMessaging::{
                EnumWindows, GetWindowThreadProcessId, IsWindowVisible, ShowWindow,
            };
            struct FindCtx {
                pid: u32,
                found: HWND,
            }
            let mut ctx = FindCtx { pid, found: std::ptr::null_mut() };
            extern "system" fn find_by_pid(hwnd: HWND, lparam: isize) -> i32 {
                unsafe {
                    let ctx = &mut *(lparam as *mut FindCtx);
                    let mut window_pid: u32 = 0;
                    GetWindowThreadProcessId(hwnd, &mut window_pid);
                    if window_pid == ctx.pid && IsWindowVisible(hwnd) != 0 {
                        ctx.found = hwnd;
                        return 0; // stop enumerating
                    }
                }
                1 // continue
            }
            EnumWindows(Some(find_by_pid), &mut ctx as *mut FindCtx as isize);
            if !ctx.found.is_null() {
                ShowWindow(ctx.found, 0); // SW_HIDE
            }
        }
    });
}

/// Kill a process tree used to stop the server together with the app.
/// Windows: `taskkill /T /F`. macOS/Linux: signal the whole process group.
fn kill_tree(pid: u32) {
    #[cfg(windows)]
    let _ = Command::new("taskkill")
        .args(["/PID", &pid.to_string(), "/T", "/F"])
        .creation_flags(0x0800_0000)
        .spawn();
    #[cfg(not(windows))]
    {
        let _ = Command::new("kill").args(["-TERM", &format!("-{pid}")]).spawn();
    }
}

// --- UI plumbing -----------------------------------------------------------

fn js_string(s: &str) -> String {
    let mut out = String::with_capacity(s.len() + 2);
    out.push('"');
    for c in s.chars() {
        match c {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            c if (c as u32) < 0x20 => out.push_str(&format!("\\u{:04x}", c as u32)),
            c => out.push(c),
        }
    }
    out.push('"');
    out
}

fn eval_js(window: &WebviewWindow, js: &str) {
    let _ = window.eval(js);
}

fn set_status(window: &WebviewWindow, text: &str) {
    eval_js(window, &format!("setStatus({})", js_string(text)));
}

fn append_log(window: &WebviewWindow, line: &str) {
    eval_js(window, &format!("appendLog({})", js_string(line)));
}

fn fail(window: &WebviewWindow, message: &str) {
    eval_js(window, &format!("showError({})", js_string(message)));
}

/// Show both versions on the boot page: the official harness version (runtime
/// package.json) and this desktop shell's own version (crate version / tag).
/// When the harness version cannot be read yet, hide the line instead of
/// showing a placeholder.
fn show_version(window: &WebviewWindow, runtime: &Path) {
    match std::fs::read_to_string(runtime.join("package.json"))
        .ok()
        .and_then(|text| serde_json::from_str::<serde_json::Value>(&text).ok())
        .and_then(|json| json.get("version").and_then(|v| v.as_str()).map(str::to_string))
    {
        Some(harness) => eval_js(
            window,
            &format!("setVersion({}, {})", js_string(&harness), js_string(env!("CARGO_PKG_VERSION"))),
        ),
        None => eval_js(window, "document.getElementById('version').style.visibility = 'hidden'"),
    }
}

// --- boot flow -------------------------------------------------------------

fn run(app: &AppHandle, window: WebviewWindow) {
    // Give the embedded boot page a moment to parse before the first eval.
    thread::sleep(Duration::from_millis(1200));

    let tools = tools_dir();
    let pnpm = pnpm_bin(&tools);
    let node = node_bin(&tools);
    if !pnpm.exists() || !node.exists() {
        return fail(
            &window,
            &format!(
                "运行时组件缺失：未找到 tools/{} 或 tools/node，请重新安装应用",
                pnpm.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_else(|| "pnpm".into())
            ),
        );
    }

    let runtime = runtime_dir();
    let mut p = port();
    let git = find_git();

    // Port conflict handling: if the preferred port is already serving a dsh
    // instance (e.g. a manually started `dsh web`), reuse it; if it is taken
    // by something else (foreign service), fall back to a free port.
    if !server_ready(p) && !port_free(p) {
        p = fallback_port(p + 1);
        set_status(&window, &format!("端口 {p} 被占用，已切换到 {p}"));
    }

    let on_progress = |pct: u32| {
        set_status(&window, &format!("正在使用内置代码初始化… {pct}%"));
        eval_js(&window, &format!("setProgress({pct})"));
    };
    let state = match ensure_runtime(git.as_deref(), &runtime, &|s| set_status(&window, s), &on_progress) {
        Ok(state) => state,
        Err(e) => return fail(&window, &format!("准备运行环境失败：{e}")),
    };

    // The runtime is ready (or the bundled snapshot was extracted): only now
    // can the real harness version be read from its package.json.
    show_version(&window, &runtime);

    let sha = current_sha(git.as_deref(), &runtime).unwrap_or_default();

    if state != RuntimeState::Unchanged || !runtime.join("node_modules").exists() {
        set_status(&window, "正在安装依赖（首次可能需要几分钟）…");
        if let Err(e) = install_deps(&pnpm, &runtime, &tools, &window) {
            return fail(&window, &format!("安装依赖失败：{e}"));
        }
    }

    set_status(&window, "正在构建（首次可能需要 10-20 分钟，进度见下方日志）…");
    if let Err(e) = build_repo(&pnpm, &runtime, &tools, &sha, &window) {
        return fail(&window, &format!("构建失败：{e}"));
    }

    let server = app.state::<ServerState>();
    let mut started_own = false;
    if !server_ready(p) {
        set_status(&window, "正在启动服务…");
        let mut slot = server.0.lock().expect("server slot lock");
        *slot = start_server(&pnpm, &runtime, &tools, p);
        started_own = true;
    }

    let deadline = Instant::now() + STARTUP_TIMEOUT;
    while Instant::now() < deadline {
        if server_ready(p) {
            break;
        }
        thread::sleep(Duration::from_millis(750));
    }
    if !server_ready(p) {
        return fail(&window, "服务启动超时（120 秒）。请检查网络后重新打开应用");
    }
    let _ = started_own;

    // A freshly started server can answer the root page before its /plugins
    // routes are fully live; loading the app in that window leaves the plugin
    // graph pending on base services. Wait for a known plugin bundle to answer
    // before navigating (a warm server passes in ~1s).
    set_status(&window, "正在启动 DeepSeek Harness…");
    server_plugins_ready(p);

    set_status(&window, "正在打开 DeepSeek Harness…");
    let url = format!("http://127.0.0.1:{p}");
    let app_handle = app.clone();
    let _ = app.run_on_main_thread(move || {
        let window_handle = app_handle.clone();
        if let Some(win) = window_handle.get_webview_window("main") {
            if let Ok(parsed) = url.parse() {
                let _ = win.navigate(parsed);
            }
        }
    });
    // Belt and suspenders: if the first load still fails to boot, reload.
    watch_boot_failure(&window);

    // Update bridge: version comparison + one-click upgrade from the injected
    // panel. Runs against the local service; version lookups are best-effort.
    let shared = SharedUpdate(std::sync::Mutex::new(UpdateSnapshot {
        shell_current: env!("CARGO_PKG_VERSION").to_string(),
        shell_latest: None,
        harness_current: current_sha(git.as_deref(), &runtime).unwrap_or_default(),
        harness_latest: None,
        upgrading: false,
        progress: 0,
        status: String::new(),
    }));
    let (shell_latest, harness_latest) = check_updates();
    if let Ok(mut s) = shared.0.lock() {
        s.shell_latest = shell_latest;
        s.harness_latest = harness_latest;
    }
    start_update_bridge(
        shared,
        app.clone(),
        git.map(PathBuf::from),
        runtime.clone(),
        tools.clone(),
        pnpm.clone(),
        p,
    );
    inject_update_panel(&window);
}

/// The app-started server child (None when an already-running server was reused).
struct ServerState(std::sync::Mutex<Option<std::process::Child>>);

// --- update bridge -----------------------------------------------------------

/// Snapshot of the version check, shared with the injected update panel.
struct UpdateSnapshot {
    shell_current: String,
    shell_latest: Option<String>,
    harness_current: String,
    harness_latest: Option<String>,
    upgrading: bool,
    progress: u32,
    status: String,
}

impl UpdateSnapshot {
    fn json(&self) -> String {
        format!(
            "{{\"shellCurrent\":{},\"shellLatest\":{},\"harnessCurrent\":{},\"harnessLatest\":{},\"upgrading\":{},\"progress\":{},\"status\":{}}}",
            js_string(&self.shell_current),
            self.shell_latest.as_ref().map(|s| js_string(s)).unwrap_or_else(|| "null".into()),
            js_string(&self.harness_current),
            self.harness_latest.as_ref().map(|s| js_string(s)).unwrap_or_else(|| "null".into()),
            self.upgrading,
            self.progress,
            js_string(&self.status),
        )
    }
}

struct SharedUpdate(std::sync::Mutex<UpdateSnapshot>);

/// Best-effort GitHub lookups: latest release tag of this repo and the latest
/// official master sha. Failures yield None (the panel shows "无法连接").
fn check_updates() -> (Option<String>, Option<String>) {
    let shell = ureq::get("https://api.github.com/repos/Myoontyee/deepseek-harness-desktop/releases/latest")
        .set("User-Agent", USER_AGENT)
        .timeout(Duration::from_secs(15))
        .call()
        .ok()
        .and_then(|r| r.into_json::<serde_json::Value>().ok())
        .and_then(|v| v.get("tag_name").and_then(|t| t.as_str()).map(|s| s.trim_start_matches('v').to_string()));
    let harness = ureq::get("https://api.github.com/repos/deepseek-ai/deepseek-harness/commits/master")
        .set("User-Agent", USER_AGENT)
        .timeout(Duration::from_secs(15))
        .call()
        .ok()
        .and_then(|r| r.into_json::<serde_json::Value>().ok())
        .and_then(|v| v.get("sha").and_then(|s| s.as_str()).map(str::to_string));
    (shell, harness)
}

/// Hard-update the runtime ignoring DSH_SKIP_UPDATE (the panel upgrade path):
/// git checkout refresh, or ZIP re-download when there is no .git.
fn force_update_runtime(
    git: Option<&Path>,
    runtime: &Path,
    on_status: &dyn Fn(&str),
) -> Result<RuntimeState, String> {
    if runtime.join(".git").exists() {
        let git = git.ok_or("更新需要 git，但系统中没有找到 git")?;
        on_status("正在拉取官方最新代码…");
        git_out_timeout(git, runtime, &["fetch", "--depth", "1", "origin", SOURCE_BRANCH], Duration::from_secs(240))
            .map_err(|e| format!("拉取更新失败：{e}"))?;
        let head = git_out(git, runtime, &["rev-parse", "HEAD"]);
        let fetched = git_out(git, runtime, &["rev-parse", "FETCH_HEAD"]);
        if head.is_some() && fetched.is_some() && head != fetched {
            on_status("发现新版本，正在更新…");
            git_out_timeout(git, runtime, &["reset", "--hard", "FETCH_HEAD"], Duration::from_secs(120))
                .map_err(|e| format!("更新代码失败：{e}"))?;
            return Ok(RuntimeState::Updated);
        }
        return Ok(RuntimeState::Unchanged);
    }
    on_status("正在下载官方最新代码…");
    let sha = latest_sha(git).unwrap_or_default();
    download_zip(runtime, &sha)?;
    Ok(RuntimeState::Fresh)
}

/// Run the full upgrade chain in the background: refresh code, reinstall
/// deps when the revision changed, rebuild, restart the server, reload the
/// page. Progress and status are written into `shared` (read by the panel).
fn upgrade_harness(
    app: AppHandle,
    shared: SharedUpdate,
    git: Option<PathBuf>,
    runtime: PathBuf,
    tools: PathBuf,
    pnpm: PathBuf,
    port: u16,
) {
    let progress = move |pct: u32, status: &str| {
        if let Ok(mut s) = shared.0.lock() {
            s.upgrading = true;
            s.progress = pct;
            s.status = status.to_string();
        }
    };
    std::thread::spawn(move || {
        progress(2, "正在拉取官方最新代码…");
        let state = force_update_runtime(git.as_deref(), &runtime, &|s| progress(5, s));
        let state = match state {
            Ok(s) => s,
            Err(e) => {
                progress(100, &format!("更新失败：{e}"));
                return;
            }
        };
        let sha = current_sha(git.as_deref(), &runtime).unwrap_or_default();
        if state != RuntimeState::Unchanged || !runtime.join("node_modules").exists() {
            progress(15, "正在安装依赖（可能需要几分钟）…");
            let window = app.get_webview_window("main");
            let result = window
                .map(|w| install_deps(&pnpm, &runtime, &tools, &w))
                .unwrap_or(Ok(()));
            if let Err(e) = result {
                progress(100, &format!("安装依赖失败：{e}"));
                return;
            }
        }
        progress(40, "正在构建（可能需要 10-20 分钟）…");
        let window = app.get_webview_window("main");
        let result = window
            .map(|w| build_repo(&pnpm, &runtime, &tools, &sha, &w))
            .unwrap_or(Ok(()));
        if let Err(e) = result {
            progress(100, &format!("构建失败：{e}"));
            return;
        }
        // Restart the server so the new code takes effect.
        progress(85, "正在重启服务…");
        if let Some(state) = app.try_state::<ServerState>() {
            let child = {
                let mut guard = state.0.lock().expect("server slot lock");
                guard.take()
            };
            if let Some(child) = child {
                kill_tree(child.id());
            }
            let mut slot = state.0.lock().expect("server slot lock");
            *slot = start_server(&pnpm, &runtime, &tools, port);
        }
        let deadline = Instant::now() + STARTUP_TIMEOUT;
        while Instant::now() < deadline {
            if server_ready(port) {
                break;
            }
            thread::sleep(Duration::from_millis(750));
        }
        progress(100, if server_ready(port) { "更新完成 ✓" } else { "服务重启失败，请手动重新打开应用" });
        if let Some(window) = app.get_webview_window("main") {
            // Reload the page against the restarted server.
            thread::sleep(Duration::from_secs(1));
            eval_js(&window, "location.reload()");
        }
    });
}

/// Tiny loopback HTTP bridge the injected update panel talks to:
///   GET  /status            -> UpdateSnapshot JSON
///   POST /upgrade-harness   -> start the background upgrade (202)
///   POST /open-release      -> open this repo's Releases page
fn start_update_bridge(
    shared: SharedUpdate,
    app: AppHandle,
    git: Option<PathBuf>,
    runtime: PathBuf,
    tools: PathBuf,
    pnpm: PathBuf,
    port: u16,
) {
    std::thread::spawn(move || {
        let listener = match std::net::TcpListener::bind(("127.0.0.1", 3091)) {
            Ok(l) => l,
            Err(_) => return,
        };
        for stream in listener.incoming().flatten() {
            let shared = shared.clone_inner_for_bridge();
            let app = app.clone();
            let git = git.clone();
            let runtime = runtime.clone();
            let tools = tools.clone();
            let pnpm = pnpm.clone();
            std::thread::spawn(move || {
                let mut stream = stream;
                let mut reader = std::io::BufReader::new(stream.try_clone().unwrap());
                let mut line = String::new();
                let _ = reader.read_line(&mut line);
                let method = line.split_whitespace().next().unwrap_or("").to_string();
                let path = line.split_whitespace().nth(1).unwrap_or("/").to_string();
                let body = match path.as_str() {
                    "/upgrade-harness" if method == "POST" => {
                        let upgrading = shared.0.lock().map(|s| s.upgrading).unwrap_or(false);
                        if !upgrading {
                            upgrade_harness(app, shared, git, runtime, tools, pnpm, port);
                            "HTTP/1.1 202 Accepted\r\nContent-Length: 2\r\nConnection: close\r\n\r\nok".to_string()
                        } else {
                            "HTTP/1.1 409 Conflict\r\nContent-Length: 2\r\nConnection: close\r\n\r\nno".to_string()
                        }
                    }
                    "/open-release" if method == "POST" => {
                        #[cfg(windows)]
                        let _ = Command::new("cmd")
                            .args(["/c", "start", "", "https://github.com/Myoontyee/deepseek-harness-desktop/releases/latest"])
                            .creation_flags(0x0800_0000)
                            .spawn();
                        #[cfg(not(windows))]
                        let _ = Command::new("open").arg("https://github.com/Myoontyee/deepseek-harness-desktop/releases/latest").spawn();
                        "HTTP/1.1 200 OK\r\nContent-Length: 2\r\nConnection: close\r\n\r\nok".to_string()
                    }
                    "/status" => {
                        let json = shared.0.lock().map(|s| s.json()).unwrap_or_else(|_| "{}".into());
                        format!("HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}", json.len(), json)
                    }
                    _ => "HTTP/1.1 404 Not Found\r\nContent-Length: 2\r\nConnection: close\r\n\r\nno".to_string(),
                };
                let _ = std::io::Write::write_all(&mut stream, body.as_bytes());
                Ok::<(), std::io::Error>(())
            });
        }
    });
}

trait CloneForBridge {
    fn clone_inner_for_bridge(&self) -> Self;
}
impl CloneForBridge for SharedUpdate {
    fn clone_inner_for_bridge(&self) -> Self {
        SharedUpdate(std::sync::Mutex::new(
            self.0.lock().map(|s| UpdateSnapshot {
                shell_current: s.shell_current.clone(),
                shell_latest: s.shell_latest.clone(),
                harness_current: s.harness_current.clone(),
                harness_latest: s.harness_latest.clone(),
                upgrading: s.upgrading,
                progress: s.progress,
                status: s.status.clone(),
            }).unwrap_or_else(|_| UpdateSnapshot {
                shell_current: String::new(),
                shell_latest: None,
                harness_current: String::new(),
                harness_latest: None,
                upgrading: false,
                progress: 0,
                status: String::new(),
            }),
        ))
    }
}

fn main() {
    let server = ServerState(std::sync::Mutex::new(None));
    tauri::Builder::default()
        // Single instance: a second launch focuses the existing window instead
        // of opening a duplicate (which previously double-started servers).
        .plugin(tauri_plugin_single_instance::init(|app, _args, _cwd| {
            if let Some(window) = app.get_webview_window("main") {
                let _ = window.show();
                let _ = window.unminimize();
                let _ = window.set_focus();
            }
        }))
        .manage(server)
        .setup(|app| {
            let handle = app.handle().clone();
            let window = app.get_webview_window("main").expect("main window must exist");

            // Closing the window hides it to the tray instead of exiting: the
            // server keeps serving, and the app stays one click away.
            let tray_window = window.clone();
            window.on_window_event(move |event| {
                if let WindowEvent::CloseRequested { api, .. } = event {
                    api.prevent_close();
                    let _ = tray_window.hide();
                }
            });

            // System tray: left-click shows the window, menu shows/exits.
            let show_item =
                MenuItem::with_id(app, "show", "显示主窗口", true, None::<&str>)?;
            let quit_item =
                MenuItem::with_id(app, "quit", "退出 DeepSeek Harness", true, None::<&str>)?;
            let menu = Menu::with_items(app, &[&show_item, &quit_item])?;
            let _tray = TrayIconBuilder::with_id("dsh-tray")
                .icon(app.default_window_icon().expect("app icon missing").clone())
                .tooltip("DeepSeek Harness")
                .menu(&menu)
                .show_menu_on_left_click(false)
                .on_menu_event(|app, event| match event.id.as_ref() {
                    "show" => {
                        if let Some(win) = app.get_webview_window("main") {
                            let _ = win.show();
                            let _ = win.unminimize();
                            let _ = win.set_focus();
                        }
                    }
                    "quit" => app.exit(0),
                    _ => {}
                })
                .on_tray_icon_event(|tray, event| {
                    if let TrayIconEvent::Click {
                        button: MouseButton::Left,
                        button_state: MouseButtonState::Up,
                        ..
                    } = event
                    {
                        if let Some(win) = tray.app_handle().get_webview_window("main") {
                            let _ = win.show();
                            let _ = win.unminimize();
                            let _ = win.set_focus();
                        }
                    }
                })
                .build(app)?;

            thread::spawn(move || run(&handle, window));
            Ok(())
        })
        .build(tauri::generate_context!())
        .expect("failed to build DeepSeek Harness desktop entry")
        .run(|app, event| {
            if matches!(event, tauri::RunEvent::Exit) {
                // Stop the server together with the app: no orphan processes,
                // no visible console. A server the user started themselves
                // (port already up on launch) is left untouched.
                let child = {
                    let state = app.state::<ServerState>();
                    let mut guard = state.0.lock().expect("server slot lock");
                    guard.take()
                };
                if let Some(child) = child {
                    kill_tree(child.id());
                }
            }
        });
}
