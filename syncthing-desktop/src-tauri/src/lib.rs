use std::{
    env,
    fs::{self, OpenOptions},
    net::TcpListener,
    path::{Path, PathBuf},
    process::{Child, Command, Stdio},
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, Ordering},
    },
    thread,
    time::{Duration, Instant},
};

#[cfg(unix)]
use std::os::unix::process::CommandExt;
#[cfg(windows)]
use std::os::windows::process::CommandExt;

use quick_xml::de::from_str;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use tauri::{
    AppHandle, Manager, RunEvent, State, WindowEvent,
    menu::{Menu, MenuItem},
    tray::{MouseButton, MouseButtonState, TrayIconBuilder, TrayIconEvent},
};
use url::Url;

const DEFAULT_GUI_URL: &str = "http://127.0.0.1:8384/";
const STARTUP_TIMEOUT: Duration = Duration::from_secs(30);
const MAX_PORT_ATTEMPTS: usize = 3;
const API_TIMEOUT: Duration = Duration::from_secs(3);

#[derive(Clone, Default)]
struct DesktopState {
    session: Arc<Mutex<DesktopSession>>,
    exiting: Arc<AtomicBool>,
}

#[derive(Default)]
struct DesktopSession {
    connection: Option<Connection>,
    daemon: Option<ManagedDaemon>,
    connecting: bool,
}

#[derive(Clone)]
struct Connection {
    gui_url: String,
    api_key: Option<String>,
    device_id: String,
    version: String,
    managed: bool,
}

struct ManagedDaemon {
    child: Child,
    connection: Connection,
    gui_ready: bool,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct ConnectionView {
    gui_url: String,
    device_id: String,
    version: String,
    managed: bool,
    details_available: bool,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct DashboardSnapshot {
    connected: bool,
    managed: bool,
    details_available: bool,
    gui_url: String,
    device_id: String,
    version: String,
    connected_devices: usize,
    total_devices: usize,
    folders: Vec<FolderSnapshot>,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct FolderSnapshot {
    id: String,
    label: String,
    state: String,
    paused: bool,
    local_files: u64,
    global_files: u64,
    need_files: u64,
    need_bytes: u64,
    errors: u64,
}

#[derive(Default, Deserialize)]
struct SyncthingConfig {
    #[serde(default)]
    gui: GuiConfig,
}

#[derive(Default, Deserialize)]
struct GuiConfig {
    #[serde(default)]
    address: String,
    #[serde(rename = "@tls", default)]
    tls: bool,
    #[serde(default)]
    apikey: String,
}

#[tauri::command]
async fn connect_to_syncthing(
    app: AppHandle,
    state: State<'_, DesktopState>,
) -> Result<ConnectionView, String> {
    let session = state.session.clone();
    let log_dir = app
        .path()
        .app_log_dir()
        .map_err(|err| format!("Could not find the app log directory: {err}"))?;

    tauri::async_runtime::spawn_blocking(move || connect_to_syncthing_inner(session, log_dir))
        .await
        .map_err(|err| format!("Could not connect to Syncthing: {err}"))?
}

fn connect_to_syncthing_inner(
    session: Arc<Mutex<DesktopSession>>,
    log_dir: PathBuf,
) -> Result<ConnectionView, String> {
    let existing = {
        let mut state = session
            .lock()
            .map_err(|_| "Syncthing session state is unavailable".to_string())?;
        if state.connecting {
            return Err("Syncthing is already starting".into());
        }

        if let Some(daemon) = state.daemon.as_mut() {
            if daemon
                .child
                .try_wait()
                .map_err(|err| format!("Could not inspect Syncthing process: {err}"))?
                .is_some()
            {
                state.daemon = None;
                state.connection = None;
            }
        }

        state.connecting = true;
        state.connection.clone()
    };

    let result = if let Some(connection) = existing {
        let managed = {
            session
                .lock()
                .map_err(|_| "Syncthing session state is unavailable".to_string())?
                .daemon
                .is_some()
        };
        if managed {
            wait_for_managed_syncthing(session.clone(), STARTUP_TIMEOUT)
                .map(|connection| connection_view(&connection))
        } else if probe_connection(&connection).is_some() {
            Ok(connection_view(&connection))
        } else {
            connect_or_start(session.clone(), log_dir)
        }
    } else {
        connect_or_start(session.clone(), log_dir)
    };

    if let Ok(mut state) = session.lock() {
        state.connecting = false;
    }
    result
}

fn connect_or_start(
    session: Arc<Mutex<DesktopSession>>,
    log_dir: PathBuf,
) -> Result<ConnectionView, String> {
    for candidate in gui_candidates() {
        let mut connection = Connection {
            gui_url: candidate.url,
            api_key: candidate.api_key,
            device_id: String::new(),
            version: String::new(),
            managed: false,
        };
        if let Some((device_id, version)) = probe_connection(&connection) {
            connection.device_id = device_id;
            connection.version = version;
            if let Ok(mut state) = session.lock() {
                state.connection = Some(connection.clone());
            }
            return Ok(connection_view(&connection));
        }
    }

    let mut last_error = "Syncthing exited before its GUI became ready".to_string();
    for _ in 0..MAX_PORT_ATTEMPTS {
        let port = available_local_port()?;
        let connection = start_managed_syncthing(port);
        let child = spawn_managed_process(port, &connection, &log_dir)?;
        {
            let mut state = session
                .lock()
                .map_err(|_| "Syncthing session state is unavailable".to_string())?;
            state.connection = Some(connection.clone());
            state.daemon = Some(ManagedDaemon {
                child,
                connection: connection.clone(),
                gui_ready: false,
            });
        }

        match wait_for_managed_syncthing(session.clone(), STARTUP_TIMEOUT) {
            Ok(connection) => return Ok(connection_view(&connection)),
            Err(err) => {
                last_error = err;
                let exited = session
                    .lock()
                    .map_err(|_| "Syncthing session state is unavailable".to_string())?
                    .daemon
                    .is_none();
                if !exited {
                    return Err(format!("{last_error} (logs: {})", log_dir.display()));
                }
            }
        }
    }

    Err(format!(
        "{last_error}; tried {MAX_PORT_ATTEMPTS} local GUI ports (logs: {})",
        log_dir.display()
    ))
}

fn start_managed_syncthing(port: u16) -> Connection {
    let address = format!("127.0.0.1:{port}");
    let gui_url = format!("http://{address}/");
    let api_key = uuid::Uuid::new_v4().simple().to_string();
    let (device_id, version) = ("".to_string(), "".to_string());
    Connection {
        gui_url,
        api_key: Some(api_key),
        device_id,
        version,
        managed: true,
    }
}

fn spawn_managed_process(
    port: u16,
    connection: &Connection,
    log_dir: &Path,
) -> Result<Child, String> {
    fs::create_dir_all(log_dir).map_err(|err| {
        format!(
            "Could not create log directory {}: {err}",
            log_dir.display()
        )
    })?;
    let desktop_log = OpenOptions::new()
        .create(true)
        .append(true)
        .open(log_dir.join("desktop.log"))
        .map_err(|err| format!("Could not open desktop log: {err}"))?;
    let child_stderr = desktop_log
        .try_clone()
        .map_err(|err| format!("Could not prepare desktop log: {err}"))?;
    let syncthing_log = log_dir.join("syncthing.log");
    let mut command = Command::new(syncthing_binary());
    command
        .arg("--no-browser")
        .env("STGUIADDRESS", format!("127.0.0.1:{port}"))
        .env(
            "STGUIAPIKEY",
            connection.api_key.as_deref().unwrap_or_default(),
        )
        .env("STLOGFILE", syncthing_log)
        .stdin(Stdio::null())
        .stdout(Stdio::from(desktop_log))
        .stderr(Stdio::from(child_stderr));

    #[cfg(unix)]
    command.process_group(0);

    #[cfg(windows)]
    command
        .env("STHIDECONSOLE", "true")
        .creation_flags(0x08000000);

    command
        .spawn()
        .map_err(|err| format!("Could not start Syncthing. Install it or set SYNCTHING_BIN: {err}"))
}

fn wait_for_managed_syncthing(
    session: Arc<Mutex<DesktopSession>>,
    timeout: Duration,
) -> Result<Connection, String> {
    let deadline = Instant::now() + timeout;
    loop {
        let connection = {
            let mut state = session
                .lock()
                .map_err(|_| "Syncthing session state is unavailable".to_string())?;
            let Some(daemon) = state.daemon.as_mut() else {
                return Err("Syncthing process is no longer running".into());
            };
            let status = daemon
                .child
                .try_wait()
                .map_err(|err| format!("Could not inspect Syncthing process: {err}"))?;
            if let Some(status) = status {
                state.daemon = None;
                state.connection = None;
                return Err(format!(
                    "Syncthing exited before its GUI became ready ({status})"
                ));
            }
            daemon.connection.clone()
        };

        if let Some((device_id, version)) = probe_api(&connection) {
            let mut connection = connection;
            connection.device_id = device_id;
            connection.version = version;
            if let Ok(mut state) = session.lock() {
                if let Some(daemon) = state.daemon.as_mut() {
                    daemon.gui_ready = true;
                    daemon.connection = connection.clone();
                }
                state.connection = Some(connection.clone());
            }
            return Ok(connection);
        }

        if Instant::now() >= deadline {
            return Err(format!(
                "Timed out waiting for Syncthing's GUI at {}",
                connection.gui_url
            ));
        }
        thread::sleep(Duration::from_millis(250));
    }
}

fn probe_api(connection: &Connection) -> Option<(String, String)> {
    let api_key = connection.api_key.as_deref()?;
    let status = api_get(&connection.gui_url, "rest/system/status", Some(api_key)).ok()?;
    let device_id = status.get("myID")?.as_str()?.to_string();
    let version = api_get(&connection.gui_url, "rest/system/version", Some(api_key))
        .ok()
        .and_then(|value| value.get("version")?.as_str().map(str::to_string))
        .unwrap_or_default();
    Some((device_id, version))
}

fn probe_gui(gui_url: &str) -> Option<(String, String)> {
    let agent = http_agent();
    let response = match agent.get(gui_url).call() {
        Ok(response) => response,
        Err(ureq::Error::Status(_, response)) => response,
        Err(_) => return None,
    };
    let device_id = response.header("X-Syncthing-ID")?.to_string();
    let version = response
        .header("X-Syncthing-Version")
        .unwrap_or_default()
        .to_string();
    Some((device_id, version))
}

fn probe_connection(connection: &Connection) -> Option<(String, String)> {
    probe_api(connection).or_else(|| probe_gui(&connection.gui_url))
}

fn http_agent() -> ureq::Agent {
    ureq::AgentBuilder::new()
        .timeout(API_TIMEOUT)
        .redirects(0)
        .build()
}

fn api_get(base_url: &str, path: &str, api_key: Option<&str>) -> Result<Value, String> {
    let url = format!("{}{}", base_url.trim_end_matches('/'), format!("/{path}"));
    let mut request = http_agent().get(&url);
    if let Some(api_key) = api_key {
        request = request.set("X-API-Key", api_key);
    }
    let response = request
        .call()
        .map_err(|err| format!("Syncthing API request failed ({path}): {err}"))?;
    let body = response
        .into_string()
        .map_err(|err| format!("Could not read Syncthing API response ({path}): {err}"))?;
    serde_json::from_str(&body)
        .map_err(|err| format!("Invalid Syncthing API response ({path}): {err}"))
}

fn connection_view(connection: &Connection) -> ConnectionView {
    ConnectionView {
        gui_url: connection.gui_url.clone(),
        device_id: connection.device_id.clone(),
        version: connection.version.clone(),
        managed: connection.managed,
        details_available: connection.api_key.is_some(),
    }
}

#[tauri::command]
async fn get_dashboard(state: State<'_, DesktopState>) -> Result<DashboardSnapshot, String> {
    let connection = state
        .session
        .lock()
        .map_err(|_| "Syncthing session state is unavailable".to_string())?
        .connection
        .clone()
        .ok_or_else(|| "Syncthing is not connected".to_string())?;

    tauri::async_runtime::spawn_blocking(move || dashboard_snapshot(connection))
        .await
        .map_err(|err| format!("Could not read Syncthing status: {err}"))?
}

fn dashboard_snapshot(connection: Connection) -> Result<DashboardSnapshot, String> {
    let Some(api_key) = connection.api_key.as_deref() else {
        return Ok(DashboardSnapshot {
            connected: true,
            managed: connection.managed,
            details_available: false,
            gui_url: connection.gui_url,
            device_id: connection.device_id,
            version: connection.version,
            connected_devices: 0,
            total_devices: 0,
            folders: Vec::new(),
        });
    };

    let connections = api_get(
        &connection.gui_url,
        "rest/system/connections",
        Some(api_key),
    )?;
    let connections = connections.as_object();
    let total_devices = connections.map_or(0, serde_json::Map::len);
    let connected_devices = connections.map_or(0, |devices| {
        devices
            .values()
            .filter(|device| device.get("connected").and_then(Value::as_bool) == Some(true))
            .count()
    });

    let configured_folders = api_get(&connection.gui_url, "rest/config/folders", Some(api_key))?;
    let mut folders = Vec::new();
    if let Some(configured_folders) = configured_folders.as_array() {
        for folder in configured_folders {
            let Some(id) = folder.get("id").and_then(Value::as_str) else {
                continue;
            };
            let encoded_id: String = url::form_urlencoded::byte_serialize(id.as_bytes()).collect();
            let summary = api_get(
                &connection.gui_url,
                &format!("rest/db/status?folder={encoded_id}"),
                Some(api_key),
            )?;
            folders.push(FolderSnapshot {
                id: id.to_string(),
                label: folder
                    .get("label")
                    .and_then(Value::as_str)
                    .filter(|label| !label.is_empty())
                    .unwrap_or(id)
                    .to_string(),
                state: summary
                    .get("state")
                    .and_then(Value::as_str)
                    .unwrap_or(
                        if folder.get("paused").and_then(Value::as_bool) == Some(true) {
                            "paused"
                        } else {
                            "unknown"
                        },
                    )
                    .to_string(),
                paused: folder.get("paused").and_then(Value::as_bool) == Some(true),
                local_files: json_u64(&summary, "localFiles"),
                global_files: json_u64(&summary, "globalFiles"),
                need_files: json_u64(&summary, "needFiles"),
                need_bytes: json_u64(&summary, "needBytes"),
                errors: json_u64(&summary, "errors"),
            });
        }
    }

    Ok(DashboardSnapshot {
        connected: true,
        managed: connection.managed,
        details_available: true,
        gui_url: connection.gui_url,
        device_id: connection.device_id,
        version: connection.version,
        connected_devices,
        total_devices,
        folders,
    })
}

fn json_u64(value: &Value, key: &str) -> u64 {
    value.get(key).and_then(Value::as_u64).unwrap_or_default()
}

#[tauri::command]
async fn set_folder_paused(
    state: State<'_, DesktopState>,
    folder_id: String,
    paused: bool,
) -> Result<(), String> {
    let connection = state
        .session
        .lock()
        .map_err(|_| "Syncthing session state is unavailable".to_string())?
        .connection
        .clone()
        .ok_or_else(|| "Syncthing is not connected".to_string())?;
    tauri::async_runtime::spawn_blocking(move || patch_folder(&connection, &folder_id, paused))
        .await
        .map_err(|err| format!("Could not update folder: {err}"))?
}

#[tauri::command]
async fn set_all_folders_paused(
    state: State<'_, DesktopState>,
    paused: bool,
) -> Result<(), String> {
    let connection = state
        .session
        .lock()
        .map_err(|_| "Syncthing session state is unavailable".to_string())?
        .connection
        .clone()
        .ok_or_else(|| "Syncthing is not connected".to_string())?;
    tauri::async_runtime::spawn_blocking(move || set_all_folders_paused_inner(&connection, paused))
        .await
        .map_err(|err| format!("Could not update folders: {err}"))?
}

fn set_all_folders_paused_inner(connection: &Connection, paused: bool) -> Result<(), String> {
    let api_key = connection
        .api_key
        .as_deref()
        .ok_or_else(|| "API access is unavailable for this Syncthing instance".to_string())?;
    let configured_folders = api_get(&connection.gui_url, "rest/config/folders", Some(api_key))?;
    let folders = configured_folders
        .as_array()
        .ok_or_else(|| "Syncthing returned an invalid folder list".to_string())?;
    for folder in folders {
        if let Some(folder_id) = folder.get("id").and_then(Value::as_str) {
            patch_folder(connection, folder_id, paused)?;
        }
    }
    Ok(())
}

fn patch_folder(connection: &Connection, folder_id: &str, paused: bool) -> Result<(), String> {
    let api_key = connection
        .api_key
        .as_deref()
        .ok_or_else(|| "API access is unavailable for this Syncthing instance".to_string())?;
    let mut url = Url::parse(&connection.gui_url)
        .map_err(|err| format!("Invalid Syncthing GUI URL: {err}"))?;
    url.set_path("");
    url.path_segments_mut()
        .map_err(|_| "Could not construct Syncthing folder URL".to_string())?
        .extend(["rest", "config", "folders", folder_id]);
    let body = json!({ "paused": paused }).to_string();
    http_agent()
        .patch(url.as_str())
        .set("X-API-Key", api_key)
        .set("Content-Type", "application/json")
        .send_string(&body)
        .map(|_| ())
        .map_err(|err| format!("Could not update folder {folder_id}: {err}"))
}

#[tauri::command]
fn open_web_gui(state: State<'_, DesktopState>) -> Result<(), String> {
    let gui_url = state
        .session
        .lock()
        .map_err(|_| "Syncthing session state is unavailable".to_string())?
        .connection
        .as_ref()
        .map(|connection| connection.gui_url.clone())
        .ok_or_else(|| "Syncthing is not connected".to_string())?;
    tauri_plugin_opener::open_url(gui_url, None::<&str>)
        .map_err(|err| format!("Could not open Syncthing's web interface: {err}"))
}

fn gui_candidates() -> Vec<GuiCandidate> {
    let mut candidates = Vec::new();
    if let Some(address) = env::var("SYNCTHING_GUI_URL").ok() {
        if let Some(url) = local_gui_url(&address, false) {
            candidates.push(GuiCandidate {
                url,
                api_key: config_api_key(),
            });
        }
    }
    if let Some(address) = env::var("STGUIADDRESS").ok() {
        if let Some(url) = local_gui_url(&address, false) {
            candidates.push(GuiCandidate {
                url,
                api_key: env::var("STGUIAPIKEY").ok().or_else(config_api_key),
            });
        }
    }
    if let Some(config) = read_syncthing_config() {
        if let Some(url) = local_gui_url(&config.gui.address, config.gui.tls) {
            candidates.push(GuiCandidate {
                url,
                api_key: if config.gui.apikey.is_empty() {
                    env::var("STGUIAPIKEY").ok()
                } else {
                    Some(config.gui.apikey)
                },
            });
        }
    }
    if let Some(url) = local_gui_url(DEFAULT_GUI_URL, false) {
        candidates.push(GuiCandidate {
            url,
            api_key: env::var("STGUIAPIKEY").ok().or_else(config_api_key),
        });
    }

    candidates
}

struct GuiCandidate {
    url: String,
    api_key: Option<String>,
}

fn local_gui_url(address: &str, tls: bool) -> Option<String> {
    let address = address.trim();
    if address.is_empty() || address.starts_with("unix") {
        return None;
    }
    let scheme = if tls { "https" } else { "http" };
    let raw_url = if address.starts_with("http://") || address.starts_with("https://") {
        address.to_string()
    } else if address.starts_with(':') {
        format!("{scheme}://127.0.0.1{address}")
    } else {
        format!("{scheme}://{address}")
    };
    let mut url = Url::parse(&raw_url).ok()?;
    if url.scheme() != "http" && url.scheme() != "https" {
        return None;
    }
    if !url.username().is_empty() || url.password().is_some() {
        return None;
    }
    let host = url.host_str()?.to_string();
    if host == "0.0.0.0" || host == "::" {
        url.set_host(Some("127.0.0.1")).ok()?;
    } else if host != "localhost"
        && host
            .parse::<std::net::IpAddr>()
            .map_or(true, |ip| !ip.is_loopback())
    {
        return None;
    }
    url.set_path("/");
    url.set_query(None);
    url.set_fragment(None);
    Some(url.to_string())
}

fn config_api_key() -> Option<String> {
    env::var("STGUIAPIKEY")
        .ok()
        .or_else(|| read_syncthing_config().map(|config| config.gui.apikey))
        .filter(|key| !key.is_empty())
}

fn read_syncthing_config() -> Option<SyncthingConfig> {
    let path = syncthing_config_path()?;
    let xml = fs::read_to_string(path).ok()?;
    from_str(&xml).ok()
}

fn syncthing_config_path() -> Option<PathBuf> {
    for variable in ["STCONFDIR", "STHOMEDIR"] {
        if let Some(path) = env::var_os(variable) {
            return Some(PathBuf::from(path).join("config.xml"));
        }
    }
    #[cfg(windows)]
    {
        let base = env::var_os("LOCALAPPDATA")
            .or_else(|| env::var_os("APPDATA"))
            .map(PathBuf::from)?;
        return Some(base.join("Syncthing").join("config.xml"));
    }

    #[cfg(target_os = "macos")]
    {
        let home = env::var_os("HOME").map(PathBuf::from)?;
        return Some(
            home.join("Library")
                .join("Application Support")
                .join("Syncthing")
                .join("config.xml"),
        );
    }

    #[cfg(all(unix, not(target_os = "macos")))]
    {
        let home = env::var_os("HOME").map(PathBuf::from)?;
        if let Some(xdg_config) = env::var_os("XDG_CONFIG_HOME").map(PathBuf::from) {
            let candidate = xdg_config.join("syncthing").join("config.xml");
            if candidate.exists() {
                return Some(candidate);
            }
        }
        let legacy = home.join(".config/syncthing/config.xml");
        if legacy.exists() {
            return Some(legacy);
        }
        if let Some(xdg_state) = env::var_os("XDG_STATE_HOME").map(PathBuf::from) {
            if xdg_state.is_absolute() {
                return Some(xdg_state.join("syncthing/config.xml"));
            }
        }
        return Some(home.join(".local/state/syncthing/config.xml"));
    }
}

fn available_local_port() -> Result<u16, String> {
    if let Ok(listener) = TcpListener::bind("127.0.0.1:8384") {
        if let Ok(address) = listener.local_addr() {
            return Ok(address.port());
        }
    }
    let listener = TcpListener::bind("127.0.0.1:0")
        .map_err(|err| format!("Could not find an available local GUI port: {err}"))?;
    listener
        .local_addr()
        .map(|addr| addr.port())
        .map_err(|err| format!("Could not determine local GUI port: {err}"))
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

fn show_window(app: &AppHandle) {
    if let Some(window) = app.get_webview_window("main") {
        if let Err(err) = window.unminimize() {
            eprintln!("Could not restore Syncthing window: {err}");
        }
        if let Err(err) = window.show() {
            eprintln!("Could not show Syncthing window: {err}");
        } else if let Err(err) = window.set_focus() {
            eprintln!("Could not focus Syncthing window: {err}");
        }
    }
}

fn quit_app(app: &AppHandle) {
    let state = app.state::<DesktopState>();
    if state.exiting.swap(true, Ordering::SeqCst) {
        return;
    }
    let app = app.clone();
    thread::spawn(move || match stop_managed_syncthing(&app) {
        Ok(()) => app.exit(0),
        Err(err) => {
            app.state::<DesktopState>()
                .exiting
                .store(false, Ordering::SeqCst);
            if let Some(window) = app.get_webview_window("main") {
                let _ = window.set_title(&format!("Syncthing - shutdown issue: {err}"));
                show_window(&app);
            }
            append_desktop_log(&app, &format!("Shutdown failed: {err}"));
        }
    });
}

fn stop_managed_syncthing(app: &AppHandle) -> Result<(), String> {
    let session = app.state::<DesktopState>().session.clone();
    let mut daemon = {
        let mut state = session
            .lock()
            .map_err(|_| "Syncthing session state is unavailable".to_string())?;
        state.daemon.take()
    };
    let Some(mut daemon) = daemon.take() else {
        return Ok(());
    };

    let result = stop_managed_daemon(app, &mut daemon);
    if let Err(err) = result {
        if let Ok(mut state) = session.lock() {
            state.connection = Some(daemon.connection.clone());
            state.daemon = Some(daemon);
        }
        return Err(err);
    }

    if let Ok(mut state) = session.lock() {
        state.connection = None;
    }
    Ok(())
}

fn stop_managed_daemon(app: &AppHandle, daemon: &mut ManagedDaemon) -> Result<(), String> {
    if daemon
        .child
        .try_wait()
        .map_err(|err| format!("Could not inspect Syncthing process: {err}"))?
        .is_some()
    {
        return Ok(());
    }

    let shutdown_result = if daemon.gui_ready {
        daemon
            .connection
            .api_key
            .as_deref()
            .ok_or_else(|| "Syncthing API key is unavailable".to_string())
            .and_then(|api_key| {
                http_agent()
                    .post(&format!(
                        "{}rest/system/shutdown",
                        daemon.connection.gui_url
                    ))
                    .set("X-API-Key", api_key)
                    .call()
                    .map(|_| ())
                    .map_err(|err| format!("Shutdown request failed: {err}"))
            })
    } else {
        Err("GUI did not become ready; stopping the managed process tree".into())
    };

    if shutdown_result.is_err() {
        append_desktop_log(
            app,
            &format!(
                "Using process-tree shutdown: {}",
                shutdown_result.unwrap_err()
            ),
        );
        terminate_process_tree(&mut daemon.child)?;
    }

    wait_for_child_exit(&mut daemon.child, Duration::from_secs(10))
        .or_else(|_| terminate_process_tree(&mut daemon.child))?;
    Ok(())
}

fn wait_for_child_exit(child: &mut Child, timeout: Duration) -> Result<(), String> {
    let deadline = Instant::now() + timeout;
    loop {
        if child
            .try_wait()
            .map_err(|err| format!("Could not inspect Syncthing process: {err}"))?
            .is_some()
        {
            return Ok(());
        }
        if Instant::now() >= deadline {
            return Err("Syncthing did not stop within the expected time".into());
        }
        thread::sleep(Duration::from_millis(100));
    }
}

#[cfg(unix)]
fn terminate_process_tree(child: &mut Child) -> Result<(), String> {
    let pid = nix::unistd::Pid::from_raw(child.id() as i32);
    let _ = nix::sys::signal::killpg(pid, nix::sys::signal::Signal::SIGTERM);
    let deadline = Instant::now() + Duration::from_secs(4);
    loop {
        child
            .try_wait()
            .map_err(|err| format!("Could not inspect Syncthing process: {err}"))?;
        match nix::sys::signal::killpg(pid, None) {
            Ok(()) => {}
            Err(nix::errno::Errno::ESRCH) => {
                child
                    .wait()
                    .map_err(|err| format!("Could not reap Syncthing process: {err}"))?;
                return Ok(());
            }
            Err(err) => return Err(format!("Could not inspect Syncthing process group: {err}")),
        }
        if Instant::now() >= deadline {
            break;
        }
        thread::sleep(Duration::from_millis(100));
    }
    nix::sys::signal::killpg(pid, Some(nix::sys::signal::Signal::SIGKILL))
        .map_err(|err| format!("Could not stop Syncthing process group: {err}"))?;
    child
        .wait()
        .map(|_| ())
        .map_err(|err| format!("Could not reap Syncthing process: {err}"))
}

#[cfg(windows)]
fn terminate_process_tree(child: &mut Child) -> Result<(), String> {
    let output = Command::new("taskkill")
        .args(["/PID", &child.id().to_string(), "/T", "/F"])
        .creation_flags(0x08000000)
        .output()
        .map_err(|err| format!("Could not stop the Syncthing process tree: {err}"))?;
    if !output.status.success() && child.try_wait().ok().flatten().is_none() {
        child
            .kill()
            .map_err(|err| format!("Could not stop Syncthing: {err}"))?;
    }
    child
        .wait()
        .map(|_| ())
        .map_err(|err| format!("Could not reap Syncthing process: {err}"))
}

fn append_desktop_log(app: &AppHandle, message: &str) {
    let result = (|| -> std::io::Result<()> {
        let path = app
            .path()
            .app_log_dir()
            .map_err(|err| std::io::Error::other(err.to_string()))?;
        fs::create_dir_all(&path)?;
        let mut file = OpenOptions::new()
            .create(true)
            .append(true)
            .open(path.join("desktop.log"))?;
        use std::io::Write;
        writeln!(file, "{message}")
    })();
    if let Err(err) = result {
        eprintln!("Could not write desktop log: {err}");
    }
}

pub fn run() {
    let builder = tauri::Builder::default()
        .manage(DesktopState::default())
        .plugin(tauri_plugin_single_instance::init(|app, _, _| {
            show_window(app);
        }))
        .setup(|app| {
            let open = MenuItem::with_id(app, "open", "Open dashboard", true, None::<&str>)?;
            let web = MenuItem::with_id(
                app,
                "web",
                "Open Syncthing web interface",
                true,
                None::<&str>,
            )?;
            let pause = MenuItem::with_id(app, "pause", "Pause all folders", true, None::<&str>)?;
            let resume =
                MenuItem::with_id(app, "resume", "Resume all folders", true, None::<&str>)?;
            let quit = MenuItem::with_id(app, "quit", "Quit and stop syncing", true, None::<&str>)?;
            let menu = Menu::with_items(app, &[&open, &web, &pause, &resume, &quit])?;
            let icon = app
                .default_window_icon()
                .cloned()
                .ok_or_else(|| std::io::Error::other("Syncthing window icon is missing"))?;

            TrayIconBuilder::new()
                .icon(icon)
                .tooltip("Syncthing - click to open dashboard")
                .menu(&menu)
                .on_menu_event(|app, event| match event.id().as_ref() {
                    "open" => show_window(app),
                    "web" => {
                        if let Err(err) = open_web_gui(app.state::<DesktopState>()) {
                            append_desktop_log(app, &err);
                        }
                    }
                    "pause" => tray_set_all_folders(app, true),
                    "resume" => tray_set_all_folders(app, false),
                    "quit" => quit_app(app),
                    _ => {}
                })
                .on_tray_icon_event(|tray, event| {
                    if let TrayIconEvent::Click {
                        button: MouseButton::Left,
                        button_state: MouseButtonState::Up,
                        ..
                    } = event
                    {
                        show_window(tray.app_handle());
                    }
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
        .invoke_handler(tauri::generate_handler![
            connect_to_syncthing,
            get_dashboard,
            open_web_gui,
            set_all_folders_paused,
            set_folder_paused
        ]);

    let app = builder
        .build(tauri::generate_context!())
        .expect("error while building Syncthing Desktop");
    app.run(|app, event| {
        if let RunEvent::ExitRequested { api, .. } = event {
            if !app.state::<DesktopState>().exiting.load(Ordering::SeqCst) {
                api.prevent_exit();
                quit_app(app);
            }
        }
    });
}

fn tray_set_all_folders(app: &AppHandle, paused: bool) {
    let app = app.clone();
    thread::spawn(move || {
        let connection = match app.state::<DesktopState>().session.lock() {
            Ok(state) => state
                .connection
                .clone()
                .ok_or_else(|| "Syncthing is not connected".to_string()),
            Err(_) => Err("Syncthing session state is unavailable".to_string()),
        };
        let result =
            connection.and_then(|connection| set_all_folders_paused_inner(&connection, paused));
        if let Err(err) = result {
            append_desktop_log(&app, &format!("Tray action failed: {err}"));
            if let Some(window) = app.get_webview_window("main") {
                let _ = window.set_title(&format!("Syncthing - {err}"));
                show_window(&app);
            }
        }
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn local_gui_url_normalizes_wildcard_and_rejects_remote_hosts() {
        assert_eq!(
            local_gui_url("0.0.0.0:8384", false).as_deref(),
            Some("http://127.0.0.1:8384/")
        );
        assert!(local_gui_url("192.0.2.10:8384", false).is_none());
        assert!(local_gui_url("http://user:password@127.0.0.1:8384", false).is_none());
        assert!(local_gui_url("/tmp/syncthing.sock", false).is_none());
    }

    #[test]
    fn parses_syncthing_gui_config() {
        let config: SyncthingConfig = from_str(
            r#"<configuration><gui tls="true"><address>127.0.0.1:9443</address><apikey>secret</apikey></gui></configuration>"#,
        )
        .unwrap();
        assert!(config.gui.tls);
        assert_eq!(config.gui.address, "127.0.0.1:9443");
        assert_eq!(config.gui.apikey, "secret");
    }
}
