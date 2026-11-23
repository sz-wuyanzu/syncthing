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
  overviewFolders: document.querySelector("#overview-folders"),
  devices: document.querySelector("#devices"),
  devicesEmpty: document.querySelector("#devices-empty"),
  folders: document.querySelector("#folders"),
  foldersEmpty: document.querySelector("#folders-empty"),
  folderDeviceOptions: document.querySelector("#folder-device-options"),
  notice: document.querySelector("#notice"),
  lastUpdated: document.querySelector("#last-updated"),
  serviceUrl: document.querySelector("#service-url"),
  refresh: document.querySelector("#refresh"),
  serviceAction: document.querySelector("#service-action"),
  restartService: document.querySelector("#restart-service"),
  pauseAll: document.querySelector("#pause-all"),
  resumeAll: document.querySelector("#resume-all"),
  deviceForm: document.querySelector("#device-form"),
  folderForm: document.querySelector("#folder-form"),
  settingsForm: document.querySelector("#settings-form"),
  folderPath: document.querySelector("#folder-path-input"),
};

let connection;
let snapshot;
let refreshTimer;
let refreshing = false;
let serviceConnected = false;

function showNotice(message) {
  elements.notice.textContent = message;
  elements.notice.hidden = !message;
}

function displayError(error, title = "操作失败") {
  elements.errorTitle.textContent = title;
  elements.errorMessage.textContent = error?.message ?? String(error);
  elements.errorCard.hidden = false;
}

function setConnected(connected) {
  serviceConnected = connected;
  elements.status.textContent = connected ? "运行中" : "服务未连接";
  elements.status.classList.toggle("is-offline", !connected);
  elements.serviceAction.textContent = connected ? "停止服务" : "启动服务";
  elements.serviceAction.disabled = false;
  elements.restartService.disabled = !connected;
  elements.refresh.disabled = !connected;
}

async function connect() {
  clearInterval(refreshTimer);
  elements.errorCard.hidden = true;
  showNotice("");
  elements.status.textContent = "正在连接";
  elements.status.classList.add("is-offline");
  elements.serviceAction.disabled = true;
  elements.restartService.disabled = true;
  elements.refresh.disabled = true;
  elements.retry.disabled = true;

  try {
    connection = await invoke("connect_to_syncthing");
    elements.serviceUrl.textContent = connection.guiUrl;
    setConnected(true);
    await refreshDashboard();
    refreshTimer = setInterval(refreshDashboard, 5000);
  } catch (error) {
    connection = null;
    setConnected(false);
    displayError(error, "无法连接 Syncthing");
  } finally {
    elements.retry.disabled = false;
    elements.serviceAction.disabled = false;
  }
}

async function refreshDashboard() {
  if (!connection || refreshing) return;
  refreshing = true;

  try {
    snapshot = await invoke("get_dashboard");
    setConnected(snapshot.connected);
    elements.welcome.textContent = snapshot.managed
      ? "Syncthing 由桌面看板启动并管理"
      : "已连接到外部运行的 Syncthing";
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
    elements.notice.textContent = snapshot.detailsAvailable
      ? ""
      : "已连接服务，但没有可用的 API 管理权限。请检查本机 Syncthing 配置中的 GUI API key。";
    elements.notice.hidden = snapshot.detailsAvailable;
    elements.pauseAll.disabled = !snapshot.detailsAvailable || snapshot.folders.length === 0;
    elements.resumeAll.disabled = !snapshot.detailsAvailable || snapshot.folders.length === 0;
    renderOverviewFolders(snapshot.folders);
    renderDevices(snapshot.devices, snapshot.detailsAvailable);
    renderFolders(snapshot.folders, snapshot.devices, snapshot.detailsAvailable);
    renderFolderDeviceOptions(snapshot.devices);
    elements.errorCard.hidden = true;
  } catch (error) {
    setConnected(false);
    displayError(error, "读取同步状态失败");
  } finally {
    refreshing = false;
  }
}

function renderOverviewFolders(folders) {
  elements.overviewFolders.replaceChildren();
  for (const folder of folders) {
    const card = document.createElement("article");
    card.className = "overview-folder";
    const title = document.createElement("strong");
    title.textContent = folder.label || folder.id;
    const path = document.createElement("span");
    path.textContent = folder.path;
    const state = document.createElement("span");
    state.className = `folder-state${folder.paused ? " is-paused" : ""}`;
    state.textContent = folder.paused ? "已暂停" : humanizeState(folder.state);
    card.append(title, path, state);
    elements.overviewFolders.append(card);
  }
  if (folders.length === 0) {
    elements.overviewFolders.append(emptyMessage("还没有同步文件夹", "在“文件夹”页面添加本机目录。"));
  }
}

function renderDevices(devices, controlsAvailable) {
  elements.devices.replaceChildren();
  elements.devicesEmpty.hidden = devices.length > 0;
  for (const device of devices) {
    const card = document.createElement("article");
    card.className = "management-card";
    const copy = document.createElement("div");
    copy.className = "management-copy";
    const title = document.createElement("h3");
    title.textContent = device.name || device.id;
    const identifier = document.createElement("p");
    identifier.className = "identifier";
    identifier.textContent = device.id;
    const details = document.createElement("p");
    details.className = "management-detail";
    details.textContent = device.connected
      ? [device.connection_type, device.connection_address, device.client_version].filter(Boolean).join(" · ")
      : `地址：${device.addresses.join(", ") || "动态发现"}`;
    copy.append(title, identifier, details);

    const state = document.createElement("span");
    state.className = `folder-state${device.paused ? " is-paused" : ""}`;
    state.textContent = device.paused ? "已暂停" : device.connected ? "已连接" : "未连接";
    const actions = document.createElement("div");
    actions.className = "management-actions";
    const toggle = button(device.paused ? "恢复" : "暂停", "button button-small button-secondary");
    toggle.disabled = !controlsAvailable;
    toggle.addEventListener("click", () => runAction(async () => {
      await invoke("set_device_paused", { deviceId: device.id, paused: !device.paused });
      await refreshDashboard();
    }));
    const remove = button("移除", "button button-small button-danger");
    remove.disabled = !controlsAvailable;
    remove.addEventListener("click", () => runAction(async () => {
      if (!window.confirm(`移除设备“${device.name || device.id}”？这只会移除本机的设备配置。`)) return;
      await invoke("remove_device", { deviceId: device.id });
      showNotice("设备配置已移除。");
      await refreshDashboard();
    }));
    actions.append(state, toggle, remove);
    card.append(copy, actions);
    elements.devices.append(card);
  }
}

function renderFolders(folders, devices, controlsAvailable) {
  elements.folders.replaceChildren();
  elements.foldersEmpty.hidden = folders.length > 0;
  for (const folder of folders) {
    const card = document.createElement("article");
    card.className = "management-card folder-management-card";
    const copy = document.createElement("div");
    copy.className = "management-copy";
    const title = document.createElement("h3");
    title.textContent = folder.label || folder.id;
    const identifier = document.createElement("p");
    identifier.className = "identifier";
    identifier.textContent = `${folder.id} · ${folder.folderType || "sendreceive"}`;
    const path = document.createElement("p");
    path.className = "management-detail";
    path.textContent = folder.path;
    const shareText = document.createElement("p");
    shareText.className = "management-detail";
    shareText.textContent = `共享设备：${folder.sharedDevices
      .map((id) => devices.find((device) => device.id === id)?.name || id)
      .join(", ") || "仅本机"}`;
    copy.append(title, identifier, path, shareText);

    const state = document.createElement("span");
    state.className = `folder-state${folder.paused ? " is-paused" : ""}`;
    state.textContent = folder.paused ? "已暂停" : humanizeState(folder.state);
    const actions = document.createElement("div");
    actions.className = "management-actions";
    const toggle = button(folder.paused ? "恢复" : "暂停", "button button-small button-secondary");
    toggle.disabled = !controlsAvailable;
    toggle.addEventListener("click", () => runAction(async () => {
      await invoke("set_folder_paused", { folderId: folder.id, paused: !folder.paused });
      await refreshDashboard();
    }));
    const scan = button("立即扫描", "button button-small button-secondary");
    scan.disabled = !controlsAvailable || folder.paused;
    scan.addEventListener("click", () => runAction(async () => {
      await invoke("scan_folder", { folderId: folder.id });
      showNotice(`已请求扫描“${folder.label || folder.id}”。`);
      await refreshDashboard();
    }));
    const remove = button("移除配置", "button button-small button-danger");
    remove.disabled = !controlsAvailable;
    remove.addEventListener("click", () => runAction(async () => {
      if (!window.confirm(`移除“${folder.label || folder.id}”的 Syncthing 配置？本机目录及文件不会被删除。`)) return;
      await invoke("remove_folder", { folderId: folder.id });
      showNotice("文件夹配置已移除，本机文件保持不变。");
      await refreshDashboard();
    }));
    actions.append(state, toggle, scan, remove);

    const sharing = document.createElement("details");
    sharing.className = "sharing-editor";
    const summary = document.createElement("summary");
    summary.textContent = "管理共享设备";
    const checkboxes = document.createElement("div");
    checkboxes.className = "checkbox-list";
    const save = button("保存共享设置", "button button-small");
    save.disabled = !controlsAvailable;
    for (const device of devices) {
      checkboxes.append(checkboxOption(device.id, device.name, folder.sharedDevices.includes(device.id), `share-${folder.id}`));
    }
    if (devices.length === 0) checkboxes.append(emptyMessage("没有其他设备", "请先添加设备。"));
    save.addEventListener("click", () => runAction(async () => {
      const selected = [...checkboxes.querySelectorAll("input:checked")].map((input) => input.value);
      await invoke("set_folder_devices", { folderId: folder.id, deviceIds: selected });
      showNotice("文件夹共享设置已更新。");
      await refreshDashboard();
    }));
    sharing.append(summary, checkboxes, save);
    card.append(copy, actions, sharing);
    elements.folders.append(card);
  }
}

function renderFolderDeviceOptions(devices) {
  elements.folderDeviceOptions.replaceChildren();
  if (devices.length === 0) {
    elements.folderDeviceOptions.append(emptyMessage("尚无其他设备", "可以先添加文件夹，再稍后设置共享设备。"));
    return;
  }
  for (const device of devices) {
    elements.folderDeviceOptions.append(
      checkboxOption(device.id, `${device.name} · ${device.connected ? "已连接" : "未连接"}`, false, "new-folder")
    );
  }
}

function checkboxOption(value, labelText, checked, group) {
  const label = document.createElement("label");
  label.className = "checkbox-option";
  const input = document.createElement("input");
  input.type = "checkbox";
  input.value = value;
  input.checked = checked;
  input.name = group;
  const text = document.createElement("span");
  text.textContent = labelText;
  label.append(input, text);
  return label;
}

function button(text, className) {
  const element = document.createElement("button");
  element.type = "button";
  element.className = className;
  element.textContent = text;
  return element;
}

function emptyMessage(titleText, messageText) {
  const element = document.createElement("div");
  element.className = "inline-empty";
  const title = document.createElement("strong");
  title.textContent = titleText;
  const message = document.createElement("span");
  message.textContent = messageText;
  element.append(title, message);
  return element;
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

async function runAction(action) {
  try {
    await action();
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

async function stopService() {
  if (!connection || !serviceConnected) return connect();
  const target = connection.managed
    ? "停止桌面看板启动的 Syncthing 进程？"
    : "当前 Syncthing 由其他方式启动。停止它可能影响其他依赖该服务的程序，确定继续吗？";
  if (!window.confirm(target)) return;
  elements.serviceAction.disabled = true;
  try {
    await invoke("stop_syncthing");
    connection = null;
    clearInterval(refreshTimer);
    setConnected(false);
    showNotice("Syncthing 已停止。点击“启动服务”可重新连接或启动桌面内置服务。");
  } catch (error) {
    displayError(error, "停止服务失败");
  } finally {
    elements.serviceAction.disabled = false;
  }
}

async function restartService() {
  if (!connection || !window.confirm("重启 Syncthing 服务？同步将在短暂中断后自动恢复。")) return;
  elements.restartService.disabled = true;
  try {
    await invoke("restart_syncthing");
    showNotice("已请求重启 Syncthing，正在等待服务恢复…");
    setTimeout(connect, 2500);
  } catch (error) {
    displayError(error, "重启服务失败");
    elements.restartService.disabled = false;
  }
}

async function loadNetworkSettings() {
  try {
    const settings = await invoke("get_network_settings");
    document.querySelector("#global-discovery").checked = settings.globalDiscoveryEnabled;
    document.querySelector("#local-discovery").checked = settings.localDiscoveryEnabled;
    document.querySelector("#relays").checked = settings.relaysEnabled;
    document.querySelector("#nat").checked = settings.natEnabled;
    document.querySelector("#max-send").value = settings.maxSendKbps;
    document.querySelector("#max-recv").value = settings.maxRecvKbps;
  } catch (error) {
    displayError(error, "读取网络设置失败");
  }
}

window.addEventListener("DOMContentLoaded", () => {
  elements.retry.addEventListener("click", connect);
  elements.refresh.addEventListener("click", refreshDashboard);
  elements.serviceAction.addEventListener("click", stopService);
  elements.restartService.addEventListener("click", restartService);
  elements.pauseAll.addEventListener("click", () => setAllFoldersPaused(true));
  elements.resumeAll.addEventListener("click", () => setAllFoldersPaused(false));

  document.querySelectorAll(".tab").forEach((tab) => {
    tab.addEventListener("click", () => {
      document.querySelectorAll(".tab").forEach((item) => item.classList.toggle("is-active", item === tab));
      document.querySelectorAll(".view").forEach((view) => {
        view.hidden = view.id !== tab.dataset.view;
      });
      if (tab.dataset.view === "settings-view") loadNetworkSettings();
    });
  });

  elements.deviceForm.addEventListener("submit", (event) => runAction(async () => {
    event.preventDefault();
    const addresses = document.querySelector("#device-addresses-input").value
      .split(/[\n,]+/)
      .map((address) => address.trim())
      .filter(Boolean);
    await invoke("add_device", {
      deviceId: document.querySelector("#device-id-input").value.trim(),
      name: document.querySelector("#device-name-input").value.trim(),
      addresses,
    });
    elements.deviceForm.reset();
    showNotice("设备已添加。请在另一台设备上确认并添加本机设备 ID。");
    await refreshDashboard();
  }));

  elements.folderForm.addEventListener("submit", (event) => runAction(async () => {
    event.preventDefault();
    const deviceIds = [...elements.folderDeviceOptions.querySelectorAll("input:checked")]
      .map((input) => input.value);
    await invoke("add_folder", {
      folderId: document.querySelector("#folder-id-input").value.trim(),
      label: document.querySelector("#folder-label-input").value.trim(),
      path: elements.folderPath.value.trim(),
      deviceIds,
    });
    elements.folderForm.reset();
    showNotice("文件夹已添加。");
    await refreshDashboard();
  }));

  document.querySelector("#choose-folder").addEventListener("click", () => runAction(async () => {
    const path = await invoke("choose_folder");
    if (path) elements.folderPath.value = path;
  }));

  elements.settingsForm.addEventListener("submit", (event) => runAction(async () => {
    event.preventDefault();
    const maxSend = Number(document.querySelector("#max-send").value);
    const maxRecv = Number(document.querySelector("#max-recv").value);
    if (!Number.isInteger(maxSend) || maxSend < 0 || !Number.isInteger(maxRecv) || maxRecv < 0) {
      throw new Error("带宽限制必须是大于或等于 0 的整数。");
    }
    await invoke("set_network_settings", {
      globalDiscoveryEnabled: document.querySelector("#global-discovery").checked,
      localDiscoveryEnabled: document.querySelector("#local-discovery").checked,
      relaysEnabled: document.querySelector("#relays").checked,
      natEnabled: document.querySelector("#nat").checked,
      maxSendKbps: maxSend,
      maxRecvKbps: maxRecv,
    });
    showNotice("网络设置已保存。");
    await loadNetworkSettings();
  }));

  connect();
});
