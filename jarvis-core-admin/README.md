# Jarvis Core Administration

Linux-only Tauri 2 + Vue 3 graphical administration companion for a Jarvis
Home Node. It targets Ubuntu 26.04 LTS, GNOME/Wayland and x86_64. It complements
the canonical `sudo jarvis` CLI/TUI; it does not replace the CLI for SSH,
recovery or automation.

## Security model

The WebKit/Tauri process always runs as the logged-in desktop user. Startup
fails when its effective UID is root. Do not launch it with `sudo`.

The frontend has no shell, filesystem, HTTP or process plugin. Its only native
surface is the explicitly registered command list in
`src-tauri/src/lib.rs`. Rust maps those commands to fixed operations:

- read-only status, health, service, update, model, credential and log queries;
- a fixed read of the root-controlled active agent manifest;
- explicitly typed Core update, agent update and model-policy mutations.

Privileged calls use `/usr/bin/pkexec` and the system's GNOME polkit agent. A
password is never accepted by Vue, Tauri IPC, command arguments or stdin.
`/usr/local/sbin/jarvis` must be a regular root-owned executable that is not
group/other-writable. The backend never invokes a shell, accepts no executable
name or unit from the frontend, clears the child environment and bounds all
captured output.

The existing Jarvis CLI, updater and helpers remain authoritative. GUI
confirmation is presentation only and never substitutes for authorization or
trusted helper validation.

At startup the installed application opens one narrow root broker through the
system authorization dialog. All later requests in that unlocked GUI session
use the typed broker, so they do not ask for the password repeatedly. Pointer
or keyboard activity renews the local session. After five inactive minutes the
frontend hides all administration views and the backend terminates the root
broker. Unlocking always creates a new system-authenticated broker. Closing the
application also terminates it.

The canonical `sudo jarvis` TUI already has one sudo authentication for its
whole process and does not use the graphical broker.

## Development

Install Node.js 24, Rust, and the Tauri Linux build prerequisites, then build as
the normal desktop user. On Ubuntu 26.04 the native packages are:

```bash
sudo apt install build-essential curl file libayatana-appindicator3-dev \
  libdbus-1-dev libssl-dev librsvg2-dev libwebkit2gtk-4.1-dev \
  libxdo-dev pkg-config wget
```

The package installation is the only step here requiring `sudo`. Never use
`sudo npm`, `sudo cargo` or `sudo tauri`.

```bash
cd jarvis-core-admin
npm ci
npm run build
npm run tauri:dev
```

The installed production `/usr/local/sbin/jarvis` boundary is used for live
data. Live development also requires a reviewed, root-owned
`/usr/bin/jarvis-core-admin`, because the mutable development binary is never
executed as root. GNOME shows one polkit authorization prompt when the app is
unlocked. Cancelling authorization is reported as a persistent GUI error.

Run native tests and lints independently of the server workspace:

```bash
cargo fmt --manifest-path src-tauri/Cargo.toml --all -- --check
cargo clippy --manifest-path src-tauri/Cargo.toml --all-targets -- -D warnings
cargo test --manifest-path src-tauri/Cargo.toml
```

## Debian package

Build the `.deb` on the Ubuntu 26.04 x86_64 target system:

```bash
npm run tauri:build
```

Tauri writes the package below `src-tauri/target/release/bundle/deb/`. The
bundle includes the normal desktop entry and application metadata. Install the
reviewed package with the operating-system package manager; the application
itself remains unprivileged after installation.

## Component versions and Core releases

Core (`jarvis-api`), the admin CLI (`jarvis-admin`) and this Core Admin App have
independent stable SemVer package versions. Keep the three app version fields
in `package.json`, `src-tauri/Cargo.toml` and `src-tauri/tauri.conf.json`
identical; `scripts/verify-versions.sh` enforces this.

The canonical Core release archive records all three component versions. It
also contains the exact GUI executable, desktop entry, icon and app version
file. The trusted Core updater installs or restores these alongside the CLI
after Core readiness succeeds. A new bundle release can therefore advertise
an update when Core, CLI, the graphical app, or multiple components changed.

## Scope

The initial navigation contains Overview, Health, Services, Update, Agents,
Models, Credentials, Logs and System/About. Credential values are never shown
or entered. Logs are bounded, control-sequence sanitized, structured where
possible, searchable/filterable, selectable, wrapped and optionally followed
through bounded polling of the allowlisted direct CLI operation.
