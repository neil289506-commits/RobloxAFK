#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

use serde::{Deserialize, Serialize};
use std::{
    ffi::OsStr,
    fs::{self, File},
    path::PathBuf,
    process::{Child, Command, Stdio},
    sync::Mutex,
    thread,
    time::{Duration, Instant},
};
use tauri::{
    menu::{Menu, MenuItem},
    tray::{MouseButton, MouseButtonState, TrayIconBuilder, TrayIconEvent},
    AppHandle, Emitter, Manager, State, WindowEvent,
};

const AHK_SCRIPT: &str = include_str!("../scripts/afk.ahk");
const PY_SCRIPT: &str = include_str!("../scripts/afk.py");

#[derive(Clone, Serialize)]
struct RunInfo {
    mode: String,
    key: String,
    target_pid: Option<u32>,
    worker_pid: u32,
    worker: String,
}

#[derive(Default)]
struct Worker {
    child: Option<Child>,
    info: Option<RunInfo>,
}
struct AppState(Mutex<Worker>);

#[derive(Deserialize)]
struct Cfg {
    mode: String,
    key: String,
    action: String,
    hold_ms: u64,
    taps: u32,
    interval_s: u64,
    pid: Option<u32>,
}

#[derive(Serialize)]
struct Env {
    ahk: Option<String>,
    python: Option<String>,
    pywinauto: Option<String>,
}

// ---------- helpers ----------
fn cmd<S: AsRef<OsStr>>(program: S) -> Command {
    let mut c = Command::new(program);
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        c.creation_flags(0x0800_0000); // CREATE_NO_WINDOW
    }
    c
}

fn work_dir() -> PathBuf {
    let d = std::env::temp_dir().join("roblox_afk");
    let _ = fs::create_dir_all(&d);
    d
}

/// 以 `#Requires AutoHotkey v2.0` 探測：v2 → exit code 20，其餘（v1 / 錯誤）皆非 20
fn is_ahk_v2(exe: &PathBuf) -> bool {
    let probe = work_dir().join("probe.ahk");
    if fs::write(&probe, "#Requires AutoHotkey v2.0\nExitApp 20\n").is_err() {
        return false;
    }
    cmd(exe)
        .arg("/ErrorStdOut")
        .arg(&probe)
        .status()
        .map(|s| s.code() == Some(20))
        .unwrap_or(false)
}

fn find_ahk() -> Option<PathBuf> {
    let mut cands: Vec<PathBuf> = Vec::new();
    for (var, rest) in [
        ("ProgramFiles", "AutoHotkey\\v2\\AutoHotkey64.exe"),
        ("ProgramFiles", "AutoHotkey\\v2\\AutoHotkey32.exe"),
        ("LOCALAPPDATA", "Programs\\AutoHotkey\\v2\\AutoHotkey64.exe"),
        ("LOCALAPPDATA", "Programs\\AutoHotkey\\v2\\AutoHotkey32.exe"),
        ("ProgramFiles", "AutoHotkey\\AutoHotkey64.exe"),
    ] {
        if let Some(base) = std::env::var_os(var) {
            cands.push(PathBuf::from(base).join(rest));
        }
    }
    if let Ok(o) = cmd("where").arg("AutoHotkey64.exe").output() {
        for l in String::from_utf8_lossy(&o.stdout).lines() {
            cands.push(PathBuf::from(l.trim()));
        }
    }
    cands.into_iter().find(|p| p.is_file() && is_ahk_v2(p))
}

fn find_python() -> Option<(String, Vec<&'static str>)> {
    for (p, a) in [("python", vec![]), ("py", vec!["-3"])] {
        let ok = cmd(p)
            .args(&a)
            .arg("--version")
            .output()
            .map(|o| o.status.success())
            .unwrap_or(false);
        if ok {
            return Some((p.to_string(), a));
        }
    }
    None
}

fn pywinauto_ver(py: &(String, Vec<&'static str>)) -> Option<String> {
    let o = cmd(&py.0)
        .args(&py.1)
        .args(["-c", "import pywinauto;print(pywinauto.__version__)"])
        .output()
        .ok()?;
    o.status
        .success()
        .then(|| String::from_utf8_lossy(&o.stdout).trim().to_string())
}

fn show_main(app: &AppHandle) {
    if let Some(w) = app.get_webview_window("main") {
        let _ = w.unminimize();
        let _ = w.show();
        let _ = w.set_focus();
    }
}

/// 先用 stop.flag 讓腳本自行放開按鍵並結束；超過 4 秒才強制終止
fn stop_worker(st: &AppState) {
    let mut w = st.0.lock().unwrap();
    if let Some(mut c) = w.child.take() {
        let flag = work_dir().join("stop.flag");
        let _ = fs::write(&flag, "1");
        let t = Instant::now();
        while t.elapsed() < Duration::from_secs(4) {
            if matches!(c.try_wait(), Ok(Some(_))) {
                break;
            }
            thread::sleep(Duration::from_millis(100));
        }
        if matches!(c.try_wait(), Ok(None)) {
            let _ = c.kill();
            let _ = c.wait();
        }
        let _ = fs::remove_file(flag);
    }
    w.info = None;
}

// ---------- commands ----------
#[tauri::command]
async fn check_env() -> Env {
    let py = find_python();
    let pyw = py.as_ref().and_then(pywinauto_ver);
    Env {
        ahk: find_ahk().map(|p| p.to_string_lossy().into_owned()),
        python: py.map(|p| p.0),
        pywinauto: pyw,
    }
}

#[tauri::command]
async fn install_pywinauto() -> Result<String, String> {
    let py = find_python().ok_or("找不到 Python，請先安裝 Python 3（安裝時勾選 Add to PATH）")?;
    let o = cmd(&py.0)
        .args(&py.1)
        .args(["-m", "pip", "install", "--upgrade", "pywinauto"])
        .output()
        .map_err(|e| e.to_string())?;
    if o.status.success() {
        Ok("pywinauto 安裝完成".into())
    } else {
        Err(String::from_utf8_lossy(&o.stderr).into_owned())
    }
}

#[tauri::command]
async fn detect_roblox() -> Vec<u32> {
    let o = match cmd("tasklist")
        .args(["/FI", "IMAGENAME eq RobloxPlayerBeta.exe", "/FO", "CSV", "/NH"])
        .output()
    {
        Ok(o) => o,
        Err(_) => return vec![],
    };
    String::from_utf8_lossy(&o.stdout)
        .lines()
        .filter_map(|l| l.split("\",\"").nth(1)?.trim_matches('"').parse().ok())
        .collect()
}

#[tauri::command]
async fn start(cfg: Cfg, app: AppHandle, state: State<'_, AppState>) -> Result<RunInfo, String> {
    let key = cfg.key.trim().to_string();
    if key.is_empty() || key.len() > 12 || !key.chars().all(|c| c.is_ascii_alphanumeric()) {
        return Err("按鍵格式不正確（例：w、Space、Left、F5）".into());
    }
    let hold = cfg.hold_ms.clamp(50, 60_000);
    let taps = cfg.taps.clamp(1, 1000);
    let interval = cfg.interval_s.clamp(1, 86_400);
    let action = if cfg.action == "tap" { "tap" } else { "hold" };

    stop_worker(&state);
    let dir = work_dir();
    let stop = dir.join("stop.flag");
    let _ = fs::remove_file(&stop);
    let log = dir.join("worker.log");
    let errf = File::create(&log).map_err(|e| e.to_string())?;
    let nums = [hold.to_string(), taps.to_string(), interval.to_string()];

    let (mut c, target_pid) = if cfg.mode == "ahk" {
        let exe = find_ahk()
            .ok_or("找不到 AutoHotkey v2，請先安裝（winget install AutoHotkey.AutoHotkey）")?;
        let script = dir.join("afk.ahk");
        fs::write(&script, AHK_SCRIPT).map_err(|e| e.to_string())?;
        let mut c = cmd(exe);
        c.arg(script);
        (c, None)
    } else {
        let pid = cfg.pid.filter(|p| *p > 0).ok_or("請輸入有效的 PID")?;
        let py = find_python().ok_or("找不到 Python 3")?;
        let script = dir.join("afk.py");
        fs::write(&script, PY_SCRIPT).map_err(|e| e.to_string())?;
        let mut c = cmd(&py.0);
        c.args(&py.1).arg(script).env("PYTHONUTF8", "1");
        (c, Some(pid))
    };
    c.arg(&key).arg(action).args(&nums).arg(&stop);
    if let Some(p) = target_pid {
        c.arg(p.to_string());
    }
    c.stdout(Stdio::null()).stderr(Stdio::from(errf));

    let mut child = c.spawn().map_err(|e| format!("無法啟動：{e}"))?;
    thread::sleep(Duration::from_millis(1800));
    if let Ok(Some(status)) = child.try_wait() {
        let msg = fs::read_to_string(&log).unwrap_or_default();
        return Err(format!("腳本啟動失敗（{status}）：{}", msg.trim()));
    }

    let worker = String::from(if cfg.mode == "ahk" { "AutoHotkey v2" } else { "Python / pywinauto" });
    let info = RunInfo { mode: cfg.mode.clone(), key, target_pid, worker_pid: child.id(), worker };
    {
        let mut w = state.0.lock().unwrap();
        w.child = Some(child);
        w.info = Some(info.clone());
    }
    if let Some(win) = app.get_webview_window("main") {
        if cfg.mode == "ahk" {
            let _ = win.minimize(); // 腳本會把 Roblox 切到前景
        } else {
            let _ = win.hide(); // 縮到系統匣
        }
    }
    Ok(info)
}

#[tauri::command]
async fn stop(state: State<'_, AppState>) -> Result<(), String> {
    stop_worker(&state);
    Ok(())
}

#[tauri::command]
fn status(state: State<'_, AppState>) -> Option<RunInfo> {
    state.0.lock().unwrap().info.clone()
}

#[tauri::command]
async fn quit_app(app: AppHandle, state: State<'_, AppState>) -> Result<(), String> {
    stop_worker(&state);
    app.exit(0);
    Ok(())
}

// ---------- tray ----------
fn setup_tray(app: &AppHandle) -> tauri::Result<()> {
    let show = MenuItem::with_id(app, "show", "顯示視窗", true, None::<&str>)?;
    let quit = MenuItem::with_id(app, "quit", "停止並結束…", true, None::<&str>)?;
    let menu = Menu::with_items(app, &[&show, &quit])?;
    let mut b = TrayIconBuilder::with_id("main-tray")
        .tooltip("Roblox AFK")
        .menu(&menu)
        .show_menu_on_left_click(false)
        .on_menu_event(|app, event| match event.id.as_ref() {
            "show" => show_main(app),
            "quit" => {
                show_main(app);
                let _ = app.emit("quit-requested", ());
            }
            _ => {}
        })
        .on_tray_icon_event(|tray, event| {
            if let TrayIconEvent::Click { button: MouseButton::Left, button_state: MouseButtonState::Up, .. } = event {
                show_main(tray.app_handle());
            }
        });
    if let Some(icon) = app.default_window_icon() {
        b = b.icon(icon.clone());
    }
    b.build(app)?;
    Ok(())
}

fn main() {
    tauri::Builder::default()
        .manage(AppState(Mutex::new(Worker::default())))
        .invoke_handler(tauri::generate_handler![
            check_env, install_pywinauto, detect_roblox, start, stop, status, quit_app
        ])
        .on_window_event(|window, event| {
            if let WindowEvent::CloseRequested { api, .. } = event {
                let busy = window.state::<AppState>().0.lock().unwrap().info.is_some();
                if busy {
                    api.prevent_close(); // 有行程執行中：改走三次確認
                    let _ = window.emit("quit-requested", ());
                }
            }
        })
        .setup(|app| {
            setup_tray(app.handle())?;
            let h = app.handle().clone();
            thread::spawn(move || loop {
                thread::sleep(Duration::from_secs(2));
                let st = h.state::<AppState>();
                let ended = {
                    let mut w = st.0.lock().unwrap();
                    let r = w.child.as_mut().map(|c| c.try_wait());
                    match r {
                        Some(Ok(Some(_))) => {
                            w.child = None;
                            w.info = None;
                            true
                        }
                        _ => false,
                    }
                };
                if ended {
                    show_main(&h);
                    let _ = h.emit("worker-exited", ());
                }
            });
            Ok(())
        })
        .run(tauri::generate_context!())
        .expect("error while running tauri application");
}
