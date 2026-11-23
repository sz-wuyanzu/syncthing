const { invoke } = window.__TAURI__.core;

const elements = {
  status: document.querySelector("#service-status"),
  errorCard: document.querySelector("#error-card"),
  errorTitle: document.querySelector("#error-title"),
  errorMessage: document.querySelector("#error-message"),
  retry: document.querySelector("#retry"),
  welcome: document.querySelector("#welcome"),
  deviceId: document.querySelector("#device-id"),
  deviceCount: document.querySelector("#device-count"),
  folderCount: document.querySelector("#folder-count"),
  version: document.querySelector("#version"),
  folders: document.querySelector("#folders"),
  emptyState: document.querySelector("#empty-state"),
  notice: document.querySelector("#notice"),
  lastUpdated: document.querySelector("#last-updated"),
  serviceUrl: document.querySelector("#service-url"),
  refresh: document.querySelector("#refresh"),
  openWeb: document.querySelector("#open-web"),
  emptyOpenWeb: document.querySelector("#empty-open-web"),
  pauseAll: document.querySelector("#pause-all"),
  resumeAll: document.querySelector("#resume-all"),
};

let connection;
let refreshTimer;
let refreshing = false;

function displayError(error, title = "操作失败") {
  elements.errorTitle.textContent = title;
  elements.errorMessage.textContent = error?.message ?? String(error);
  elements.errorCard.hidden = false;
}

function setConnected(connected) {
  elements.status.textContent = connected ? "运行中" : "连接中断";
  elements.status.classList.toggle("is-offline", !connected);
}

async function connect() {
  clearInterval(refreshTimer);
  elements.errorCard.hidden = true;
  elements.status.textContent = "正在连接";
  elements.status.classList.remove("is-offline");
  elements.retry.disabled = true;

  try {
    connection = await invoke("connect_to_syncthing");
    elements.openWeb.disabled = false;
    elements.refresh.disabled = false;
    elements.serviceUrl.textContent = connection.guiUrl;
    elements.welcome.textContent = connection.managed
      ? "你的 Syncthing 桌面服务已就绪"
      : "已连接到正在运行的 Syncthing";
    elements.deviceId.textContent = connection.deviceId
      ? `本机设备 ID：${connection.deviceId}`
      : "已连接本机 Syncthing 服务";
    setConnected(true);
    await refreshDashboard();
    refreshTimer = setInterval(refreshDashboard, 5000);
  } catch (error) {
    setConnected(false);
    displayError(error, "无法连接 Syncthing");
  } finally {
    elements.retry.disabled = false;
  }
}

async function refreshDashboard() {
  if (!connection || refreshing) return;
  refreshing = true;
  elements.refresh.disabled = true;

  try {
    const snapshot = await invoke("get_dashboard");
    setConnected(snapshot.connected);
    elements.welcome.textContent = snapshot.managed
      ? "你的 Syncthing 桌面服务已就绪"
      : "已连接到正在运行的 Syncthing";
    elements.deviceId.textContent = snapshot.deviceId
      ? `本机设备 ID：${snapshot.deviceId}`
      : "已连接本机 Syncthing 服务";
    elements.deviceCount.textContent = snapshot.detailsAvailable
      ? `${snapshot.connectedDevices} / ${snapshot.totalDevices}`
      : "—";
    elements.folderCount.textContent = snapshot.detailsAvailable
      ? String(snapshot.folders.length)
      : "—";
    elements.version.textContent = snapshot.version || "—";
    elements.lastUpdated.textContent = `更新于 ${new Date().toLocaleTimeString()}`;
    elements.notice.hidden = snapshot.detailsAvailable;
    elements.notice.textContent = snapshot.detailsAvailable
      ? ""
      : "已连接服务，但无法读取管理 API。可打开完整界面查看状态；请确认此桌面应用与 Syncthing 使用同一配置目录。";
    elements.pauseAll.disabled = !snapshot.detailsAvailable || snapshot.folders.length === 0;
    elements.resumeAll.disabled = !snapshot.detailsAvailable || snapshot.folders.length === 0;
    renderFolders(snapshot.folders, snapshot.detailsAvailable);
    elements.errorCard.hidden = true;
  } catch (error) {
    setConnected(false);
    displayError(error, "读取同步状态失败");
  } finally {
    refreshing = false;
    elements.refresh.disabled = false;
  }
}

function renderFolders(folders, controlsAvailable) {
  elements.folders.replaceChildren();
  elements.emptyState.hidden = folders.length > 0;

  for (const folder of folders) {
    const card = document.createElement("article");
    card.className = "folder-card";

    const copy = document.createElement("div");
    copy.className = "folder-copy";
    const title = document.createElement("h3");
    title.textContent = folder.label || folder.id;
    const identifier = document.createElement("p");
    identifier.className = "folder-id";
    identifier.textContent = folder.id;
    copy.append(title, identifier);

    const state = document.createElement("span");
    state.className = `folder-state${folder.paused ? " is-paused" : ""}`;
    state.textContent = folder.paused ? "已暂停" : humanizeState(folder.state);

    const stats = document.createElement("div");
    stats.className = "folder-stats";
    stats.append(
      stat(`${folder.localFiles.toLocaleString()} / ${folder.globalFiles.toLocaleString()}`, "本地 / 全局文件"),
      stat(folder.needFiles.toLocaleString(), "待同步文件"),
      stat(formatBytes(folder.needBytes), "待同步数据"),
      stat(folder.errors ? folder.errors.toLocaleString() : "无", "错误")
    );

    const actions = document.createElement("div");
    actions.className = "folder-actions";
    const button = document.createElement("button");
    button.className = "button button-small button-secondary";
    button.type = "button";
    button.disabled = !controlsAvailable;
    button.textContent = folder.paused ? "恢复" : "暂停";
    button.addEventListener("click", async () => {
      button.disabled = true;
      try {
        await invoke("set_folder_paused", {
          folderId: folder.id,
          paused: !folder.paused,
        });
        await refreshDashboard();
      } catch (error) {
        displayError(error);
      } finally {
        button.disabled = false;
      }
    });
    actions.append(state, button);
    card.append(copy, stats, actions);
    elements.folders.append(card);
  }
}

function stat(value, label) {
  const item = document.createElement("div");
  item.className = "folder-stat";
  const strong = document.createElement("strong");
  strong.textContent = value;
  const caption = document.createElement("span");
  caption.textContent = label;
  item.append(strong, caption);
  return item;
}

function humanizeState(state) {
  return ({
    idle: "已同步",
    paused: "已暂停",
    scanning: "正在扫描",
    syncing: "正在同步",
    error: "发生错误",
    cleaning: "正在整理",
  })[state] ?? state ?? "未知状态";
}

function formatBytes(bytes) {
  if (!bytes) return "0 B";
  const units = ["B", "KB", "MB", "GB", "TB"];
  const index = Math.min(Math.floor(Math.log(bytes) / Math.log(1024)), units.length - 1);
  return `${(bytes / 1024 ** index).toFixed(index ? 1 : 0)} ${units[index]}`;
}

async function openWeb() {
  try {
    await invoke("open_web_gui");
  } catch (error) {
    displayError(error);
  }
}

async function setAllFoldersPaused(paused) {
  elements.pauseAll.disabled = true;
  elements.resumeAll.disabled = true;
  try {
    await invoke("set_all_folders_paused", { paused });
    await refreshDashboard();
  } catch (error) {
    displayError(error);
  } finally {
    elements.pauseAll.disabled = false;
    elements.resumeAll.disabled = false;
  }
}

window.addEventListener("DOMContentLoaded", () => {
  elements.retry.addEventListener("click", connect);
  elements.refresh.addEventListener("click", refreshDashboard);
  elements.openWeb.addEventListener("click", openWeb);
  elements.emptyOpenWeb.addEventListener("click", openWeb);
  elements.pauseAll.addEventListener("click", () => setAllFoldersPaused(true));
  elements.resumeAll.addEventListener("click", () => setAllFoldersPaused(false));
  connect();
});
