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
use tauri_plugin_dialog::DialogExt;
use url::Url;

mod status;

const DEFAULT_GUI_URL: &str = "http://127.0.0.1:8384/";
const STARTUP_TIMEOUT: Duration = Duration::from_secs(30);
const MAX_PORT_ATTEMPTS: usize = 3;
const API_TIMEOUT: Duration = Duration::from_secs(3);
const STATUS_PANEL_WIDTH: f64 = 340.0;
const STATUS_PANEL_HEIGHT: f64 = 410.0;

#[derive(Clone, Default)]
struct DesktopState {
    session: Arc<Mutex<DesktopSession>>,
    exiting: Arc<AtomicBool>,
    status_client: Arc<Mutex<Option<Arc<status::StatusClient>>>>,
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
    devices: Vec<DeviceSnapshot>,
    folders: Vec<FolderSnapshot>,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct DeviceSnapshot {
    id: String,
    name: String,
    addresses: Vec<String>,
    paused: bool,
    connected: bool,
    connection_address: String,
    connection_type: String,
    client_version: String,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct FolderSnapshot {
    id: String,
    label: String,
    path: String,
    folder_type: String,
    shared_devices: Vec<String>,
    state: String,
    paused: bool,
    local_files: u64,
    global_files: u64,
    need_files: u64,
    need_bytes: u64,
    errors: u64,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct NetworkSettings {
    global_discovery_enabled: bool,
    local_discovery_enabled: bool,
    relays_enabled: bool,
    nat_enabled: bool,
    max_send_kbps: i64,
    max_recv_kbps: i64,
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
    let resource_dir = app
        .path()
        .resource_dir()
        .map_err(|err| format!("Could not find the app resource directory: {err}"))?;

    let result = tauri::async_runtime::spawn_blocking(move || {
        connect_to_syncthing_inner(session, log_dir, resource_dir)
    })
    .await
    .map_err(|err| format!("Could not connect to Syncthing: {err}"))??;
    *state
        .status_client
        .lock()
        .map_err(|_| "Syncthing 状态数据不可用".to_string())? = None;
    Ok(result)
}

#[tauri::command]
async fn get_sync_status(state: State<'_, DesktopState>) -> Result<status::SyncStatus, String> {
    let client = status_client_for_state(state.inner())?;
    tauri::async_runtime::spawn_blocking(move || client.fetch())
        .await
        .map_err(|error| format!("读取 Syncthing 同步状态失败：{error}"))?
}

#[tauri::command]
async fn set_sync_paused(state: State<'_, DesktopState>, paused: bool) -> Result<(), String> {
    let client = status_client_for_state(state.inner())?;
    tauri::async_runtime::spawn_blocking(move || client.set_paused(paused))
        .await
        .map_err(|error| format!("更新 Syncthing 暂停状态失败：{error}"))?
}

fn status_client_for_state(state: &DesktopState) -> Result<Arc<status::StatusClient>, String> {
    let connection = state
        .session
        .lock()
        .map_err(|_| "Syncthing 会话状态不可用".to_string())?
        .connection
        .clone()
        .ok_or_else(|| "Syncthing 尚未连接，无法读取同步状态".to_string())?;
    let api_key = connection
        .api_key
        .or_else(config_api_key)
        .ok_or_else(|| "未找到 Syncthing GUI API 密钥，请检查配置后重试".to_string())?;

    let mut cached_client = state
        .status_client
        .lock()
        .map_err(|_| "Syncthing 状态数据不可用".to_string())?;
    if let Some(client) = cached_client.as_ref() {
        if client.matches_target(&connection.gui_url, &api_key) {
            return Ok(client.clone());
        }
    }

    let client = Arc::new(status::StatusClient::new(&connection.gui_url, api_key));
    *cached_client = Some(client.clone());
    Ok(client)
}

#[tauri::command]
fn open_main_window(app: AppHandle) {
    show_window(&app);
}

#[tauri::command]
fn hide_status_panel(app: AppHandle) {
    if let Some(window) = app.get_webview_window("status-panel") {
        if let Err(error) = window.hide() {
            eprintln!("Could not hide status panel: {error}");
        }
    }
}

fn connect_to_syncthing_inner(
    session: Arc<Mutex<DesktopSession>>,
    log_dir: PathBuf,
    resource_dir: PathBuf,
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
            connect_or_start(session.clone(), log_dir, resource_dir)
        }
    } else {
        connect_or_start(session.clone(), log_dir, resource_dir)
    };

    if let Ok(mut state) = session.lock() {
        state.connecting = false;
    }
    result
}

fn connect_or_start(
    session: Arc<Mutex<DesktopSession>>,
    log_dir: PathBuf,
    resource_dir: PathBuf,
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
        let child = spawn_managed_process(port, &connection, &log_dir, &resource_dir)?;
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
    resource_dir: &Path,
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
    let mut command = Command::new(syncthing_binary(resource_dir));
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

    command.spawn().map_err(|err| {
        format!(
            "Could not start Syncthing. Set SYNCTHING_BIN to override the bundled executable: {err}"
        )
    })
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
            devices: Vec::new(),
            folders: Vec::new(),
        });
    };

    let connection_stats = api_get(
        &connection.gui_url,
        "rest/system/connections",
        Some(api_key),
    )?;
    let connection_stats = connection_stats
        .get("connections")
        .unwrap_or(&connection_stats);
    let configured_devices = api_get(&connection.gui_url, "rest/config/devices", Some(api_key))?;
    let mut devices = Vec::new();
    if let Some(configured_devices) = configured_devices.as_array() {
        for device in configured_devices {
            let Some(id) = device.get("deviceID").and_then(Value::as_str) else {
                continue;
            };
            let stats = connection_stats.get(id).unwrap_or(&Value::Null);
            devices.push(DeviceSnapshot {
                id: id.to_string(),
                name: device
                    .get("name")
                    .and_then(Value::as_str)
                    .filter(|name| !name.is_empty())
                    .unwrap_or(id)
                    .to_string(),
                addresses: device
                    .get("addresses")
                    .and_then(Value::as_array)
                    .map(|addresses| {
                        addresses
                            .iter()
                            .filter_map(Value::as_str)
                            .map(str::to_string)
                            .collect()
                    })
                    .unwrap_or_default(),
                paused: device.get("paused").and_then(Value::as_bool) == Some(true),
                connected: stats.get("connected").and_then(Value::as_bool) == Some(true),
                connection_address: stats
                    .get("address")
                    .and_then(Value::as_str)
                    .unwrap_or_default()
                    .to_string(),
                connection_type: stats
                    .get("type")
                    .and_then(Value::as_str)
                    .unwrap_or_default()
                    .to_string(),
                client_version: stats
                    .get("clientVersion")
                    .and_then(Value::as_str)
                    .unwrap_or_default()
                    .to_string(),
            });
        }
    }
    let total_devices = devices.len();
    let connected_devices = devices.iter().filter(|device| device.connected).count();

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
                path: folder
                    .get("path")
                    .and_then(Value::as_str)
                    .unwrap_or_default()
                    .to_string(),
                folder_type: folder
                    .get("type")
                    .and_then(Value::as_str)
                    .unwrap_or_default()
                    .to_string(),
                shared_devices: folder
                    .get("devices")
                    .and_then(Value::as_array)
                    .map(|devices| {
                        devices
                            .iter()
                            .filter_map(|device| device.get("deviceID").and_then(Value::as_str))
                            .map(str::to_string)
                            .collect()
                    })
                    .unwrap_or_default(),
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
        devices,
        folders,
    })
}

fn json_u64(value: &Value, key: &str) -> u64 {
    value.get(key).and_then(Value::as_u64).unwrap_or_default()
}

fn api_url(connection: &Connection, segments: &[&str]) -> Result<Url, String> {
    let mut url = Url::parse(&connection.gui_url)
        .map_err(|err| format!("Invalid Syncthing GUI URL: {err}"))?;
    url.set_path("/");
    url.set_query(None);
    url.path_segments_mut()
        .map_err(|_| "Could not construct Syncthing API URL".to_string())?
        .pop_if_empty()
        .extend(segments.iter().copied());
    Ok(url)
}

fn api_request(
    connection: &Connection,
    method: &str,
    url: &Url,
    body: Option<&Value>,
) -> Result<(), String> {
    let api_key = connection
        .api_key
        .as_deref()
        .ok_or_else(|| "API access is unavailable for this Syncthing instance".to_string())?;
    let mut request = http_agent()
        .request(method, url.as_str())
        .set("X-API-Key", api_key);
    if let Some(body) = body {
        request = request.set("Content-Type", "application/json");
        request
            .send_string(&body.to_string())
            .map(|_| ())
            .map_err(|err| api_request_error(method, url, err))
    } else {
        request
            .call()
            .map(|_| ())
            .map_err(|err| api_request_error(method, url, err))
    }
}

fn api_request_error(method: &str, url: &Url, err: ureq::Error) -> String {
    let detail = match err {
        ureq::Error::Status(code, response) => {
            let body = response.into_string().unwrap_or_default();
            if body.trim().is_empty() {
                format!("HTTP {code}")
            } else {
                format!("HTTP {code}: {}", body.trim())
            }
        }
        other => other.to_string(),
    };
    format!(
        "Syncthing API request failed ({method} {}): {detail}",
        url.path()
    )
}

fn api_patch(connection: &Connection, segments: &[&str], body: &Value) -> Result<(), String> {
    let url = api_url(connection, segments)?;
    api_request(connection, "PATCH", &url, Some(body))
}

fn updated_folder_devices(existing_devices: &[Value], device_ids: &[String]) -> Vec<Value> {
    device_ids
        .iter()
        .map(|device_id| {
            existing_devices
                .iter()
                .find(|device| {
                    device.get("deviceID").and_then(Value::as_str) == Some(device_id.as_str())
                })
                .cloned()
                .unwrap_or_else(|| json!({ "deviceID": device_id }))
        })
        .collect()
}

fn network_settings(connection: &Connection) -> Result<NetworkSettings, String> {
    let api_key = connection
        .api_key
        .as_deref()
        .ok_or_else(|| "API access is unavailable for this Syncthing instance".to_string())?;
    let options = api_get(&connection.gui_url, "rest/config/options", Some(api_key))?;
    Ok(NetworkSettings {
        global_discovery_enabled: required_bool_option(&options, "globalAnnounceEnabled")?,
        local_discovery_enabled: required_bool_option(&options, "localAnnounceEnabled")?,
        relays_enabled: required_bool_option(&options, "relaysEnabled")?,
        nat_enabled: required_bool_option(&options, "natEnabled")?,
        max_send_kbps: required_i64_option(&options, "maxSendKbps")?,
        max_recv_kbps: required_i64_option(&options, "maxRecvKbps")?,
    })
}

fn required_bool_option(options: &Value, key: &str) -> Result<bool, String> {
    options
        .get(key)
        .and_then(Value::as_bool)
        .ok_or_else(|| format!("Syncthing returned an invalid setting: {key}"))
}

fn required_i64_option(options: &Value, key: &str) -> Result<i64, String> {
    options
        .get(key)
        .and_then(Value::as_i64)
        .ok_or_else(|| format!("Syncthing returned an invalid setting: {key}"))
}

#[tauri::command]
async fn get_network_settings(state: State<'_, DesktopState>) -> Result<NetworkSettings, String> {
    let connection = state
        .session
        .lock()
        .map_err(|_| "Syncthing session state is unavailable".to_string())?
        .connection
        .clone()
        .ok_or_else(|| "Syncthing is not connected".to_string())?;
    tauri::async_runtime::spawn_blocking(move || network_settings(&connection))
        .await
        .map_err(|err| format!("Could not read Syncthing settings: {err}"))?
}

#[tauri::command]
async fn set_network_settings(
    state: State<'_, DesktopState>,
    global_discovery_enabled: bool,
    local_discovery_enabled: bool,
    relays_enabled: bool,
    nat_enabled: bool,
    max_send_kbps: i64,
    max_recv_kbps: i64,
) -> Result<(), String> {
    if max_send_kbps < 0 || max_recv_kbps < 0 {
        return Err("Bandwidth limits must be zero or a positive number".into());
    }
    let connection = state
        .session
        .lock()
        .map_err(|_| "Syncthing session state is unavailable".to_string())?
        .connection
        .clone()
        .ok_or_else(|| "Syncthing is not connected".to_string())?;
    tauri::async_runtime::spawn_blocking(move || {
        let url = api_url(&connection, &["rest", "config", "options"])?;
        let body = json!({
            "globalAnnounceEnabled": global_discovery_enabled,
            "localAnnounceEnabled": local_discovery_enabled,
            "relaysEnabled": relays_enabled,
            "natEnabled": nat_enabled,
            "maxSendKbps": max_send_kbps,
            "maxRecvKbps": max_recv_kbps,
        });
        api_request(&connection, "PATCH", &url, Some(&body))
    })
    .await
    .map_err(|err| format!("Could not update Syncthing settings: {err}"))?
}

#[tauri::command]
async fn add_device(
    state: State<'_, DesktopState>,
    device_id: String,
    name: String,
    addresses: Vec<String>,
) -> Result<(), String> {
    if device_id.trim().is_empty() {
        return Err("Device ID cannot be empty".into());
    }
    let connection = state
        .session
        .lock()
        .map_err(|_| "Syncthing session state is unavailable".to_string())?
        .connection
        .clone()
        .ok_or_else(|| "Syncthing is not connected".to_string())?;
    tauri::async_runtime::spawn_blocking(move || {
        let addresses: Vec<String> = addresses
            .into_iter()
            .map(|address| address.trim().to_string())
            .filter(|address| !address.is_empty())
            .collect();
        let body = json!({
            "deviceID": device_id.trim(),
            "name": name.trim(),
            "addresses": if addresses.is_empty() { vec!["dynamic".to_string()] } else { addresses },
        });
        let url = api_url(&connection, &["rest", "config", "devices"])?;
        api_request(&connection, "POST", &url, Some(&body))
    })
    .await
    .map_err(|err| format!("Could not add device: {err}"))?
}

#[tauri::command]
async fn set_device_paused(
    state: State<'_, DesktopState>,
    device_id: String,
    paused: bool,
) -> Result<(), String> {
    let connection = state
        .session
        .lock()
        .map_err(|_| "Syncthing session state is unavailable".to_string())?
        .connection
        .clone()
        .ok_or_else(|| "Syncthing is not connected".to_string())?;
    tauri::async_runtime::spawn_blocking(move || {
        api_patch(
            &connection,
            &["rest", "config", "devices", &device_id],
            &json!({ "paused": paused }),
        )
    })
    .await
    .map_err(|err| format!("Could not update device: {err}"))?
}

#[tauri::command]
async fn remove_device(state: State<'_, DesktopState>, device_id: String) -> Result<(), String> {
    let connection = state
        .session
        .lock()
        .map_err(|_| "Syncthing session state is unavailable".to_string())?
        .connection
        .clone()
        .ok_or_else(|| "Syncthing is not connected".to_string())?;
    tauri::async_runtime::spawn_blocking(move || {
        let url = api_url(&connection, &["rest", "config", "devices", &device_id])?;
        api_request(&connection, "DELETE", &url, None)
    })
    .await
    .map_err(|err| format!("Could not remove device: {err}"))?
}

#[tauri::command]
async fn add_folder(
    state: State<'_, DesktopState>,
    folder_id: String,
    label: String,
    path: String,
    device_ids: Vec<String>,
) -> Result<(), String> {
    if folder_id.trim().is_empty() || path.trim().is_empty() {
        return Err("Folder ID and local path cannot be empty".into());
    }
    let connection = state
        .session
        .lock()
        .map_err(|_| "Syncthing session state is unavailable".to_string())?
        .connection
        .clone()
        .ok_or_else(|| "Syncthing is not connected".to_string())?;
    tauri::async_runtime::spawn_blocking(move || {
        let body = json!({
            "id": folder_id.trim(),
            "label": label.trim(),
            "path": path.trim(),
            "devices": device_ids
                .into_iter()
                .map(|device_id| json!({ "deviceID": device_id }))
                .collect::<Vec<_>>(),
        });
        let url = api_url(&connection, &["rest", "config", "folders"])?;
        api_request(&connection, "POST", &url, Some(&body))
    })
    .await
    .map_err(|err| format!("Could not add folder: {err}"))?
}

#[tauri::command]
async fn set_folder_devices(
    state: State<'_, DesktopState>,
    folder_id: String,
    device_ids: Vec<String>,
) -> Result<(), String> {
    let connection = state
        .session
        .lock()
        .map_err(|_| "Syncthing session state is unavailable".to_string())?
        .connection
        .clone()
        .ok_or_else(|| "Syncthing is not connected".to_string())?;
    tauri::async_runtime::spawn_blocking(move || {
        let api_key = connection
            .api_key
            .as_deref()
            .ok_or_else(|| "API access is unavailable for this Syncthing instance".to_string())?;
        let encoded_folder_id: String =
            url::form_urlencoded::byte_serialize(folder_id.as_bytes()).collect();
        let existing = api_get(
            &connection.gui_url,
            &format!("rest/config/folders/{encoded_folder_id}"),
            Some(api_key),
        )?;
        let existing_devices = existing
            .get("devices")
            .and_then(Value::as_array)
            .cloned()
            .unwrap_or_default();
        let devices = updated_folder_devices(&existing_devices, &device_ids);
        let url = api_url(&connection, &["rest", "config", "folders", &folder_id])?;
        let body = json!({
            "devices": devices,
        });
        api_request(&connection, "PATCH", &url, Some(&body))
    })
    .await
    .map_err(|err| format!("Could not update folder sharing: {err}"))?
}

#[tauri::command]
async fn remove_folder(state: State<'_, DesktopState>, folder_id: String) -> Result<(), String> {
    let connection = state
        .session
        .lock()
        .map_err(|_| "Syncthing session state is unavailable".to_string())?
        .connection
        .clone()
        .ok_or_else(|| "Syncthing is not connected".to_string())?;
    tauri::async_runtime::spawn_blocking(move || {
        let url = api_url(&connection, &["rest", "config", "folders", &folder_id])?;
        api_request(&connection, "DELETE", &url, None)
    })
    .await
    .map_err(|err| format!("Could not remove folder configuration: {err}"))?
}

#[tauri::command]
async fn scan_folder(state: State<'_, DesktopState>, folder_id: String) -> Result<(), String> {
    let connection = state
        .session
        .lock()
        .map_err(|_| "Syncthing session state is unavailable".to_string())?
        .connection
        .clone()
        .ok_or_else(|| "Syncthing is not connected".to_string())?;
    tauri::async_runtime::spawn_blocking(move || {
        let mut url = api_url(&connection, &["rest", "db", "scan"])?;
        url.query_pairs_mut().append_pair("folder", &folder_id);
        api_request(&connection, "POST", &url, None)
    })
    .await
    .map_err(|err| format!("Could not scan folder: {err}"))?
}

#[tauri::command]
async fn restart_syncthing(state: State<'_, DesktopState>) -> Result<(), String> {
    let connection = state
        .session
        .lock()
        .map_err(|_| "Syncthing session state is unavailable".to_string())?
        .connection
        .clone()
        .ok_or_else(|| "Syncthing is not connected".to_string())?;
    tauri::async_runtime::spawn_blocking(move || {
        let url = api_url(&connection, &["rest", "system", "restart"])?;
        api_request(&connection, "POST", &url, None)
    })
    .await
    .map_err(|err| format!("Could not restart Syncthing: {err}"))?
}

#[tauri::command]
async fn stop_syncthing(app: AppHandle, state: State<'_, DesktopState>) -> Result<(), String> {
    let session = state.session.clone();
    let connection = state
        .session
        .lock()
        .map_err(|_| "Syncthing session state is unavailable".to_string())?
        .connection
        .clone()
        .ok_or_else(|| "Syncthing is not connected".to_string())?;
    if connection.managed {
        return tauri::async_runtime::spawn_blocking(move || stop_managed_syncthing(&app))
            .await
            .map_err(|err| format!("Could not stop Syncthing: {err}"))?;
    }
    tauri::async_runtime::spawn_blocking(move || {
        let url = api_url(&connection, &["rest", "system", "shutdown"])?;
        api_request(&connection, "POST", &url, None)?;
        if let Ok(mut session_state) = session.lock() {
            session_state.connection = None;
        }
        Ok(())
    })
    .await
    .map_err(|err| format!("Could not stop Syncthing: {err}"))?
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
    api_patch(
        connection,
        &["rest", "config", "folders", folder_id],
        &json!({ "paused": paused }),
    )
}

#[tauri::command]
async fn choose_folder(app: AppHandle) -> Result<Option<String>, String> {
    tauri::async_runtime::spawn_blocking(move || {
        app.dialog()
            .file()
            .blocking_pick_folder()
            .map(|path| path.to_string())
    })
    .await
    .map_err(|err| format!("Could not open the folder picker: {err}"))
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

fn syncthing_binary(resource_dir: &Path) -> std::ffi::OsString {
    if let Some(binary) = env::var_os("SYNCTHING_BIN") {
        return binary;
    }

    if let Some(binary) = bundled_syncthing_binary(resource_dir) {
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

fn bundled_syncthing_binary(resource_dir: &Path) -> Option<std::ffi::OsString> {
    let bundled_directory = resource_dir.join("target/bundled-resources");
    #[cfg(windows)]
    let bundled_name = "syncthing.exe";
    #[cfg(not(windows))]
    let bundled_name = "syncthing";
    let bundled_binary = bundled_directory.join(bundled_name);
    bundled_binary
        .is_file()
        .then(|| bundled_binary.into_os_string())
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

#[cfg(not(target_os = "macos"))]
fn toggle_status_panel(_: &AppHandle) {}

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
        .plugin(tauri_plugin_dialog::init())
        .setup(|app| {
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

            let open = MenuItem::with_id(app, "open", "Open dashboard", true, None::<&str>)?;
            let pause = MenuItem::with_id(app, "pause", "Pause all folders", true, None::<&str>)?;
            let resume =
                MenuItem::with_id(app, "resume", "Resume all folders", true, None::<&str>)?;
            let quit = MenuItem::with_id(app, "quit", "Quit desktop app", true, None::<&str>)?;
            #[cfg(target_os = "macos")]
            let toggle_panel =
                MenuItem::with_id(app, "toggle-panel", "显示/隐藏状态面板", true, None::<&str>)?;
            #[cfg(target_os = "macos")]
            let menu = Menu::with_items(app, &[&open, &pause, &resume, &toggle_panel, &quit])?;
            #[cfg(not(target_os = "macos"))]
            let menu = Menu::with_items(app, &[&open, &pause, &resume, &quit])?;
            let icon = app
                .default_window_icon()
                .cloned()
                .ok_or_else(|| std::io::Error::other("Syncthing window icon is missing"))?;

            let tray = TrayIconBuilder::new();
            #[cfg(target_os = "macos")]
            let tray = tray.show_menu_on_left_click(false);
            tray.icon(icon)
                .tooltip("Syncthing - click to open dashboard")
                .menu(&menu)
                .on_menu_event(|app, event| match event.id().as_ref() {
                    "open" => show_window(app),
                    "pause" => tray_set_all_folders(app, true),
                    "resume" => tray_set_all_folders(app, false),
                    "toggle-panel" => toggle_status_panel(app),
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
                        #[cfg(target_os = "macos")]
                        toggle_status_panel(tray.app_handle());
                        #[cfg(not(target_os = "macos"))]
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
            add_device,
            add_folder,
            choose_folder,
            connect_to_syncthing,
            get_dashboard,
            get_network_settings,
            remove_device,
            remove_folder,
            restart_syncthing,
            scan_folder,
            get_sync_status,
            set_sync_paused,
            open_main_window,
            hide_status_panel,
            set_all_folders_paused,
            set_device_paused,
            set_folder_devices,
            set_folder_paused,
            set_network_settings,
            stop_syncthing
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
    use std::{
        io::{BufRead, BufReader, Read, Write},
        net::TcpListener,
        sync::mpsc::{self, Receiver},
    };

    struct MockRequest {
        line: String,
        headers: String,
        body: String,
    }

    fn mock_server(responses: Vec<String>) -> (String, Receiver<MockRequest>) {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let address = listener.local_addr().unwrap();
        let (sender, receiver) = mpsc::channel();

        thread::spawn(move || {
            for response_body in responses {
                let (mut stream, _) = listener.accept().unwrap();
                stream
                    .set_read_timeout(Some(Duration::from_secs(3)))
                    .unwrap();
                let mut reader = BufReader::new(stream.try_clone().unwrap());
                let mut line = String::new();
                reader.read_line(&mut line).unwrap();
                let request_line = line.trim_end().to_string();
                let mut headers = String::new();
                let mut content_length = 0;
                loop {
                    line.clear();
                    reader.read_line(&mut line).unwrap();
                    if line.trim().is_empty() {
                        break;
                    }
                    if let Some((name, value)) = line.split_once(':') {
                        if name.eq_ignore_ascii_case("content-length") {
                            content_length = value.trim().parse().unwrap_or_default();
                        }
                    }
                    headers.push_str(&line);
                }
                let mut body = vec![0; content_length];
                reader.read_exact(&mut body).unwrap();
                sender
                    .send(MockRequest {
                        line: request_line,
                        headers,
                        body: String::from_utf8(body).unwrap(),
                    })
                    .unwrap();

                write!(
                    stream,
                    "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
                    response_body.len(),
                    response_body
                )
                .unwrap();
            }
        });

        (format!("http://{address}/"), receiver)
    }

    fn mock_connection(gui_url: String) -> Connection {
        Connection {
            gui_url,
            api_key: Some("test-api-key".into()),
            device_id: "local-device".into(),
            version: "v1.0.0".into(),
            managed: true,
        }
    }

    fn requests(receiver: Receiver<MockRequest>, count: usize) -> Vec<MockRequest> {
        (0..count)
            .map(|_| receiver.recv_timeout(Duration::from_secs(3)).unwrap())
            .collect()
    }

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
    fn resolves_bundled_syncthing_from_tauri_resources() {
        let resource_dir =
            env::temp_dir().join(format!("syncthing-desktop-test-{}", uuid::Uuid::new_v4()));
        let bundled_directory = resource_dir.join("target/bundled-resources");
        #[cfg(windows)]
        let executable_name = "syncthing.exe";
        #[cfg(not(windows))]
        let executable_name = "syncthing";
        let executable_path = bundled_directory.join(executable_name);
        fs::create_dir_all(&bundled_directory).unwrap();
        fs::write(&executable_path, b"test executable").unwrap();

        assert_eq!(
            bundled_syncthing_binary(&resource_dir).map(PathBuf::from),
            Some(executable_path)
        );

        fs::remove_dir_all(resource_dir).unwrap();
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

    #[test]
    fn dashboard_refresh_reads_device_and_folder_status() {
        let (gui_url, receiver) = mock_server(vec![
            r#"{"connections":{"device-a":{"connected":true},"device-b":{"connected":false}}}"#.into(),
            r#"[{"deviceID":"device-a","name":"Laptop","addresses":["dynamic"],"paused":false},{"deviceID":"device-b","name":"NAS","addresses":["tcp://nas:22000"],"paused":false}]"#.into(),
            r#"[{"id":"docs","label":"Documents","path":"/tmp/docs","type":"sendreceive","devices":[{"deviceID":"device-a"}],"paused":false}]"#.into(),
            r#"{"state":"syncing","localFiles":9,"globalFiles":12,"needFiles":3,"needBytes":2048,"errors":1}"#.into(),
        ]);

        let snapshot = dashboard_snapshot(mock_connection(gui_url)).unwrap();

        assert_eq!(snapshot.connected_devices, 1);
        assert_eq!(snapshot.total_devices, 2);
        assert_eq!(snapshot.devices[0].name, "Laptop");
        assert!(snapshot.devices[0].connected);
        assert_eq!(snapshot.folders.len(), 1);
        assert_eq!(snapshot.folders[0].path, "/tmp/docs");
        assert_eq!(snapshot.folders[0].shared_devices, vec!["device-a"]);
        assert_eq!(snapshot.folders[0].state, "syncing");
        assert_eq!(snapshot.folders[0].need_files, 3);
        assert_eq!(snapshot.folders[0].need_bytes, 2048);
        assert_eq!(snapshot.folders[0].errors, 1);
        let requests = requests(receiver, 4);
        assert!(
            requests[0]
                .line
                .starts_with("GET /rest/system/connections ")
        );
        assert!(requests[1].line.starts_with("GET /rest/config/devices "));
        assert!(requests[2].line.starts_with("GET /rest/config/folders "));
        assert!(
            requests[3]
                .line
                .starts_with("GET /rest/db/status?folder=docs ")
        );
        assert!(
            requests[0]
                .headers
                .to_ascii_lowercase()
                .contains("x-api-key: test-api-key")
        );
    }

    #[test]
    fn pause_all_updates_every_folder() {
        let (gui_url, receiver) = mock_server(vec![
            r#"[{"id":"photos"},{"id":"docs"}]"#.into(),
            "{}".into(),
            "{}".into(),
        ]);

        set_all_folders_paused_inner(&mock_connection(gui_url), true).unwrap();

        let requests = requests(receiver, 3);
        assert!(requests[0].line.starts_with("GET /rest/config/folders "));
        assert!(
            requests[1]
                .line
                .starts_with("PATCH /rest/config/folders/photos ")
        );
        assert!(
            requests[2]
                .line
                .starts_with("PATCH /rest/config/folders/docs ")
        );
        assert_eq!(requests[1].body, r#"{"paused":true}"#);
        assert_eq!(requests[2].body, r#"{"paused":true}"#);
    }

    #[test]
    fn individual_folder_can_be_resumed() {
        let (gui_url, receiver) = mock_server(vec!["{}".into()]);

        patch_folder(&mock_connection(gui_url), "docs", false).unwrap();

        let request = requests(receiver, 1).pop().unwrap();
        assert!(request.line.starts_with("PATCH /rest/config/folders/docs "));
        assert_eq!(request.body, r#"{"paused":false}"#);
        assert!(
            request
                .headers
                .to_ascii_lowercase()
                .contains("x-api-key: test-api-key")
        );
    }

    #[test]
    fn folder_api_paths_encode_each_segment() {
        let url = api_url(
            &mock_connection("http://127.0.0.1:8384/".into()),
            &["rest", "config", "folders", "personal docs"],
        )
        .unwrap();
        assert_eq!(url.path(), "/rest/config/folders/personal%20docs");
    }

    #[test]
    fn changing_shared_devices_preserves_existing_device_configuration() {
        let existing = vec![json!({
            "deviceID": "device-a",
            "encryptionPassword": "keep-this",
            "introducedBy": "introducer",
        })];

        let updated =
            updated_folder_devices(&existing, &["device-a".to_string(), "device-b".to_string()]);

        assert_eq!(updated[0]["encryptionPassword"], "keep-this");
        assert_eq!(updated[0]["introducedBy"], "introducer");
        assert_eq!(updated[1], json!({ "deviceID": "device-b" }));
    }
}
