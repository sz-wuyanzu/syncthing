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

Build an installer with:

```sh
npm run build
```

Installers currently expect Syncthing to be installed separately; bundling the
Syncthing executable is not configured yet.
