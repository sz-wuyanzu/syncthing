# Syncthing Desktop

Syncthing Desktop is a Tauri dashboard for a locally running Syncthing service.
It can start Syncthing without opening a terminal or browser, shows device and
folder status, and provides pause/resume controls. The full Syncthing web
interface remains available in the system browser.

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
menu can reopen the dashboard, open the full web interface, pause or resume all
folders, or **Quit and stop syncing**. The app prevents a second desktop
instance from starting.

Desktop and Syncthing process logs are written to the app's operating-system
log directory as `desktop.log` and `syncthing.log`.

Build an installer with:

```sh
npm run build
```

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
