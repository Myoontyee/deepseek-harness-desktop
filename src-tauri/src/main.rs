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
use std::os::windows::process::CommandExt;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::thread;
use std::time::{Duration, Instant};

use tauri::{AppHandle, Manager, WebviewWindow};

const SOURCE_URL: &str = "https://github.com/deepseek-ai/deepseek-harness.git";
const SOURCE_BRANCH: &str = "main";
const API_LATEST: &str = "https://api.github.com/repos/deepseek-ai/deepseek-harness/commits/main";
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
    if let Some(git) = find_on_path("git.exe") {
        return Some(git);
    }
    [
        r"C:\Program Files\Git\cmd\git.exe",
        r"C:\Program Files (x86)\Git\cmd\git.exe",
        r"C:\Program Files\Git\bin\git.exe",
    ]
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

/// Download the repository as a ZIP (codeload) and extract it into `runtime`,
/// recording the revision so later launches can skip the download.
fn download_zip(runtime: &Path, sha: &str) -> Result<(), String> {
    let parent = runtime.parent().ok_or("runtime 路径没有父目录")?;
    let url = format!(
        "https://codeload.github.com/deepseek-ai/deepseek-harness/zip/refs/heads/{SOURCE_BRANCH}"
    );
    let response = ureq::get(&url)
        .set("User-Agent", USER_AGENT)
        .timeout(Duration::from_secs(900))
        .call()
        .map_err(|e| format!("下载失败：{e}"))?;
    let mut bytes = Vec::new();
    response
        .into_reader()
        .take(2 * 1024 * 1024 * 1024)
        .read_to_end(&mut bytes)
        .map_err(|e| format!("读取下载内容失败：{e}"))?;

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
        on_status("首次运行：正在下载 DeepSeek Harness 最新代码…");
        if let Some(git) = git {
            if runtime.exists() {
                let _ = std::fs::remove_dir_all(runtime);
            }
            let parent = runtime.parent().ok_or("runtime 路径没有父目录")?;
            let clone_target = runtime.to_str().unwrap_or(RUNTIME_DIR_NAME);
            git_out(
                git,
                parent,
                &["clone", "--depth", "1", "--branch", SOURCE_BRANCH, &source, clone_target],
            )
            .ok_or("下载代码失败（git clone）")?;
            Ok(RuntimeState::Fresh)
        } else {
            let sha = latest_sha(git).unwrap_or_default();
            download_zip(runtime, &sha)?;
            Ok(RuntimeState::Fresh)
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
    let mut buf = [0u8; 64];
    let Ok(n) = stream.read(&mut buf) else {
        return false;
    };
    String::from_utf8_lossy(&buf[..n]).contains("200")
}

/// Start `pnpm dsh web` with no console window at all (CREATE_NO_WINDOW),
/// redirecting its output to `server.log` for diagnostics.
///
/// The server is spawned directly (no cmd/`start` indirection, so no shell
/// quoting pitfalls) and its lifecycle is tied to the app: when the window
/// closes, the whole process tree is killed.
fn start_server(pnpm: &Path, runtime: &Path, tools: &Path, port: u16) -> Option<std::process::Child> {
    let log = std::fs::File::create(runtime.join("server.log")).ok()?;
    let stdout = Stdio::from(log.try_clone().ok()?);
    let stderr = Stdio::from(log);
    Command::new(pnpm)
        .args(["dsh", "web", "--port", &port.to_string()])
        .current_dir(runtime)
        .env("PATH", prepend_path(tools))
        .creation_flags(0x0800_0000) // CREATE_NO_WINDOW: no console, ever
        .stdout(stdout)
        .stderr(stderr)
        .spawn()
        .ok()
}

/// Kill a process tree on Windows (`taskkill /T`), used to stop the server
/// together with the app.
fn kill_tree(pid: u32) {
    let _ = Command::new("taskkill")
        .args(["/PID", &pid.to_string(), "/T", "/F"])
        .creation_flags(0x0800_0000)
        .spawn();
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
fn show_version(window: &WebviewWindow, runtime: &Path) {
    let harness = std::fs::read_to_string(runtime.join("package.json"))
        .ok()
        .and_then(|text| serde_json::from_str::<serde_json::Value>(&text).ok())
        .and_then(|json| json.get("version").and_then(|v| v.as_str()).map(str::to_string))
        .unwrap_or_else(|| "unknown".to_string());
    eval_js(window, &format!("setVersion({}, {})", js_string(&harness), js_string(env!("CARGO_PKG_VERSION"))));
}

// --- boot flow -------------------------------------------------------------

fn run(app: &AppHandle, window: WebviewWindow) {
    // Give the embedded boot page a moment to parse before the first eval.
    thread::sleep(Duration::from_millis(1200));

    let tools = tools_dir();
    let pnpm = tools.join("pnpm.exe");
    let node = tools.join("node").join("node.exe");
    if !pnpm.exists() || !node.exists() {
        return fail(&window, "运行时组件缺失：未找到 tools/pnpm.exe 或 tools/node，请重新安装应用");
    }

    let runtime = runtime_dir();
    let p = port();
    let git = find_git();

    show_version(&window, &runtime);

    let state = match ensure_runtime(git.as_deref(), &runtime, &|s| set_status(&window, s)) {
        Ok(state) => state,
        Err(e) => return fail(&window, &format!("准备运行环境失败：{e}")),
    };

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
}

/// The app-started server child (None when an already-running server was reused).
struct ServerState(std::sync::Mutex<Option<std::process::Child>>);

fn main() {
    let server = ServerState(std::sync::Mutex::new(None));
    tauri::Builder::default()
        .manage(server)
        .setup(|app| {
            let handle = app.handle().clone();
            let window = app.get_webview_window("main").expect("main window must exist");
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
