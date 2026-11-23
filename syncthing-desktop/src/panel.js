const { invoke } = window.__TAURI__.core;
const stateElement = document.querySelector("#state");
const dotElement = document.querySelector("#status-dot");
const downloadRateElement = document.querySelector("#download-rate");
const uploadRateElement = document.querySelector("#upload-rate");
const filesElement = document.querySelector("#files");
const fileCountElement = document.querySelector("#file-count");
const emptyFilesElement = document.querySelector("#empty-files");
const errorElement = document.querySelector("#error");
const toggleSyncButton = document.querySelector("#toggle-sync");

const stateLabels = {
  syncing: "Syncing",
  scanning: "Scanning",
  idle: "Up to date",
  paused: "Paused",
  "no-folders": "No folders configured",
};

let latestStatus;
let refreshInProgress = false;

function formatRate(bytesPerSecond) {
  const units = ["B/s", "KB/s", "MB/s", "GB/s"];
  let value = bytesPerSecond;
  let unit = 0;
  while (value >= 1024 && unit < units.length - 1) {
    value /= 1024;
    unit += 1;
  }
  return `${value.toFixed(unit === 0 ? 0 : 1)} ${units[unit]}`;
}

function formatSize(bytes) {
  if (bytes < 1024) return `${bytes} B`;
  const units = ["KB", "MB", "GB", "TB"];
  let value = bytes / 1024;
  let unit = 0;
  while (value >= 1024 && unit < units.length - 1) {
    value /= 1024;
    unit += 1;
  }
  return `${value.toFixed(1)} ${units[unit]}`;
}

function renderFiles(files) {
  filesElement.replaceChildren();
  fileCountElement.textContent = String(files.length);
  emptyFilesElement.hidden = files.length > 0;

  for (const file of files) {
    const item = document.createElement("li");
    const details = document.createElement("div");
    const name = document.createElement("strong");
    const folder = document.createElement("span");
    const size = document.createElement("span");

    item.className = "file";
    details.className = "file-details";
    name.className = "file-name";
    name.textContent = file.name;
    folder.className = "folder-name";
    folder.textContent = file.folder;
    size.className = "file-size";
    size.textContent = formatSize(file.size);
    details.append(name, folder);
    item.append(details, size);
    filesElement.append(item);
  }
}

function render(status) {
  latestStatus = status;
  stateElement.textContent = stateLabels[status.state] ?? status.state;
  dotElement.dataset.state = status.state;
  downloadRateElement.textContent = formatRate(status.downloadRate);
  uploadRateElement.textContent = formatRate(status.uploadRate);
  toggleSyncButton.textContent = status.paused ? "Resume syncing" : "Pause syncing";
  toggleSyncButton.disabled = false;
  renderFiles(status.activeFiles);
  errorElement.hidden = true;
}

async function refresh() {
  if (refreshInProgress) return;
  refreshInProgress = true;
  try {
    render(await invoke("get_sync_status"));
  } catch (error) {
    stateElement.textContent = "Status unavailable";
    dotElement.dataset.state = "error";
    downloadRateElement.textContent = "—";
    uploadRateElement.textContent = "—";
    renderFiles([]);
    toggleSyncButton.disabled = true;
    errorElement.textContent = String(error);
    errorElement.hidden = false;
  } finally {
    refreshInProgress = false;
  }
}

document.querySelector("#close").addEventListener("click", () => invoke("hide_status_panel"));
document.querySelector("#open-main").addEventListener("click", () => invoke("open_main_window"));
toggleSyncButton.addEventListener("click", async () => {
  toggleSyncButton.disabled = true;
  errorElement.hidden = true;
  try {
    await invoke("set_sync_paused", { paused: !latestStatus.paused });
    await refresh();
  } catch (error) {
    errorElement.textContent = String(error);
    errorElement.hidden = false;
    toggleSyncButton.disabled = false;
  }
});
window.addEventListener("keydown", (event) => {
  if (event.key === "Escape") invoke("hide_status_panel");
});

refresh();
window.setInterval(refresh, 5000);
