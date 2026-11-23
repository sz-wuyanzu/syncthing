# Syncthing Desktop

Syncthing Desktop is a native control console for a locally running Syncthing
service. It manages service start, stop, and restart; remote devices; synced
folders; and common discovery, relay, NAT, and bandwidth settings without
requiring the Syncthing web interface or a terminal.

The dashboard can add devices and folders, select a local folder through the
native directory picker, change folder sharing, pause or resume syncing, and
request folder scans. Removing a folder removes only its Syncthing
configuration; it does not delete the local directory or files. Stopping a
Syncthing process started outside the desktop app requires confirmation.

## Requirements

- Node.js and npm
- Rust and the platform prerequisites listed in the
  [Tauri setup guide](https://tauri.app/start/prerequisites/)

The app bundles the official [Syncthing v2.1.7](https://github.com/syncthing/syncthing/releases/tag/v2.1.7)
executable for the build platform. Set `SYNCTHING_BIN` only if you want to
override the bundled version.

## Development

From this directory:

```sh
npm install
npm run dev
```

On startup, the app first looks for a reachable Syncthing GUI on the local
machine. It checks `SYNCTHING_GUI_URL`, `STGUIADDRESS`, the discovered
`config.xml`, and the default `127.0.0.1:8384` address. The GUI address must use
localhost or a loopback IP; remote hosts are deliberately not accepted. The app
reads the API key from `STGUIAPIKEY` or the local Syncthing configuration to
show dashboard details. HTTPS connections use normal certificate validation;
the app does not bypass certificate errors.

If no existing GUI is found, the app starts Syncthing on the default GUI port
when available, otherwise on a free local port, and generates a private API
key. It retries when the selected port becomes unavailable during startup. The
generated key is used only for that managed process.

Closing the window hides it in the system tray; syncing continues. The tray
menu can reopen the dashboard, pause or resume all folders, or quit the desktop
app. Quitting stops a Syncthing process started by the app; an externally
managed service is left running unless it is explicitly stopped in the
dashboard. The app prevents a second desktop instance from starting.

Desktop and Syncthing process logs are written to the app's operating-system
log directory as `desktop.log` and `syncthing.log`.

Build an installer with:

```sh
npm run build
```

On Windows, build a per-user x64 NSIS installer with:

```powershell
npm run build:windows
```

The Windows installer installs for the current user and does not require
administrator elevation. It is written to
`src-tauri/target/x86_64-pc-windows-msvc/release/bundle/nsis/`.
Syncthing configuration and data remain in `%LOCALAPPDATA%\Syncthing`, and
desktop application logs remain in the Tauri application log directory.

Uninstalling displays an opt-in prompt for deleting local Syncthing
configuration, database, cache, and desktop application data. The local
Syncthing configuration, database, and desktop application data can be
removed, but synced folders outside those directories are never removed.

The `prepare:syncthing` script downloads the matching official release archive,
verifies its SHA-256 digest, and stages the executable and license under the
ignored Tauri build directory. `npm run dev` and `npm run build` run this step
automatically. Syncthing's data and configuration remain in the user's normal
Syncthing directories.

The macOS workflow ad-hoc signs the app bundle, but does not use a Developer ID
or notarize it. Gatekeeper may require manually approving the app; seamless
first launch requires Apple Developer ID signing and notarization.

The `Build Syncthing Desktop (macOS)` GitHub Actions workflow runs for changes
to this app on the `desktop` branch and can also be started manually. Download
the generated DMG from the run's **Artifacts** section; artifacts are retained
for 30 days.
