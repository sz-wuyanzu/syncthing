# Syncthing Desktop

Syncthing Desktop is a Tauri shell around Syncthing's existing web GUI. It starts
the Syncthing process without opening a browser or terminal window, displays the
existing management interface in a desktop window, and keeps running in the
system tray when the window is closed.

## Requirements

- Node.js and npm
- Rust and the platform prerequisites listed in the
  [Tauri setup guide](https://tauri.app/start/prerequisites/)
- Syncthing installed on the system

The app checks common installation paths and then `PATH`. Set `SYNCTHING_BIN`
to the executable path if Syncthing is installed elsewhere.

## Development

From this directory:

```sh
npm install
npm run dev
```

The desktop shell binds the managed Syncthing GUI to `127.0.0.1:8384` and
generates a private API key for its shutdown action. Closing the window hides it
to the tray; choose **Quit and stop syncing** from the tray menu to stop the
Syncthing process started by this app. If a Syncthing GUI is already running on
that address, the app connects to it and leaves that externally managed process
running when the desktop app exits.

On macOS, click the tray icon to open a compact status panel with a native
translucent material. It shows the sync state, transfer rates, and up to three
active downloads, and provides controls to pause or resume syncing or open the
full interface. Transfer details refresh less frequently than the summary to
limit calls to Syncthing's more expensive file-need endpoint.

For an externally managed Syncthing instance, the panel reads the GUI API key
from the standard macOS configuration file (or `STCONFDIR`, `STHOMEDIR`, or
`STGUIAPIKEY` when set for this app). If the key is unavailable, the full
interface remains accessible and the panel explains why status is unavailable.

Build an installer with:

```sh
npm run build
```

Installers currently expect Syncthing to be installed separately; bundling the
Syncthing executable is not configured yet.
