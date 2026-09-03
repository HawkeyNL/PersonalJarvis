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
- explicitly typed Core update, agent update and model-policy mutations;
- a typed credential-provider action that opens a separate trusted terminal.

Privileged calls use `/usr/bin/pkexec` and the system's GNOME polkit agent. A
password is never accepted by Vue, Tauri IPC, command arguments or stdin.
`/usr/local/sbin/jarvis` must be a regular root-owned executable that is not
group/other-writable. The backend never invokes a shell, accepts no executable
name or unit from the frontend, clears the child environment and bounds all
captured output.

The existing Jarvis CLI, updater and helpers remain authoritative. GUI
confirmation is presentation only and never substitutes for authorization or
trusted helper validation.

Credential **Set** and **Replace** actions never create a secret field in the
webview. The normal-user application opens the exact active Core Admin binary
in GNOME Ptyxis (or GNOME Terminal) credential-entry mode. The binary remains
unprivileged and must be root-owned in production, or owned by the current
normal user for development, executable and not group/other-writable. It
invokes only `pkexec /usr/local/sbin/jarvis credentials set <typed-provider>`;
the active verified credential helper reads hidden input directly from the
controlling terminal. Only the allowlisted provider name enters argv. The
secret never enters Vue state, Tauri IPC, captured output or a command line.

At startup the installed application opens one narrow root broker through the
system authorization dialog. All later requests in that unlocked GUI session
use the typed broker, so they do not ask for the password repeatedly. Pointer
or keyboard activity renews the local session. After five inactive minutes the
frontend hides all administration views and the backend terminates the root
broker. Unlocking always creates a new system-authenticated broker. Closing the
application also terminates it.

After a trusted Core update or rollback succeeds, the running GUI is treated as
stale because its installed executable may have been replaced. A mandatory
`Restart now` dialog prevents further administration through the old process;
restarting first closes the privileged broker. The production app also compares
its running executable with the fixed root-owned installed executable every 15
seconds, so an update completed from `sudo jarvis` is detected. Development mode
does not compare itself with the installed production binary.

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

The visible desktop entry is installed as
`com.hawkeynl.jarvis.core.admin.desktop`, matching the GTK/Wayland application
ID. Its icon remains the fixed `jarvis-core-admin` hicolor asset. This exact
identity lets GNOME Shell associate search results and running dock windows
with the same application instead of displaying its generic fallback icon.

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

The categorized navigation contains Overview, Health, Services, Logs, Agents,
Models, Usage & Costs, Credentials, Update and System/About. The Models view
can filter and sort exact reviewed per-million-token prices, shows at most 25
rows per page and presents Hugging Face route selection in a focused modal;
unknown remote prices remain visibly unknown. Usage & Costs shows bounded
current-month request, token and estimated-cost aggregates by day, provider
and model. Its responsive stacked daily token chart uses a narrowly registered
Chart.js build; budget and provider progress indicators remain native
application UI. No prompt, reply, credential or request identifier enters
either view. Credential values are never shown or stored by the GUI. Logs are
bounded, control-sequence sanitized,
structured where possible, searchable/filterable, selectable, wrapped and
optionally followed through bounded polling of the allowlisted direct CLI
operation.

Core writes the bounded Usage & Costs aggregate at startup, after metered
model calls and on a periodic recovery interval. A not-yet-created snapshot is
shown as an initializing empty state instead of a generic failed-operation
panel.

The Agents page obtains its tree through the canonical protected admin CLI's
bounded safe projection. It never reads the private checkout, agent JSON files,
or Markdown prompt bodies. **Check** compares the configured, allowlisted
`HawkeyNL/PersonalJarvisAgents` checkout with `origin/main`; **Update bundle**
uses the existing trusted validator and transactional Core-readiness rollback
path before the refreshed safe tree is displayed.
