use std::time::{Duration, Instant};

use serde::Serialize;
use serde_json::Value;

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

    pub(crate) fn matches_target(&self, base_url: &str, api_key: &str) -> bool {
        self.base_url == base_url.trim_end_matches('/') && self.api_key == api_key
    }

    pub(crate) fn fetch(&self) -> Result<SyncStatus, String> {
        let connections = self.get_json("/rest/system/connections", &[])?;
        let totals = connections
            .get("total")
            .ok_or("Syncthing 未返回连接统计信息")?;
        let received = required_u64(totals, "inBytesTotal")?;
        let sent = required_u64(totals, "outBytesTotal")?;
        let (download_rate, upload_rate) = self.rates(received, sent)?;

        let devices = self.get_json("/rest/config/devices", &[])?;
        let devices = devices
            .as_array()
            .ok_or("Syncthing 返回的设备列表格式无效")?;
        let paused = !devices.is_empty()
            && devices
                .iter()
                .all(|device| device.get("paused").and_then(Value::as_bool) == Some(true));

        let folders = self.get_json("/rest/config/folders", &[])?;
        let folders = folders
            .as_array()
            .ok_or("Syncthing 返回的文件夹列表格式无效")?;
        let mut has_scanning = false;
        let mut has_syncing = false;
        let mut folders_to_check = Vec::with_capacity(folders.len());

        for folder in folders {
            let id = folder
                .get("id")
                .and_then(Value::as_str)
                .filter(|id| !id.is_empty())
                .ok_or("Syncthing 返回的文件夹缺少 ID")?;
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
                None => return Err("Syncthing 返回的文件夹缺少同步状态".into()),
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
        let (action, action_label) = if paused {
            ("pause", "暂停")
        } else {
            ("resume", "恢复")
        };
        let url = format!("{}/rest/system/{action}", self.base_url);
        self.agent
            .post(&url)
            .set("X-API-Key", &self.api_key)
            .call()
            .map_err(|error| format!("{action_label} Syncthing 失败：{error}"))?;
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
            .map_err(|error| format!("读取 Syncthing 同步状态失败：{error}"))?;
        let body = response
            .into_string()
            .map_err(|error| format!("读取 Syncthing 响应失败：{error}"))?;
        serde_json::from_str(&body)
            .map_err(|error| format!("Syncthing 返回的同步状态数据无效：{error}"))
    }

    fn rates(&self, received: u64, sent: u64) -> Result<(u64, u64), String> {
        let mut sample = self
            .rate_sample
            .lock()
            .map_err(|_| "Syncthing 传输速率状态不可用".to_string())?;
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
            .map_err(|_| "Syncthing 下载状态不可用".to_string())?
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
                .ok_or("Syncthing 返回的下载列表格式无效")?;
            for file in progress.iter().take(3usize.saturating_sub(files.len())) {
                let name = file
                    .get("name")
                    .and_then(Value::as_str)
                    .ok_or("Syncthing 返回的下载项缺少文件名")?;
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
            .map_err(|_| "Syncthing 下载状态不可用".to_string())? = Some(ActiveFileSample {
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
        .ok_or_else(|| format!("Syncthing 返回的 {name} 字段无效"))
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
