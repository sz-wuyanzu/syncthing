use std::{
    env, fs,
    path::PathBuf,
    time::{Duration, Instant},
};

use serde::{Deserialize, Serialize};
use serde_json::Value;

const GUI_URL: &str = "http://127.0.0.1:8384";

#[derive(Clone)]
pub(crate) struct StatusClient {
    base_url: String,
    api_key: String,
    agent: ureq::Agent,
    rate_sample: std::sync::Arc<std::sync::Mutex<Option<RateSample>>>,
    active_file_sample: std::sync::Arc<std::sync::Mutex<Option<ActiveFileSample>>>,
}

#[derive(Clone, Copy)]
struct RateSample {
    received: u64,
    sent: u64,
    at: Instant,
}

struct ActiveFileSample {
    files: Vec<ActiveFile>,
    at: Instant,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct SyncStatus {
    state: String,
    paused: bool,
    download_rate: u64,
    upload_rate: u64,
    active_files: Vec<ActiveFile>,
}

#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct ActiveFile {
    folder: String,
    name: String,
    size: u64,
}

#[derive(Deserialize)]
struct SyncthingConfig {
    gui: GuiConfig,
}

#[derive(Deserialize)]
struct GuiConfig {
    #[serde(rename = "apikey")]
    api_key: Option<String>,
}

impl StatusClient {
    pub(crate) fn new(base_url: impl Into<String>, api_key: impl Into<String>) -> Self {
        Self {
            base_url: base_url.into().trim_end_matches('/').to_string(),
            api_key: api_key.into(),
            agent: ureq::AgentBuilder::new()
                .timeout(Duration::from_secs(3))
                .build(),
            rate_sample: Default::default(),
            active_file_sample: Default::default(),
        }
    }

    pub(crate) fn discover_external() -> Result<Self, String> {
        let api_key = read_external_api_key()?;
        Ok(Self::new(GUI_URL, api_key))
    }

    pub(crate) fn fetch(&self) -> Result<SyncStatus, String> {
        let connections = self.get_json("/rest/system/connections", &[])?;
        let totals = connections
            .get("total")
            .ok_or("Syncthing returned no connection totals")?;
        let received = required_u64(totals, "inBytesTotal")?;
        let sent = required_u64(totals, "outBytesTotal")?;
        let (download_rate, upload_rate) = self.rates(received, sent)?;

        let devices = self.get_json("/rest/config/devices", &[])?;
        let devices = devices
            .as_array()
            .ok_or("Syncthing returned an invalid device list")?;
        let paused = !devices.is_empty()
            && devices
                .iter()
                .all(|device| device.get("paused").and_then(Value::as_bool) == Some(true));

        let folders = self.get_json("/rest/config/folders", &[])?;
        let folders = folders
            .as_array()
            .ok_or("Syncthing returned an invalid folder list")?;
        let mut has_scanning = false;
        let mut has_syncing = false;
        let mut folders_to_check = Vec::with_capacity(folders.len());

        for folder in folders {
            let id = folder
                .get("id")
                .and_then(Value::as_str)
                .filter(|id| !id.is_empty())
                .ok_or("Syncthing returned a folder without an ID")?;
            let label = folder
                .get("label")
                .and_then(Value::as_str)
                .filter(|label| !label.is_empty())
                .unwrap_or(id)
                .to_string();
            folders_to_check.push((id.to_string(), label));

            let db_status = self.get_json("/rest/db/status", &[("folder", id)])?;
            match db_status.get("state").and_then(Value::as_str) {
                Some("scanning") => has_scanning = true,
                Some("syncing") => has_syncing = true,
                Some(_) => {}
                None => return Err("Syncthing returned a folder without a state".into()),
            }
        }
        let active_files = self.active_files(&folders_to_check)?;

        let state = if paused {
            "paused"
        } else if has_syncing {
            "syncing"
        } else if has_scanning {
            "scanning"
        } else if folders.is_empty() {
            "no-folders"
        } else {
            "idle"
        };

        Ok(SyncStatus {
            state: state.into(),
            paused,
            download_rate,
            upload_rate,
            active_files,
        })
    }

    pub(crate) fn set_paused(&self, paused: bool) -> Result<(), String> {
        let action = if paused { "pause" } else { "resume" };
        let url = format!("{}/rest/system/{action}", self.base_url);
        self.agent
            .post(&url)
            .set("X-API-Key", &self.api_key)
            .call()
            .map_err(|error| format!("Could not {action} Syncthing: {error}"))?;
        Ok(())
    }

    fn get_json(&self, path: &str, query: &[(&str, &str)]) -> Result<Value, String> {
        let url = format!("{}{}", self.base_url, path);
        let mut request = self.agent.get(&url).set("X-API-Key", &self.api_key);
        for (name, value) in query {
            request = request.query(name, value);
        }
        let response = request
            .call()
            .map_err(|error| format!("Could not read Syncthing status: {error}"))?;
        let body = response
            .into_string()
            .map_err(|error| format!("Could not read Syncthing response: {error}"))?;
        serde_json::from_str(&body)
            .map_err(|error| format!("Syncthing returned invalid status data: {error}"))
    }

    fn rates(&self, received: u64, sent: u64) -> Result<(u64, u64), String> {
        let mut sample = self
            .rate_sample
            .lock()
            .map_err(|_| "Syncthing rate state is unavailable".to_string())?;
        let now = Instant::now();
        let rates = sample
            .map(|previous| calculate_rates(previous, received, sent, now))
            .unwrap_or((0, 0));
        *sample = Some(RateSample {
            received,
            sent,
            at: now,
        });
        Ok(rates)
    }

    fn active_files(&self, folders: &[(String, String)]) -> Result<Vec<ActiveFile>, String> {
        let now = Instant::now();
        if let Some(sample) = self
            .active_file_sample
            .lock()
            .map_err(|_| "Syncthing transfer state is unavailable".to_string())?
            .as_ref()
            .filter(|sample| now.duration_since(sample.at) < Duration::from_secs(15))
        {
            return Ok(sample.files.clone());
        }

        let mut files = Vec::new();
        for (id, label) in folders {
            let needed = self.get_json(
                "/rest/db/need",
                &[("folder", id), ("page", "1"), ("perpage", "3")],
            )?;
            let progress = needed
                .get("progress")
                .and_then(Value::as_array)
                .ok_or("Syncthing returned an invalid transfer list")?;
            for file in progress.iter().take(3usize.saturating_sub(files.len())) {
                let name = file
                    .get("name")
                    .and_then(Value::as_str)
                    .ok_or("Syncthing returned an active file without a name")?;
                files.push(ActiveFile {
                    folder: label.clone(),
                    name: name.to_string(),
                    size: required_u64(file, "size")?,
                });
            }
            if files.len() == 3 {
                break;
            }
        }
        *self
            .active_file_sample
            .lock()
            .map_err(|_| "Syncthing transfer state is unavailable".to_string())? =
            Some(ActiveFileSample {
                files: files.clone(),
                at: now,
            });
        Ok(files)
    }
}

fn calculate_rates(previous: RateSample, received: u64, sent: u64, now: Instant) -> (u64, u64) {
    let elapsed = now.duration_since(previous.at).as_secs_f64().max(0.001);
    (
        (received.saturating_sub(previous.received) as f64 / elapsed) as u64,
        (sent.saturating_sub(previous.sent) as f64 / elapsed) as u64,
    )
}

fn required_u64(value: &Value, name: &str) -> Result<u64, String> {
    value
        .get(name)
        .and_then(Value::as_u64)
        .ok_or_else(|| format!("Syncthing returned an invalid {name} value"))
}

fn read_external_api_key() -> Result<String, String> {
    if let Some(api_key) = env::var("STGUIAPIKEY")
        .ok()
        .filter(|key| !key.trim().is_empty())
    {
        return Ok(api_key);
    }
    let config_file = config_file_path()?;
    let xml = fs::read_to_string(&config_file).map_err(|error| {
        format!(
            "Could not read Syncthing configuration at {}: {error}",
            config_file.display()
        )
    })?;
    parse_api_key(&xml)
}

fn config_file_path() -> Result<PathBuf, String> {
    if let Some(config_dir) = env::var_os("STCONFDIR").filter(|value| !value.is_empty()) {
        return Ok(PathBuf::from(config_dir).join("config.xml"));
    }
    if let Some(home_dir) = env::var_os("STHOMEDIR").filter(|value| !value.is_empty()) {
        return Ok(PathBuf::from(home_dir).join("config.xml"));
    }

    let home = env::var_os("HOME")
        .map(PathBuf::from)
        .ok_or("Could not determine the home directory for Syncthing configuration")?;
    let candidates = [
        home.join("Library/Application Support/Syncthing/config.xml"),
        home.join(".config/syncthing/config.xml"),
    ];
    candidates
        .iter()
        .find(|path| path.is_file())
        .cloned()
        .ok_or_else(|| {
            format!(
                "Could not find Syncthing configuration at {}",
                candidates[0].display()
            )
        })
}

fn parse_api_key(xml: &str) -> Result<String, String> {
    let config: SyncthingConfig = quick_xml::de::from_str(xml)
        .map_err(|_| "Could not parse the Syncthing configuration".to_string())?;
    config
        .gui
        .api_key
        .filter(|key| !key.trim().is_empty())
        .ok_or_else(|| "Syncthing configuration does not contain a GUI API key".into())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::{
        io::{BufRead, BufReader, Write},
        net::TcpListener,
        thread,
    };

    #[test]
    fn parses_gui_api_key_without_exposing_it_in_errors() {
        let config = r#"<configuration><gui><apikey>private-key</apikey></gui></configuration>"#;
        assert_eq!(parse_api_key(config).unwrap(), "private-key");

        let error = parse_api_key("<configuration><gui></configuration>").unwrap_err();
        assert!(!error.contains("private-key"));
    }

    #[test]
    fn rejects_config_without_gui_api_key() {
        let error = parse_api_key("<configuration><gui></gui></configuration>").unwrap_err();
        assert!(error.contains("does not contain a GUI API key"));
    }

    #[test]
    fn transfer_rates_use_counter_deltas() {
        let now = Instant::now();
        let previous = RateSample {
            received: 100,
            sent: 200,
            at: now - Duration::from_secs(2),
        };
        let rates = calculate_rates(previous, 500, 600, now);
        assert!((190..=210).contains(&rates.0));
        assert!((190..=210).contains(&rates.1));
    }

    #[test]
    fn transfer_rates_handle_counter_resets() {
        let previous = RateSample {
            received: 500,
            sent: 600,
            at: Instant::now() - Duration::from_secs(1),
        };
        assert_eq!(calculate_rates(previous, 10, 20, Instant::now()), (0, 0));
    }

    #[test]
    fn fetch_returns_live_status_and_active_downloads() {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let address = listener.local_addr().unwrap();
        let responses = [
            (
                "/rest/system/connections",
                r#"{"total":{"inBytesTotal":1024,"outBytesTotal":2048}}"#,
            ),
            ("/rest/config/devices", r#"[{"paused":false}]"#),
            (
                "/rest/config/folders",
                r#"[{"id":"folder id","label":"Photos"}]"#,
            ),
            ("/rest/db/status", r#"{"state":"syncing"}"#),
            (
                "/rest/db/need",
                r#"{"progress":[{"name":"photo.jpg","size":4096}]}"#,
            ),
        ];
        let server = thread::spawn(move || {
            for (expected_path, body) in responses {
                let (mut stream, _) = listener.accept().unwrap();
                let mut reader = BufReader::new(stream.try_clone().unwrap());
                let mut request_line = String::new();
                reader.read_line(&mut request_line).unwrap();
                assert!(request_line.contains(expected_path), "{request_line}");
                let mut has_api_key = false;
                loop {
                    let mut line = String::new();
                    reader.read_line(&mut line).unwrap();
                    if line == "\r\n" || line.is_empty() {
                        break;
                    }
                    if line
                        .to_ascii_lowercase()
                        .starts_with("x-api-key: panel-test")
                    {
                        has_api_key = true;
                    }
                }
                assert!(has_api_key);
                write!(
                    stream,
                    "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
                    body.len(),
                    body
                )
                .unwrap();
            }
        });

        let client = StatusClient::new(format!("http://{address}"), "panel-test");
        let status = client.fetch().unwrap();
        server.join().unwrap();

        assert_eq!(status.state, "syncing");
        assert_eq!(status.active_files.len(), 1);
        assert_eq!(status.active_files[0].folder, "Photos");
        assert_eq!(status.active_files[0].name, "photo.jpg");
        assert_eq!(status.active_files[0].size, 4096);
        let serialized = serde_json::to_value(status).unwrap();
        assert_eq!(serialized["downloadRate"], 0);
        assert_eq!(serialized["activeFiles"][0]["name"], "photo.jpg");
    }
}
