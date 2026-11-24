const { invoke } = window.__TAURI__.core;
const { listen } = window.__TAURI__.event;

const stateElement = document.querySelector("#state");
const dotElement = document.querySelector("#status-dot");
const downloadRateElement = document.querySelector("#download-rate");
const uploadRateElement = document.querySelector("#upload-rate");
const filesElement = document.querySelector("#files");
const fileCountElement = document.querySelector("#file-count");
const emptyFilesElement = document.querySelector("#empty-files");
const errorElement = document.querySelector("#error");
const moreFilesElement = document.querySelector("#more-files");
const toggleSyncButton = document.querySelector("#toggle-sync");

const panelElement = document.querySelector("#panel");
const compactLogoElement = document.querySelector("#compact-logo");
const collapseButton = document.querySelector("#collapse-panel");

const stateLabels = {
  syncing: "正在同步",
  scanning: "正在扫描",
  idle: "已同步",
  paused: "已暂停",
  "no-folders": "尚未配置同步文件夹",
};

let latestStatus;
let refreshInProgress = false;
let compact = true;

function formatRate(bytesPerSecond) {
  const units = ["B/秒", "KB/秒", "MB/秒", "GB/秒"];
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

function setMode(nextCompact) {
  if (compact === nextCompact) return;
  window.clearTimeout(expandTimer);
  window.clearTimeout(collapseTimer);
  compact = nextCompact;
  document.body.classList.toggle("is-compact", compact);
  document.body.classList.toggle("is-expanded", !compact);
  lastContentSize = null;
  invoke("set_status_panel_mode", { compact }).catch((error) => {
    console.error("Could not resize status panel:", error);
  });
  if (!compact) refresh();
}

const panelResizeObserver = new ResizeObserver(() => {
  if (compact || dragging) return;
  const width = Math.ceil(panelElement.scrollWidth);
  const height = Math.ceil(panelElement.scrollHeight);
  if (!width || !height) return;
  if (
    lastContentSize &&
    Math.abs(lastContentSize.width - width) < 3 &&
    Math.abs(lastContentSize.height - height) < 3
  ) {
    return;
  }
  lastContentSize = { width, height };
  invoke("resize_status_panel", { width, height }).catch((error) => {
    console.error("Could not auto-size status panel:", error);
    lastContentSize = null;
  });
});
panelResizeObserver.observe(panelElement);

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
  const label = stateLabels[status.state] ?? `同步状态：${status.state}`;
  stateElement.textContent = label;
  dotElement.dataset.state = status.state;
  downloadRateElement.textContent = formatRate(status.downloadRate);
  uploadRateElement.textContent = formatRate(status.uploadRate);
  toggleSyncButton.textContent = status.paused ? "继续同步" : "暂停同步";
  toggleSyncButton.disabled = false;
  renderFiles(status.activeFiles);
  const remaining = (status.totalNeedFiles ?? status.activeFiles.length) - status.activeFiles.length;
  moreFilesElement.hidden = remaining <= 0;
  if (remaining > 0) moreFilesElement.textContent = `还有 ${remaining} 个文件待下载`;
  errorElement.hidden = true;
}

async function refresh() {
  // The compact badge shows no details; poll only while the panel is open.
  if (refreshInProgress || compact) return;
  refreshInProgress = true;
  try {
    render(await invoke("get_sync_status"));
  } catch (error) {
    const message = String(error);
    stateElement.textContent = "同步状态暂不可用";
    dotElement.dataset.state = "error";
    downloadRateElement.textContent = "—";
    uploadRateElement.textContent = "—";
    renderFiles([]);
    moreFilesElement.hidden = true;
    toggleSyncButton.disabled = true;
    errorElement.textContent = message;
    errorElement.hidden = false;
  } finally {
    refreshInProgress = false;
  }
}

let dragStart = null;
let dragging = false;
let justDragged = false;
let expandTimer = null;
let collapseTimer = null;
let lastContentSize = null;

compactLogoElement.addEventListener("mouseenter", () => {
  window.clearTimeout(expandTimer);
  expandTimer = window.setTimeout(() => {
    if (!dragging) setMode(false);
  }, 220);
});
compactLogoElement.addEventListener("mouseleave", () => {
  window.clearTimeout(expandTimer);
});
panelElement.addEventListener("mouseenter", () => {
  window.clearTimeout(collapseTimer);
});
panelElement.addEventListener("mouseleave", () => {
  window.clearTimeout(collapseTimer);
  collapseTimer = window.setTimeout(() => {
    if (!dragging) setMode(true);
  }, 200);
});

compactLogoElement.addEventListener("pointerdown", (event) => {
  if (event.button !== 0) return;
  window.clearTimeout(expandTimer);
  dragStart = { x: event.clientX, y: event.clientY };
  dragging = false;
  justDragged = false;
});
compactLogoElement.addEventListener("pointermove", (event) => {
  if (dragging || !dragStart) return;
  const distance = Math.hypot(event.clientX - dragStart.x, event.clientY - dragStart.y);
  if (distance < 4) return;
  dragging = true;
  justDragged = true;
  invoke("start_status_panel_drag").catch((error) => {
    console.error("Could not start status panel drag:", error);
  });
});
compactLogoElement.addEventListener("pointerup", () => {
  justDragged = dragging;
  if (!dragging && compact) setMode(false);
  dragStart = null;
  dragging = false;
  window.setTimeout(() => {
    justDragged = false;
  }, 300);
});
compactLogoElement.addEventListener("click", () => {
  if (justDragged) {
    justDragged = false;
    return;
  }
  setMode(false);
});
document
  .querySelector("#panel-drag-handle")
  .addEventListener("pointerdown", (event) => {
    if (event.button === 0) {
      invoke("start_status_panel_drag").catch((error) => {
        console.error("Could not start status panel drag:", error);
      });
    }
  });
collapseButton.addEventListener("click", () => setMode(true));
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
  if (event.key !== "Escape") return;
  if (compact) invoke("hide_status_panel");
  else setMode(true);
});
listen("panel-blur", () => {
  if (!compact) setMode(true);
});
// Refresh the moment the panel is shown instead of serving up to 5s-old
// data from the last time it was open.
window.addEventListener("focus", refresh);

refresh();
window.setInterval(refresh, 2000);
