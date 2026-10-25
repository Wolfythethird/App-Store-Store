# App Store Store

App Store Store, also known as **PMM** (Package Manager Manager), helps you
download and set up package-manager applications themselves. It does **not**
browse or install applications from their catalogs.

The app is written in Rust. Its default interface is a native, cross-platform
dark-mode desktop GUI built with [egui](https://www.egui.rs/). A keyboard-driven
terminal UI is also available.

## Build and run

Install Rust using [rustup](https://rustup.rs/), then run:

```text
cargo build --release
cargo run
cargo run -- --tui
```

## Desktop GUI

The desktop GUI uses a dark, card-based layout with manager search, setup
details, and download/install/setup actions. It filters entries for the
current operating system and checks installation status only for manager rows
visible on screen. Installers always require confirmation before running.
Long-running operations run in the background so the window remains
responsive.

## Website

The static product website is in [`web/`](./web/). Open `web/index.html`
directly in a browser or serve the folder with any static web server. Its
manager examples and supplied desktop screenshots are informational;
installation actions are performed by the native desktop application. The user
guides and local plugin documentation are in [`web/docs/`](./web/docs/),
starting with [`web/docs/index.html`](./web/docs/index.html).

To serve the website and release downloads locally, run
`python web/server.py` (or `py -3 web/server.py` on Windows) and open
`http://127.0.0.1:8000/`. The download page is at
`http://127.0.0.1:8000/downloads.html`. The server binds to localhost by
default; use `--host` and `--port` to change the interface or port. To rebuild
and publish the Windows app executable, run `cargo build --release` and copy
`target/release/app-store-store.exe` to
`web/downloads/pmm_installer.exe`. The download is the portable application
executable, not an MSI or setup wizard.

## TUI

Run `cargo run -- --tui` to use the original terminal interface. The TUI
filters the YAML registry for the current OS and checks installation status
only for visible rows.

| Key | Action |
| --- | --- |
| `d` | Download a standalone installer, when provided |
| `i` | Install/setup; downloaded installers require a `y` confirmation |
| `o` | Open the official setup page |
| `q` / `Esc` | Quit |

## CLI

```text
cargo run -- --list
cargo run -- --download brew
cargo run -- --install brew
cargo run -- --install brew --yes
```

Built-in or plugin-provided download installers must be downloaded before
they can be run. Local plugin scripts run directly after confirmation.
`--yes` confirms only the explicitly selected manager installer; it never
installs applications through the manager. Use `--plugins-dir PATH` to load
local installer plugins from a different directory.

## Manager registry

The project-root [`sources.list`](./sources.list) is the YAML registry for
system package managers and language ecosystem tools. It includes the listed
Windows, macOS, and Linux distribution managers, plus npm, Yarn, pnpm, Bun,
pip, uv, Poetry, conda, Cargo, Composer, RubyGems, Maven, Gradle, vcpkg, Conan,
Go modules, Nix, mobile-development tools, additional language ecosystems,
container and infrastructure tools, BSD/Solaris managers, GNU Guix, and asdf.
OS support, official URLs, installer scripts, and executable checks are
configured there. Tools without a documented standalone installer link to
their official setup instructions.

The registry supports separate installer profiles per operating system.
Homebrew has explicit Linux and macOS profiles. Official installers are also configured for uv, Bun, pnpm, Poetry, Cargo (via
rustup), Nix, conda, Stack, Clojure CLI (Linux), OPAM, Helm, and GNU Guix on
their supported platforms. Command-based profiles use existing tools such as
Homebrew, Winget, Chocolatey, npm, pip, RubyGems, ghcup, and Coursier, and
check their declared prerequisites before running. Many ecosystem tools are
already bundled with their runtime or are registries rather than independently
installable applications; those entries provide the official setup
instructions instead of assuming prerequisites or running an unrelated
package manager automatically.

Some managers are distributed by the operating system or require a
version-specific installer rather than a portable standalone download. For
those entries, the app directs you to the official setup page instead. The
downloadable scripts run only after explicit confirmation and may modify the
system or fetch additional files.

Managers are filtered by their declared operating-system support. Entries
that are built into or require another runtime show that prerequisite in the
selected-manager details; the app does not install missing runtimes for you.
When a command-based installer is configured, `i` runs that command only
after confirmation. For example, Yarn needs Node.js/npm, and CocoaPods needs
RubyGems.

### Local installer plugins

Place one YAML manifest and its script in
`~/.app_store_store/plugins/` (on Windows, under `%USERPROFILE%\.app_store_store\plugins\`).
Override the directory with `APP_STORE_PLUGINS` or `--plugins-dir PATH`.
Scripts must be `.sh` (`installer_type: bash`), `.ps1`
(`installer_type: powershell`), or `.py` (`installer_type: python`) and must be
stored inside the plugin directory.
The app discovers YAML/YML manifests, and asks for explicit confirmation
before running each plugin script. Plugins run with the current user's
permissions and are not sandboxed, so only use scripts you trust.
Manifests can instead provide `installer_url`, `installer_file`, and
`installer_type` to use the existing HTTPS download and confirmation flow.
For different installers by operating system, use an `installers` list with
`os`, `installer_url`, `installer_file`, and `installer_type` per profile.
An optional `arch` list restricts a profile to matching Rust architecture
names such as `x86_64` or `aarch64`; unsupported profiles are hidden.

Example `my-manager.yml`:

```yaml
id: my-manager
name: My Manager
os: windows
description: Custom manager installed by a local script.
homepage: https://example.org/
install_page: https://example.org/install
installer_type: powershell
plugin_script: install.ps1
installed_check: [my-manager]
```

For a downloaded installer, replace `plugin_script` with fields such as:

```yaml
installer_url: https://example.org/install.sh
installer_file: install.sh
installer_type: bash
```

Installers downloaded from the built-in registry are stored under
`~/.app_store_store/downloads/<manager-id>/`. Override this with
`APP_STORE_DOWNLOADS`; override the YAML registry path with `APP_STORE_SOURCES`
or `--sources-file PATH`.

## Tests

```text
cargo test
```
