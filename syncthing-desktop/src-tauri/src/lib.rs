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
    menu::{Menu, MenuItem},
    tray::TrayIconBuilder,
    AppHandle, Manager, State, WindowEvent,
};

const GUI_URL: &str = "http://127.0.0.1:8384/";

#[derive(Clone, Default)]
struct DesktopState(Arc<Mutex<Option<ManagedDaemon>>>);

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
    let processes = state.0.clone();
    tauri::async_runtime::spawn_blocking(move || connect_to_syncthing_inner(processes))
        .await
        .map_err(|err| format!("Could not start Syncthing: {err}"))?
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

fn wait_for_managed_syncthing(
    managed: &mut Option<ManagedDaemon>,
) -> Result<String, String> {
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
            return Err(format!("Syncthing exited before its GUI became ready ({status})"));
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

fn quit_app(app: &AppHandle) {
    let app = app.clone();
    let processes = app.state::<DesktopState>().0.clone();
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

fn stop_managed_syncthing(
    processes: Arc<Mutex<Option<ManagedDaemon>>>,
) -> Result<(), String> {
    let mut managed = processes
        .lock()
        .map_err(|_| "Syncthing process state is unavailable".to_string())?;
    let Some(daemon) = managed.as_mut() else {
        return Ok(());
    };

    if !daemon.gui_ready {
        return Err("Syncthing's GUI was not verified; refusing an unverified shutdown request"
            .into());
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
    tauri::Builder::default()
        .manage(DesktopState::default())
        .setup(|app| {
            let open = MenuItem::with_id(app, "open", "Open Syncthing", true, None::<&str>)?;
            let quit =
                MenuItem::with_id(app, "quit", "Quit and stop syncing", true, None::<&str>)?;
            let menu = Menu::with_items(app, &[&open, &quit])?;
            let icon = app
                .default_window_icon()
                .cloned()
                .ok_or("Syncthing window icon is missing")?;

            TrayIconBuilder::new()
                .icon(icon)
                .tooltip("Syncthing")
                .menu(&menu)
                .on_menu_event(|app, event| match event.id().as_ref() {
                    "open" => show_window(app),
                    "quit" => quit_app(app),
                    _ => {}
                })
                .build(app)?;

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
        })
        .invoke_handler(tauri::generate_handler![connect_to_syncthing])
        .run(tauri::generate_context!())
        .expect("error while running Syncthing Desktop");
}
