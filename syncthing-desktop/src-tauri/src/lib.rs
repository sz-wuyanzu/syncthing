use std::{
    env,
    net::TcpListener,
    path::PathBuf,
    process::{Child, Command, Stdio},
    sync::{Arc, Mutex},
    thread,
    time::{Duration, Instant},
};

#[cfg(windows)]
use std::os::windows::process::CommandExt;

use tauri::{
    AppHandle, Manager, State, WindowEvent,
    menu::{Menu, MenuItem},
    tray::TrayIconBuilder,
};

#[cfg(target_os = "macos")]
use tauri::tray::{MouseButton, MouseButtonState, TrayIconEvent};

#[cfg(target_os = "macos")]
mod status;

const GUI_URL: &str = "http://127.0.0.1:8384/";
#[cfg(target_os = "macos")]
const STATUS_PANEL_WIDTH: f64 = 340.0;
#[cfg(target_os = "macos")]
const STATUS_PANEL_HEIGHT: f64 = 410.0;

#[derive(Clone, Default)]
struct DesktopState {
    processes: Arc<Mutex<Option<ManagedDaemon>>>,
    #[cfg(target_os = "macos")]
    status_client: Arc<Mutex<Option<Arc<status::StatusClient>>>>,
    #[cfg(target_os = "macos")]
    status_error: Arc<Mutex<Option<String>>>,
}

struct ManagedDaemon {
    child: Child,
    api_key: String,
    gui_ready: bool,
    gui_url: String,
    ping_url: String,
    shutdown_url: String,
}

#[tauri::command]
async fn connect_to_syncthing(state: State<'_, DesktopState>) -> Result<String, String> {
    let processes = state.processes.clone();
    #[cfg(target_os = "macos")]
    let status_client = state.status_client.clone();
    #[cfg(target_os = "macos")]
    let status_error = state.status_error.clone();
    let result =
        tauri::async_runtime::spawn_blocking(move || connect_to_syncthing_inner(processes))
            .await
            .map_err(|err| format!("Could not start Syncthing: {err}"))??;

    #[cfg(target_os = "macos")]
    {
        let managed_api = state
            .processes
            .lock()
            .map_err(|_| "Syncthing 进程状态不可用".to_string())?
            .as_ref()
            .map(|daemon| status::StatusClient::new(&daemon.gui_url, &daemon.api_key));
        let discovered_api = match managed_api {
            Some(client) => Ok(client),
            None => status::StatusClient::discover_external(),
        };
        match discovered_api {
            Ok(client) => {
                *status_client
                    .lock()
                    .map_err(|_| "Syncthing 状态数据不可用".to_string())? = Some(Arc::new(client));
                *status_error
                    .lock()
                    .map_err(|_| "Syncthing 状态数据不可用".to_string())? = None;
            }
            Err(error) => {
                *status_client
                    .lock()
                    .map_err(|_| "Syncthing 状态数据不可用".to_string())? = None;
                *status_error
                    .lock()
                    .map_err(|_| "Syncthing 状态数据不可用".to_string())? = Some(error);
            }
        }
    }

    Ok(result)
}

#[cfg(target_os = "macos")]
#[tauri::command]
async fn get_sync_status(state: State<'_, DesktopState>) -> Result<status::SyncStatus, String> {
    let client = state
        .status_client
        .lock()
        .map_err(|_| "Syncthing 状态数据不可用".to_string())?
        .clone();
    let client = match client {
        Some(client) => client,
        None => {
            return Err(state
                .status_error
                .lock()
                .map_err(|_| "Syncthing 状态数据不可用".to_string())?
                .clone()
                .unwrap_or_else(|| "Syncthing 尚未连接，暂时无法读取同步状态".into()));
        }
    };
    tauri::async_runtime::spawn_blocking(move || client.fetch())
        .await
        .map_err(|error| format!("读取 Syncthing 同步状态失败：{error}"))?
}

#[cfg(target_os = "macos")]
#[tauri::command]
async fn set_sync_paused(state: State<'_, DesktopState>, paused: bool) -> Result<(), String> {
    let client = state
        .status_client
        .lock()
        .map_err(|_| "Syncthing 状态数据不可用".to_string())?
        .clone()
        .ok_or_else(|| "同步状态不可用，请打开 Syncthing 主界面管理同步".to_string())?;
    tauri::async_runtime::spawn_blocking(move || client.set_paused(paused))
        .await
        .map_err(|error| format!("更新 Syncthing 暂停状态失败：{error}"))?
}

#[cfg(target_os = "macos")]
#[tauri::command]
fn open_main_window(app: AppHandle) {
    show_window(&app);
}

#[cfg(target_os = "macos")]
#[tauri::command]
fn hide_status_panel(app: AppHandle) {
    if let Some(window) = app.get_webview_window("status-panel") {
        if let Err(error) = window.hide() {
            eprintln!("Could not hide status panel: {error}");
        }
    }
}

fn connect_to_syncthing_inner(
    processes: Arc<Mutex<Option<ManagedDaemon>>>,
) -> Result<String, String> {
    let mut managed = processes
        .lock()
        .map_err(|_| "Syncthing process state is unavailable".to_string())?;

    if let Some(daemon) = managed.as_mut() {
        if daemon
            .child
            .try_wait()
            .map_err(|err| format!("Could not inspect Syncthing process: {err}"))?
            .is_some()
        {
            *managed = None;
        } else {
            return wait_for_managed_syncthing(&mut *managed);
        }
    }

    if syncthing_is_ready(GUI_URL, None) {
        return Ok(GUI_URL.to_string());
    }

    let listener = TcpListener::bind("127.0.0.1:0")
        .map_err(|err| format!("Could not find an available local GUI port: {err}"))?;
    let port = listener
        .local_addr()
        .map_err(|err| format!("Could not determine local GUI port: {err}"))?
        .port();
    drop(listener);
    let address = format!("127.0.0.1:{port}");
    let gui_url = format!("http://{address}/");
    let ping_url = format!("{gui_url}rest/system/ping");
    let shutdown_url = format!("{gui_url}rest/system/shutdown");
    let api_key = uuid::Uuid::new_v4().simple().to_string();
    let binary = syncthing_binary();
    let mut command = Command::new(binary);
    command
        .arg("--no-browser")
        .env("STGUIADDRESS", &address)
        .env("STGUIAPIKEY", &api_key)
        .env("STLOGFILE", "default")
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null());

    #[cfg(windows)]
    command
        .env("STHIDECONSOLE", "true")
        .creation_flags(0x08000000);

    let child = command.spawn().map_err(|err| {
        format!("Could not start Syncthing. Install it or set SYNCTHING_BIN: {err}")
    })?;
    *managed = Some(ManagedDaemon {
        child,
        api_key,
        gui_ready: false,
        gui_url,
        ping_url,
        shutdown_url,
    });

    wait_for_managed_syncthing(&mut *managed)
}

fn syncthing_binary() -> std::ffi::OsString {
    if let Some(binary) = env::var_os("SYNCTHING_BIN") {
        return binary;
    }

    #[cfg(target_os = "macos")]
    let candidates = vec![
        PathBuf::from("/opt/homebrew/bin/syncthing"),
        PathBuf::from("/usr/local/bin/syncthing"),
        PathBuf::from("/opt/local/bin/syncthing"),
    ];

    #[cfg(target_os = "linux")]
    let candidates = vec![
        PathBuf::from("/usr/bin/syncthing"),
        PathBuf::from("/usr/local/bin/syncthing"),
        PathBuf::from("/snap/bin/syncthing"),
    ];

    #[cfg(windows)]
    let candidates = env::var_os("ProgramFiles")
        .map(|path| vec![PathBuf::from(path).join("Syncthing/syncthing.exe")])
        .unwrap_or_default();

    #[cfg(not(any(target_os = "macos", target_os = "linux", windows)))]
    let candidates: Vec<PathBuf> = Vec::new();

    candidates
        .into_iter()
        .find(|path| path.is_file())
        .map(PathBuf::into_os_string)
        .unwrap_or_else(|| "syncthing".into())
}

fn wait_for_managed_syncthing(managed: &mut Option<ManagedDaemon>) -> Result<String, String> {
    let deadline = Instant::now() + Duration::from_secs(30);
    loop {
        let daemon = managed
            .as_mut()
            .ok_or_else(|| "Syncthing process state is unavailable".to_string())?;
        if syncthing_is_ready(&daemon.ping_url, Some(&daemon.api_key)) {
            daemon.gui_ready = true;
            return Ok(daemon.gui_url.clone());
        }
        if let Some(status) = daemon
            .child
            .try_wait()
            .map_err(|err| format!("Could not inspect Syncthing process: {err}"))?
        {
            *managed = None;
            return Err(format!(
                "Syncthing exited before its GUI became ready ({status})"
            ));
        }
        if Instant::now() >= deadline {
            return Err("Timed out waiting for Syncthing's GUI at 127.0.0.1:8384".into());
        }
        thread::sleep(Duration::from_millis(100));
    }
}

fn syncthing_is_ready(url: &str, api_key: Option<&str>) -> bool {
    let agent = ureq::AgentBuilder::new()
        .timeout(Duration::from_secs(2))
        .build();
    let mut request = agent.get(url);
    if let Some(api_key) = api_key {
        request = request.set("X-API-Key", api_key);
    }

    match request.call() {
        Ok(response) => response.header("X-Syncthing-ID").is_some(),
        Err(ureq::Error::Status(_, response)) => {
            api_key.is_none() && response.header("X-Syncthing-ID").is_some()
        }
        Err(ureq::Error::Transport(_)) => false,
    }
}

fn show_window(app: &AppHandle) {
    if let Some(window) = app.get_webview_window("main") {
        if let Err(err) = window.show() {
            eprintln!("Could not show Syncthing window: {err}");
        } else if let Err(err) = window.set_focus() {
            eprintln!("Could not focus Syncthing window: {err}");
        }
    }
}

#[cfg(target_os = "macos")]
fn toggle_status_panel(app: &AppHandle) {
    use tauri::PhysicalPosition;

    let Some(window) = app.get_webview_window("status-panel") else {
        return;
    };
    match window.is_visible() {
        Ok(true) => {
            if let Err(error) = window.hide() {
                eprintln!("Could not hide status panel: {error}");
            }
        }
        Ok(false) => {
            match window.primary_monitor() {
                Ok(Some(monitor)) => {
                    let scale = monitor.scale_factor();
                    let position = monitor.position();
                    let size = monitor.size();
                    let x = position.x + size.width as i32 - (STATUS_PANEL_WIDTH * scale) as i32;
                    let y = position.y + (28.0 * scale) as i32;
                    if let Err(error) = window.set_position(PhysicalPosition::new(x, y)) {
                        eprintln!("Could not position status panel: {error}");
                    }
                }
                Ok(None) => {}
                Err(error) => eprintln!("Could not find a monitor for status panel: {error}"),
            }
            if let Err(error) = window.show() {
                eprintln!("Could not show status panel: {error}");
            } else if let Err(error) = window.set_focus() {
                eprintln!("Could not focus status panel: {error}");
            }
        }
        Err(error) => eprintln!("Could not inspect status panel visibility: {error}"),
    }
}

fn quit_app(app: &AppHandle) {
    let app = app.clone();
    let processes = app.state::<DesktopState>().processes.clone();
    thread::spawn(move || {
        if let Err(err) = stop_managed_syncthing(processes) {
            if let Some(window) = app.get_webview_window("main") {
                if let Err(title_err) =
                    window.set_title(&format!("Syncthing - could not stop service: {err}"))
                {
                    eprintln!("Could not show shutdown error: {title_err}");
                }
                if let Err(show_err) = window.show() {
                    eprintln!("Could not show Syncthing window: {show_err}");
                }
            }
            return;
        }
        app.exit(0);
    });
}

fn stop_managed_syncthing(processes: Arc<Mutex<Option<ManagedDaemon>>>) -> Result<(), String> {
    let mut managed = processes
        .lock()
        .map_err(|_| "Syncthing process state is unavailable".to_string())?;
    let Some(daemon) = managed.as_mut() else {
        return Ok(());
    };

    if !daemon.gui_ready {
        return Err(
            "Syncthing's GUI was not verified; refusing an unverified shutdown request".into(),
        );
    }

    if daemon
        .child
        .try_wait()
        .map_err(|err| format!("Could not inspect Syncthing process: {err}"))?
        .is_some()
    {
        *managed = None;
        return Ok(());
    }

    let agent = ureq::AgentBuilder::new()
        .timeout(Duration::from_secs(5))
        .build();
    agent
        .post(&daemon.shutdown_url)
        .set("X-API-Key", &daemon.api_key)
        .call()
        .map_err(|err| format!("Shutdown request failed: {err}"))?;

    let deadline = Instant::now() + Duration::from_secs(10);
    loop {
        match daemon.child.try_wait() {
            Ok(Some(_)) => break,
            Ok(None) if Instant::now() < deadline => {
                thread::sleep(Duration::from_millis(100));
            }
            Ok(None) => return Err("shutdown is taking longer than expected".into()),
            Err(err) => return Err(format!("Could not inspect Syncthing process: {err}")),
        }
    }
    *managed = None;
    Ok(())
}

pub fn run() {
    let builder = tauri::Builder::default()
        .manage(DesktopState::default())
        .setup(|app| {
            #[cfg(target_os = "macos")]
            let open = MenuItem::with_id(app, "open", "打开 Syncthing", true, None::<&str>)?;
            #[cfg(not(target_os = "macos"))]
            let open = MenuItem::with_id(app, "open", "Open Syncthing", true, None::<&str>)?;
            #[cfg(target_os = "macos")]
            let toggle_panel =
                MenuItem::with_id(app, "toggle-panel", "显示/隐藏状态面板", true, None::<&str>)?;
            #[cfg(target_os = "macos")]
            let quit = MenuItem::with_id(app, "quit", "退出并停止同步", true, None::<&str>)?;
            #[cfg(not(target_os = "macos"))]
            let quit = MenuItem::with_id(app, "quit", "Quit and stop syncing", true, None::<&str>)?;
            #[cfg(target_os = "macos")]
            let menu = Menu::with_items(app, &[&toggle_panel, &open, &quit])?;
            #[cfg(not(target_os = "macos"))]
            let menu = Menu::with_items(app, &[&open, &quit])?;
            let icon = app
                .default_window_icon()
                .cloned()
                .ok_or("Syncthing window icon is missing")?;

            let tray = TrayIconBuilder::new()
                .icon(icon)
                .tooltip("Syncthing")
                .menu(&menu);
            #[cfg(target_os = "macos")]
            let tray = tray
                .show_menu_on_left_click(false)
                .on_tray_icon_event(|tray, event| {
                    if let TrayIconEvent::Click {
                        button: MouseButton::Left,
                        button_state: MouseButtonState::Up,
                        ..
                    } = event
                    {
                        #[cfg(target_os = "macos")]
                        toggle_status_panel(tray.app_handle());
                    }
                });
            tray.on_menu_event(|app, event| match event.id().as_ref() {
                "open" => show_window(app),
                "toggle-panel" => {
                    #[cfg(target_os = "macos")]
                    toggle_status_panel(app);
                }
                "quit" => quit_app(app),
                _ => {}
            })
            .build(app)?;

            #[cfg(target_os = "macos")]
            {
                use tauri::{WebviewUrl, webview::WebviewWindowBuilder};
                use window_vibrancy::{
                    NSVisualEffectMaterial, NSVisualEffectState, apply_vibrancy,
                };

                let panel = WebviewWindowBuilder::new(
                    app,
                    "status-panel",
                    WebviewUrl::App("panel.html".into()),
                )
                .title("Syncthing 同步状态")
                .inner_size(STATUS_PANEL_WIDTH, STATUS_PANEL_HEIGHT)
                .resizable(false)
                .decorations(false)
                .transparent(true)
                .shadow(true)
                .always_on_top(true)
                .skip_taskbar(true)
                .visible(false)
                .build()?;
                apply_vibrancy(
                    &panel,
                    NSVisualEffectMaterial::HudWindow,
                    Some(NSVisualEffectState::Active),
                    Some(12.0),
                )?;
                let panel_to_hide = panel.clone();
                panel.on_window_event(move |event| {
                    if let WindowEvent::Focused(false) = event {
                        if let Err(error) = panel_to_hide.hide() {
                            eprintln!("Could not hide status panel: {error}");
                        }
                    }
                });
            }

            if let Some(window) = app.get_webview_window("main") {
                let window_to_hide = window.clone();
                window.on_window_event(move |event| {
                    if let WindowEvent::CloseRequested { api, .. } = event {
                        api.prevent_close();
                        if let Err(err) = window_to_hide.hide() {
                            eprintln!("Could not hide Syncthing window: {err}");
                        }
                    }
                });
            }
            Ok(())
        });
    #[cfg(target_os = "macos")]
    let builder = builder.invoke_handler(tauri::generate_handler![
        connect_to_syncthing,
        get_sync_status,
        set_sync_paused,
        open_main_window,
        hide_status_panel
    ]);
    #[cfg(not(target_os = "macos"))]
    let builder = builder.invoke_handler(tauri::generate_handler![connect_to_syncthing]);
    builder
        .run(tauri::generate_context!())
        .expect("error while running Syncthing Desktop");
}
