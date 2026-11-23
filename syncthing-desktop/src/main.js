const { invoke } = window.__TAURI__.core;
const statusElement = document.querySelector("#status");
const errorElement = document.querySelector("#error");
const loaderElement = document.querySelector("#loader");
const retryButton = document.querySelector("#retry");

async function connect() {
  retryButton.hidden = true;
  errorElement.hidden = true;
  loaderElement.hidden = false;
  statusElement.textContent = "Starting Syncthing…";

  try {
    const url = await invoke("connect_to_syncthing");
    window.location.replace(url);
  } catch (error) {
    loaderElement.hidden = true;
    statusElement.textContent = "Syncthing could not be started.";
    errorElement.textContent = String(error);
    errorElement.hidden = false;
    retryButton.hidden = false;
  }
}

window.addEventListener("DOMContentLoaded", () => {
  retryButton.addEventListener("click", connect);
  connect();
});
