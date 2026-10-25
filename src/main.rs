use std::{
    collections::HashSet,
    env,
    fs::{self, File},
    io::{self, Read, Write},
    path::{Path, PathBuf},
    process::{Command, Stdio},
    sync::mpsc::{self, Receiver, TryRecvError},
    thread,
    time::Duration,
};

use anyhow::{Context, Result, bail};
use clap::Parser;
use crossterm::{
    event::{self, Event, KeyCode, KeyEventKind},
    execute,
    terminal::{EnterAlternateScreen, LeaveAlternateScreen, disable_raw_mode, enable_raw_mode},
};
use eframe::egui;
use ratatui::{
    Terminal,
    backend::CrosstermBackend,
    layout::{Constraint, Direction, Layout, Rect},
    style::{Color, Modifier, Style},
    text::{Line, Span, Text},
    widgets::{Block, Borders, List, ListItem, ListState, Paragraph, Wrap},
};
use reqwest::blocking::Client;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

const MAX_INSTALLER_SIZE: u64 = 50 * 1024 * 1024;

#[derive(Debug, Deserialize, Clone)]
struct Registry {
    sources: Vec<Manager>,
}

#[derive(Debug, Deserialize, Serialize, Clone)]
struct InstallerProfile {
    #[serde(rename = "os", deserialize_with = "deserialize_os")]
    os: Vec<String>,
    #[serde(default)]
    arch: Vec<String>,
    #[serde(default)]
    installer_url: String,
    #[serde(default)]
    installer_file: String,
    installer_type: String,
    #[serde(default)]
    sha256: String,
    #[serde(default)]
    command: String,
    #[serde(default)]
    args: Vec<String>,
    #[serde(default)]
    requires: Vec<String>,
}

#[derive(Debug, Deserialize, Serialize, Clone)]
struct Manager {
    id: String,
    name: String,
    #[serde(rename = "os", deserialize_with = "deserialize_os")]
    os: Vec<String>,
    description: String,
    homepage: String,
    install_page: String,
    #[serde(default)]
    installer_url: String,
    #[serde(default)]
    installer_file: String,
    #[serde(default)]
    installer_type: String,
    #[serde(default)]
    installers: Vec<InstallerProfile>,
    #[serde(default)]
    plugin_script: Option<PathBuf>,
    #[serde(default)]
    installed_check: Vec<String>,
    #[serde(default)]
    requires: Vec<String>,
    #[serde(default)]
    sha256: String,
    #[serde(default)]
    system_managed: bool,
}

fn deserialize_os<'de, D>(deserializer: D) -> std::result::Result<Vec<String>, D::Error>
where
    D: serde::Deserializer<'de>,
{
    #[derive(Deserialize)]
    #[serde(untagged)]
    enum OsValue {
        One(String),
        Many(Vec<String>),
    }

    Ok(match OsValue::deserialize(deserializer)? {
        OsValue::One(os) => vec![os],
        OsValue::Many(os) => os,
    })
}

#[derive(Debug, Parser)]
#[command(
    name = "app-store-store",
    version,
    about = "Download and install package-manager applications"
)]
struct Args {
    #[arg(long, help = "Run the keyboard-driven terminal interface")]
    tui: bool,
    #[arg(long, help = "List manager apps available on this operating system")]
    list: bool,
    #[arg(long, value_name = "MANAGER_ID", help = "Download a manager installer")]
    download: Option<String>,
    #[arg(
        long,
        value_name = "MANAGER_ID",
        help = "Install a previously downloaded manager"
    )]
    install: Option<String>,
    #[arg(long, help = "Confirm installation without an interactive prompt")]
    yes: bool,
    #[arg(long, value_name = "PATH", help = "Path to the YAML manager registry")]
    sources_file: Option<PathBuf>,
    #[arg(
        long,
        value_name = "DIR",
        help = "Path to local YAML installer plugins"
    )]
    plugins_dir: Option<PathBuf>,
}

fn current_os() -> &'static str {
    if cfg!(target_os = "windows") {
        "windows"
    } else if cfg!(target_os = "macos") {
        "darwin"
    } else if cfg!(target_os = "linux") {
        "linux"
    } else if cfg!(target_os = "freebsd") {
        "freebsd"
    } else if cfg!(target_os = "dragonfly") {
        "dragonfly"
    } else if cfg!(target_os = "openbsd") {
        "openbsd"
    } else if cfg!(target_os = "solaris") {
        "solaris"
    } else if cfg!(target_os = "illumos") {
        "illumos"
    } else {
        "unknown"
    }
}

fn default_sources_file() -> PathBuf {
    env::var_os("APP_STORE_SOURCES")
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("sources.list"))
}

fn default_plugin_directory() -> PathBuf {
    env::var_os("APP_STORE_PLUGINS")
        .map(PathBuf::from)
        .unwrap_or_else(|| {
            let home = env::var_os(if cfg!(target_os = "windows") {
                "USERPROFILE"
            } else {
                "HOME"
            })
            .map(PathBuf::from)
            .unwrap_or_else(|| PathBuf::from("."));
            home.join(".app_store_store").join("plugins")
        })
}

fn download_root() -> PathBuf {
    if let Some(path) = env::var_os("APP_STORE_DOWNLOADS") {
        return PathBuf::from(path);
    }
    let home = env::var_os(if cfg!(target_os = "windows") {
        "USERPROFILE"
    } else {
        "HOME"
    })
    .map(PathBuf::from)
    .unwrap_or_else(|| PathBuf::from("."));
    home.join(".app_store_store").join("downloads")
}

fn load_managers(path: &Path) -> Result<Vec<Manager>> {
    let yaml = fs::read_to_string(path)
        .with_context(|| format!("Could not read manager registry {}", path.display()))?;
    let registry: Registry = serde_yaml::from_str(&yaml)
        .with_context(|| format!("Invalid YAML manager registry {}", path.display()))?;

    validate_managers(&registry.sources)?;
    Ok(registry.sources)
}

fn load_plugin_managers(directory: &Path) -> Result<Vec<Manager>> {
    if !directory.exists() {
        return Ok(Vec::new());
    }
    if !directory.is_dir() {
        bail!("Plugin path {} is not a directory", directory.display());
    }
    let canonical_directory = directory
        .canonicalize()
        .with_context(|| format!("Could not resolve plugin directory {}", directory.display()))?;
    let mut manifests = fs::read_dir(directory)
        .with_context(|| format!("Could not read plugin directory {}", directory.display()))?
        .map(|entry| entry.map(|entry| entry.path()))
        .collect::<std::io::Result<Vec<_>>>()
        .with_context(|| {
            format!(
                "Could not enumerate plugin directory {}",
                directory.display()
            )
        })?;
    manifests.sort();

    let mut plugins = Vec::new();
    for manifest in manifests.into_iter().filter(|path| {
        path.is_file()
            && path.extension().is_some_and(|extension| {
                extension.eq_ignore_ascii_case("yaml") || extension.eq_ignore_ascii_case("yml")
            })
    }) {
        let yaml = fs::read_to_string(&manifest)
            .with_context(|| format!("Could not read plugin manifest {}", manifest.display()))?;
        let mut manager: Manager = serde_yaml::from_str(&yaml)
            .with_context(|| format!("Invalid plugin manifest {}", manifest.display()))?;
        if let Some(script) = &manager.plugin_script {
            if script.is_absolute() {
                bail!(
                    "Plugin script in {} must use a relative path",
                    manifest.display()
                );
            }
            let script = manifest
                .parent()
                .context("Plugin manifest has no parent directory")?
                .join(script)
                .canonicalize()
                .with_context(|| {
                    format!("Could not resolve plugin script in {}", manifest.display())
                })?;
            if !script.starts_with(&canonical_directory) {
                bail!(
                    "Plugin script in {} must remain inside the plugin directory",
                    manifest.display()
                );
            }
            manager.plugin_script = Some(script);
        } else if manager.installer_url.is_empty() && manager.installers.is_empty() {
            bail!(
                "Plugin manifest {} must specify plugin_script or installer_url",
                manifest.display()
            );
        }
        plugins.push(manager);
    }
    validate_managers(&plugins)?;
    Ok(plugins)
}

fn validate_managers(managers: &[Manager]) -> Result<()> {
    let mut seen = HashSet::new();
    for (index, manager) in managers.iter().enumerate() {
        let entry = index + 1;
        if manager.id.is_empty()
            || !manager
                .id
                .chars()
                .all(|ch| ch.is_ascii_alphanumeric() || "_.+-".contains(ch))
        {
            bail!("source entry {entry}: invalid id");
        }
        if !seen.insert(&manager.id) {
            bail!("source entry {entry}: duplicate id '{}'", manager.id);
        }
        if manager.os.is_empty()
            || manager.os.iter().any(|os| {
                ![
                    "linux",
                    "windows",
                    "darwin",
                    "freebsd",
                    "dragonfly",
                    "openbsd",
                    "solaris",
                    "illumos",
                    "any",
                ]
                .contains(&os.as_str())
            })
        {
            bail!("source entry {entry}: invalid operating system");
        }
        for (field, url) in [
            ("homepage", &manager.homepage),
            ("install_page", &manager.install_page),
        ] {
            if !(url.starts_with("https://") || url.starts_with("ms-windows-store://")) {
                bail!("source entry {entry}: {field} must be HTTPS or a Microsoft Store URL");
            }
        }
        let has_download = !manager.installer_url.is_empty();
        let has_plugin_script = manager.plugin_script.is_some();
        let valid_installer_type = |installer_type: &str| {
            [
                "bash",
                "powershell",
                "python",
                "php",
                "executable",
                "command",
            ]
            .contains(&installer_type)
        };
        if has_download
            != (!manager.installer_file.is_empty() && valid_installer_type(&manager.installer_type))
        {
            bail!(
                "source entry {entry}: installer_url, installer_file, and installer_type are required together"
            );
        }
        if has_plugin_script
            && (has_download
                || !manager.installers.is_empty()
                || !manager.installer_file.is_empty()
                || !manager.sha256.is_empty()
                || !["bash", "powershell", "python"].contains(&manager.installer_type.as_str()))
        {
            bail!(
                "source entry {entry}: plugin scripts require installer_type and cannot use download fields"
            );
        }
        if let Some(script) = &manager.plugin_script {
            let expected_extension = match manager.installer_type.as_str() {
                "bash" => "sh",
                "powershell" => "ps1",
                "python" => "py",
                other => bail!("source entry {entry}: unsupported plugin script type '{other}'"),
            };
            if !script.is_file()
                || !script
                    .extension()
                    .is_some_and(|extension| extension.eq_ignore_ascii_case(expected_extension))
            {
                bail!(
                    "source entry {entry}: plugin script must be an existing .{expected_extension} file"
                );
            }
        }
        if has_download && !manager.installer_url.starts_with("https://") {
            bail!("source entry {entry}: installer_url must use HTTPS");
        }
        if has_download {
            validate_installer_fields(
                entry,
                &manager.installer_file,
                &manager.installer_type,
                &manager.sha256,
            )?;
        }
        let mut profile_os = HashSet::new();
        for profile in &manager.installers {
            if profile.os.is_empty()
                || profile.os.iter().any(|os| {
                    ![
                        "linux",
                        "windows",
                        "darwin",
                        "freebsd",
                        "dragonfly",
                        "openbsd",
                        "solaris",
                        "illumos",
                    ]
                    .contains(&os.as_str())
                        || (!manager.os.iter().any(|manager_os| manager_os == "any")
                            && !manager.os.contains(os))
                })
            {
                bail!("source entry {entry}: invalid installer operating system");
            }
            if profile.arch.iter().any(|arch| {
                ![
                    "x86_64",
                    "aarch64",
                    "x86",
                    "arm",
                    "powerpc64",
                    "s390x",
                    "riscv64",
                ]
                .contains(&arch.as_str())
            }) {
                bail!("source entry {entry}: invalid installer architecture");
            }
            for os in &profile.os {
                if !profile_os.insert(os) {
                    bail!("source entry {entry}: duplicate installer profile for '{os}'");
                }
            }
            if profile.installer_type == "php" {
                validate_command_name(entry, "php")?;
            }
            if profile.installer_type == "command" {
                if profile.command.is_empty()
                    || !profile.installer_url.is_empty()
                    || !profile.installer_file.is_empty()
                    || !profile.sha256.is_empty()
                    || profile.requires.is_empty()
                {
                    bail!(
                        "source entry {entry}: command profiles require command and prerequisites and cannot specify download fields"
                    );
                }
                validate_command_name(entry, &profile.command)?;
                for required in &profile.requires {
                    validate_command_name(entry, required)?;
                }
            } else {
                if !profile.installer_url.starts_with("https://")
                    || !valid_installer_type(&profile.installer_type)
                {
                    bail!(
                        "source entry {entry}: installer profiles require HTTPS URLs and supported installer types"
                    );
                }
                if !profile.command.is_empty() || !profile.args.is_empty() {
                    bail!(
                        "source entry {entry}: downloaded installer profiles cannot specify commands"
                    );
                }
                for required in &profile.requires {
                    validate_command_name(entry, required)?;
                }
                validate_installer_fields(
                    entry,
                    &profile.installer_file,
                    &profile.installer_type,
                    &profile.sha256,
                )?;
            }
        }
        if (!manager.installers.is_empty() && (has_download || has_plugin_script))
            || (manager.system_managed
                && (has_download || has_plugin_script || !manager.installers.is_empty()))
        {
            bail!(
                "source entry {entry}: installer profiles cannot be combined with other installers or system-managed managers"
            );
        }
        if manager.system_managed && (has_download || has_plugin_script) {
            bail!("source entry {entry}: system-managed managers cannot specify an installer");
        }
        for required in &manager.requires {
            validate_command_name(entry, required)?;
        }
    }
    Ok(())
}

fn validate_command_name(entry: usize, command: &str) -> Result<()> {
    if command.is_empty()
        || !command
            .chars()
            .all(|ch| ch.is_ascii_alphanumeric() || "_.+-".contains(ch))
    {
        bail!("source entry {entry}: invalid command name '{command}'");
    }
    Ok(())
}

fn validate_installer_fields(
    entry: usize,
    installer_file: &str,
    installer_type: &str,
    sha256: &str,
) -> Result<()> {
    if Path::new(installer_file)
        .file_name()
        .and_then(|name| name.to_str())
        != Some(installer_file)
    {
        bail!("source entry {entry}: installer_file must be a filename");
    }
    if !sha256.is_empty()
        && (sha256.len() != 64 || !sha256.chars().all(|ch| ch.is_ascii_hexdigit()))
    {
        bail!("source entry {entry}: sha256 must be a 64-character hexadecimal digest");
    }
    let expected_extension = match installer_type {
        "bash" => Some("sh"),
        "powershell" => Some("ps1"),
        "python" => Some("py"),
        "php" => Some("php"),
        "executable" => None,
        _ => bail!("source entry {entry}: unsupported installer type '{installer_type}'"),
    };
    if expected_extension.is_some_and(|extension| {
        !Path::new(installer_file)
            .extension()
            .is_some_and(|actual| actual.eq_ignore_ascii_case(extension))
    }) {
        bail!("source entry {entry}: installer_file extension does not match installer_type");
    }
    Ok(())
}

fn managers_for_os(managers: &[Manager]) -> Vec<Manager> {
    managers
        .iter()
        .filter(|manager| manager_available_on_os(manager, current_os()))
        .cloned()
        .collect()
}

fn manager_available_on_os(manager: &Manager, os: &str) -> bool {
    let os_supported = manager
        .os
        .iter()
        .any(|manager_os| manager_os == "any" || manager_os == os);
    os_supported
        && (manager.system_managed
            || manager.plugin_script.is_some()
            || active_installer_for_os(manager, os).is_some()
            || !manager.requires.is_empty()
            || !manager.os.iter().any(|manager_os| manager_os == "any"))
}

fn is_installed(manager: &Manager) -> bool {
    manager
        .installed_check
        .first()
        .is_some_and(|program| find_executable(program).is_some())
}

fn find_executable(program: &str) -> Option<PathBuf> {
    let path = Path::new(program);
    if path.components().count() > 1 {
        return path.is_file().then(|| path.to_path_buf());
    }
    let path_var = env::var_os("PATH")?;
    for directory in env::split_paths(&path_var) {
        let candidate = directory.join(program);
        if candidate.is_file() {
            return Some(candidate);
        }
        #[cfg(target_os = "windows")]
        for extension in ["exe", "cmd", "bat", "ps1"] {
            let candidate = directory.join(format!("{program}.{extension}"));
            if candidate.is_file() {
                return Some(candidate);
            }
        }
    }
    None
}

#[derive(Clone, Copy)]
struct ActiveInstaller<'a> {
    url: &'a str,
    file: &'a str,
    installer_type: &'a str,
    sha256: &'a str,
    command: &'a str,
    args: &'a [String],
    requires: &'a [String],
}

fn active_installer(manager: &Manager) -> Option<ActiveInstaller<'_>> {
    active_installer_for_os(manager, current_os())
}

fn active_installer_for_os<'a>(manager: &'a Manager, os: &str) -> Option<ActiveInstaller<'a>> {
    active_installer_for_os_and_arch(manager, os, std::env::consts::ARCH)
}

fn active_installer_for_os_and_arch<'a>(
    manager: &'a Manager,
    os: &str,
    arch: &str,
) -> Option<ActiveInstaller<'a>> {
    if !manager.installer_url.is_empty() {
        return Some(ActiveInstaller {
            url: &manager.installer_url,
            file: &manager.installer_file,
            installer_type: &manager.installer_type,
            sha256: &manager.sha256,
            command: "",
            args: &[],
            requires: &[],
        });
    }
    manager
        .installers
        .iter()
        .find(|installer| {
            installer.os.iter().any(|profile_os| profile_os == os)
                && (installer.arch.is_empty() || installer.arch.iter().any(|value| value == arch))
        })
        .map(|installer| ActiveInstaller {
            url: &installer.installer_url,
            file: &installer.installer_file,
            installer_type: &installer.installer_type,
            sha256: &installer.sha256,
            command: &installer.command,
            args: &installer.args,
            requires: &installer.requires,
        })
}

fn installer_path(manager: &Manager) -> Option<PathBuf> {
    active_installer(manager)
        .map(|installer| download_root().join(&manager.id).join(installer.file))
}

fn download_manager(manager: &Manager) -> Result<PathBuf> {
    let installer = active_installer(manager).context(
        "No standalone installer is configured for this platform; see the official setup page",
    )?;
    if installer.installer_type == "command" {
        bail!(
            "{} uses the installed {} command; press i to run its installer",
            manager.name,
            installer.command
        );
    }
    if manager.plugin_script.is_some() {
        bail!(
            "{} uses a local plugin script and does not need a download",
            manager.name
        );
    }
    let client = Client::builder()
        .timeout(Duration::from_secs(45))
        .redirect(reqwest::redirect::Policy::limited(5))
        .user_agent("AppStoreStore/0.1")
        .build()
        .context("Could not initialize HTTPS client")?;
    let mut response = client
        .get(installer.url)
        .send()
        .with_context(|| format!("Could not download {}", manager.name))?
        .error_for_status()
        .with_context(|| format!("Download failed for {}", manager.name))?;
    if response.url().scheme() != "https" {
        bail!("Installer download redirected away from HTTPS");
    }
    if response
        .content_length()
        .is_some_and(|length| length > MAX_INSTALLER_SIZE)
    {
        bail!("Installer download exceeds the 50 MiB safety limit");
    }

    let destination = installer_path(manager).context("No installer path for this platform")?;
    let parent = destination
        .parent()
        .context("Installer destination has no parent directory")?;
    fs::create_dir_all(parent)?;
    let temporary = destination.with_extension(format!(
        "{}.part",
        destination
            .extension()
            .and_then(|extension| extension.to_str())
            .unwrap_or("")
    ));
    let result = (|| -> Result<()> {
        let mut file = File::create(&temporary)?;
        let mut digest = Sha256::new();
        let mut size = 0_u64;
        let mut buffer = [0_u8; 64 * 1024];
        loop {
            let count = response.read(&mut buffer)?;
            if count == 0 {
                break;
            }
            size += count as u64;
            if size > MAX_INSTALLER_SIZE {
                bail!("Installer download exceeds the 50 MiB safety limit");
            }
            digest.update(&buffer[..count]);
            file.write_all(&buffer[..count])?;
        }
        file.flush()?;
        let digest = format!("{:x}", digest.finalize());
        if !installer.sha256.is_empty() && !digest.eq_ignore_ascii_case(installer.sha256) {
            bail!("Installer checksum mismatch for {}", manager.name);
        }
        if destination.exists() {
            fs::remove_file(&destination)?;
        }
        fs::rename(&temporary, &destination)?;
        Ok(())
    })();
    if result.is_err() {
        let _ = fs::remove_file(&temporary);
    }
    result?;
    Ok(destination)
}

fn run_interactive(command: &[String]) -> Result<()> {
    let (program, args) = command.split_first().context("Command is empty")?;
    let status = Command::new(program)
        .args(args)
        .stdin(Stdio::inherit())
        .stdout(Stdio::inherit())
        .stderr(Stdio::inherit())
        .status()
        .with_context(|| format!("Could not run command '{program}'"))?;
    if !status.success() {
        bail!("Command '{program}' exited with status {status}");
    }
    Ok(())
}

fn installer_command(installer_type: &str, path: &Path) -> Result<Vec<String>> {
    let path = path.to_string_lossy().into_owned();
    match installer_type {
        "bash" => Ok(vec!["bash".to_string(), path]),
        "powershell" => Ok(vec![
            "powershell".to_string(),
            "-NoProfile".to_string(),
            "-ExecutionPolicy".to_string(),
            "Bypass".to_string(),
            "-File".to_string(),
            path,
        ]),
        "python" => Ok(vec![
            if cfg!(target_os = "windows") {
                "python".to_string()
            } else {
                "python3".to_string()
            },
            path,
        ]),
        "php" => Ok(vec!["php".to_string(), path]),
        "executable" => Ok(vec![path]),
        other => bail!("No installer runner is configured for '{other}'"),
    }
}

fn has_installer(manager: &Manager) -> bool {
    manager.plugin_script.is_some() || active_installer(manager).is_some()
}

fn has_downloadable_installer(manager: &Manager) -> bool {
    active_installer(manager)
        .is_some_and(|installer| installer.installer_type != "command" && !installer.url.is_empty())
}

fn prerequisite_message(installer: &ActiveInstaller<'_>) -> Option<String> {
    let missing = installer
        .requires
        .iter()
        .filter(|required| find_executable(required).is_none())
        .cloned()
        .collect::<Vec<_>>();
    (!missing.is_empty()).then(|| {
        format!(
            "Install prerequisite{} first: {}",
            if missing.len() == 1 { "" } else { "s" },
            missing.join(", ")
        )
    })
}

fn install_manager(manager: &Manager, confirmed: bool) -> Result<String> {
    let missing = manager
        .requires
        .iter()
        .filter(|required| find_executable(required).is_none())
        .cloned()
        .collect::<Vec<_>>();
    if !missing.is_empty() {
        bail!(
            "Install prerequisite{} first: {}",
            if missing.len() == 1 { "" } else { "s" },
            missing.join(", ")
        );
    }
    if manager.system_managed {
        return Ok(format!(
            "{} is provided by the operating system; see {}",
            manager.name, manager.install_page
        ));
    }
    if !has_installer(manager) {
        return Ok(format!(
            "{} must be installed from its official page: {}",
            manager.name, manager.install_page
        ));
    }
    if !confirmed {
        bail!("Installing a manager changes the system; confirm the install first");
    }
    let active_installer = active_installer(manager);
    if let Some(installer) = active_installer {
        if let Some(message) = prerequisite_message(&installer) {
            bail!("{message}");
        }
        if installer.installer_type == "command" {
            let mut command = vec![installer.command.to_string()];
            command.extend(installer.args.iter().cloned());
            run_interactive(&command)?;
            return Ok(format!("{} installer completed", manager.name));
        }
    }
    let installer = if let Some(script) = &manager.plugin_script {
        script.clone()
    } else {
        installer_path(manager).context("No installer is configured for this platform")?
    };
    if !installer.is_file() {
        bail!(
            "Download the {} installer before installing it",
            manager.name
        );
    }
    let installer_type = if manager.plugin_script.is_some() {
        manager.installer_type.as_str()
    } else {
        active_installer
            .context("No installer is configured for this platform")?
            .installer_type
    };
    let command = installer_command(installer_type, &installer)?;
    run_interactive(&command)?;
    Ok(format!("{} installer completed", manager.name))
}

fn open_manager_page(manager: &Manager) -> Result<String> {
    open::that(&manager.install_page)
        .with_context(|| format!("Could not open {}", manager.install_page))?;
    Ok(format!("Opened official {} setup page", manager.name))
}

fn print_managers(managers: &[Manager]) {
    println!("{:<12} {:<28} {:<15} SETUP", "ID", "MANAGER", "INSTALLED");
    for manager in managers_for_os(managers) {
        let installed = if is_installed(&manager) { "yes" } else { "no" };
        let setup = if manager.system_managed {
            "OS / distro"
        } else if manager.plugin_script.is_some() {
            "local plugin"
        } else if active_installer(&manager)
            .is_some_and(|installer| installer.installer_type == "command")
        {
            "runtime command"
        } else if has_installer(&manager) {
            "downloadable"
        } else if !manager.requires.is_empty() {
            "requires runtime"
        } else {
            "official page"
        };
        println!(
            "{:<12} {:<28} {:<15} {}",
            manager.id, manager.name, installed, setup
        );
    }
}

struct TerminalGuard;

impl TerminalGuard {
    fn enter() -> Result<Self> {
        enable_raw_mode().context("Could not enable terminal raw mode")?;
        if let Err(error) = execute!(io::stdout(), EnterAlternateScreen) {
            let _ = disable_raw_mode();
            return Err(error).context("Could not enter alternate screen");
        }
        Ok(Self)
    }

    fn run_external<T>(operation: impl FnOnce() -> Result<T>) -> Result<T> {
        disable_raw_mode().context("Could not suspend terminal input")?;
        execute!(io::stdout(), LeaveAlternateScreen).context("Could not leave alternate screen")?;
        let result = operation();
        let restore_screen = execute!(io::stdout(), EnterAlternateScreen)
            .context("Could not restore alternate screen");
        let restore_input = enable_raw_mode().context("Could not restore terminal input");
        restore_screen?;
        restore_input?;
        result
    }
}

impl Drop for TerminalGuard {
    fn drop(&mut self) {
        let _ = disable_raw_mode();
        let _ = execute!(io::stdout(), LeaveAlternateScreen);
    }
}

struct Tui {
    manager_sources: Vec<String>,
    visible_managers: Vec<(Manager, bool)>,
    visible_start: usize,
    visible_count: usize,
    selected: usize,
    status: String,
    confirm_install: bool,
}

#[derive(Clone, Copy)]
enum GuiOperation {
    Download,
    Install,
    OpenPage,
}

struct GuiApp {
    managers: Vec<Manager>,
    installation_status: Vec<Option<bool>>,
    selected: Option<usize>,
    search: String,
    status: String,
    confirm_install: Option<Manager>,
    operation: Option<Receiver<(usize, String)>>,
}

impl GuiApp {
    fn new(managers: Vec<Manager>) -> Self {
        let installation_status = vec![None; managers.len()];
        Self {
            managers,
            installation_status,
            selected: None,
            search: String::new(),
            status: "Choose a package manager to view setup options.".into(),
            confirm_install: None,
            operation: None,
        }
    }

    fn start_operation(&mut self, manager: Manager, operation: GuiOperation) {
        let Some(manager_index) = self
            .managers
            .iter()
            .position(|entry| entry.id == manager.id)
        else {
            self.status = "The selected manager is no longer available.".into();
            return;
        };
        let (sender, receiver) = mpsc::channel();
        self.operation = Some(receiver);
        self.status = match operation {
            GuiOperation::Download => format!("Downloading {} installer…", manager.name),
            GuiOperation::Install => format!("Running {} installer…", manager.name),
            GuiOperation::OpenPage => format!("Opening {} setup page…", manager.name),
        };
        thread::spawn(move || {
            let result = match operation {
                GuiOperation::Download => download_manager(&manager)
                    .map(|path| format!("Installer saved to {}", path.display())),
                GuiOperation::Install => install_manager(&manager, true),
                GuiOperation::OpenPage => open_manager_page(&manager),
            }
            .unwrap_or_else(|error| format!("Error: {error:#}"));
            let _ = sender.send((manager_index, result));
        });
    }

    fn poll_operation(&mut self) {
        let Some(receiver) = &self.operation else {
            return;
        };
        match receiver.try_recv() {
            Ok((manager_index, message)) => {
                self.status = message;
                self.operation = None;
                if let Some(status) = self.installation_status.get_mut(manager_index) {
                    *status = None;
                }
            }
            Err(TryRecvError::Disconnected) => {
                self.status = "The background operation stopped before reporting a result.".into();
                self.operation = None;
            }
            Err(TryRecvError::Empty) => {}
        }
    }

    fn selected_manager(&self) -> Option<&Manager> {
        self.selected
            .and_then(|selected| self.managers.get(selected))
    }

    fn render_manager_row(&mut self, ui: &mut eframe::egui::Ui, index: usize) {
        if self.installation_status[index].is_none() {
            self.installation_status[index] = Some(is_installed(&self.managers[index]));
        }
        let manager = &self.managers[index];
        let installed = self.installation_status[index].unwrap_or(false);
        let setup = if manager.system_managed {
            "System managed"
        } else if manager.plugin_script.is_some() {
            "Local plugin"
        } else if active_installer(manager)
            .is_some_and(|installer| installer.installer_type == "command")
        {
            "Command installer"
        } else if has_downloadable_installer(manager) {
            "Download available"
        } else if !manager.requires.is_empty() {
            "Runtime required"
        } else {
            "Setup guide"
        };

        let selected = self.selected == Some(index);
        let response = ui
            .push_id(&manager.id, |ui| {
                egui::Frame::new()
                    .fill(if selected {
                        egui::Color32::from_rgb(39, 48, 67)
                    } else {
                        egui::Color32::from_rgb(30, 33, 40)
                    })
                    .inner_margin(egui::Margin::same(12))
                    .corner_radius(egui::CornerRadius::same(10))
                    .show(ui, |ui| {
                        ui.set_min_width(ui.available_width());
                        ui.horizontal(|ui| {
                            ui.vertical(|ui| {
                                ui.label(
                                    egui::RichText::new(&manager.name)
                                        .strong()
                                        .color(egui::Color32::from_rgb(235, 238, 245)),
                                );
                                ui.label(
                                    egui::RichText::new(setup)
                                        .small()
                                        .color(egui::Color32::from_rgb(151, 160, 176)),
                                );
                            });
                            ui.with_layout(
                                egui::Layout::right_to_left(egui::Align::Center),
                                |ui| {
                                    ui.label(
                                        egui::RichText::new(if installed {
                                            "Installed"
                                        } else {
                                            "Not installed"
                                        })
                                        .small()
                                        .color(
                                            if installed {
                                                egui::Color32::from_rgb(104, 211, 145)
                                            } else {
                                                egui::Color32::from_rgb(151, 160, 176)
                                            },
                                        ),
                                    );
                                },
                            );
                        });
                    })
                    .response
                    .interact(egui::Sense::click())
            })
            .inner;
        if response.clicked() {
            self.selected = Some(index);
        }
    }

    fn render_confirmation(&mut self, ctx: &eframe::egui::Context) {
        let Some(manager) = self.confirm_install.clone() else {
            return;
        };
        let mut decision = None;
        egui::Window::new("Confirm installation")
            .collapsible(false)
            .resizable(false)
            .anchor(egui::Align2::CENTER_CENTER, egui::Vec2::ZERO)
            .show(ctx, |ui| {
                ui.set_width(340.0);
                ui.label(format!(
                    "Run the {} installer? It may modify your system.",
                    manager.name
                ));
                ui.add_space(12.0);
                ui.horizontal(|ui| {
                    if ui.button("Cancel").clicked() {
                        decision = Some(false);
                    }
                    if ui
                        .button(egui::RichText::new("Run installer").color(egui::Color32::WHITE))
                        .clicked()
                    {
                        decision = Some(true);
                    }
                });
            });
        if let Some(confirmed) = decision {
            self.confirm_install = None;
            if confirmed {
                self.start_operation(manager, GuiOperation::Install);
            } else {
                self.status = "Installation cancelled.".into();
            }
        }
    }
}

impl eframe::App for GuiApp {
    fn update(&mut self, ctx: &eframe::egui::Context, _frame: &mut eframe::Frame) {
        self.poll_operation();
        if self.operation.is_some() {
            ctx.request_repaint_after(Duration::from_millis(100));
        }

        let query = self.search.trim().to_lowercase();
        let matching = self
            .managers
            .iter()
            .enumerate()
            .filter(|(_, manager)| {
                query.is_empty()
                    || manager.name.to_lowercase().contains(&query)
                    || manager.id.to_lowercase().contains(&query)
                    || manager.description.to_lowercase().contains(&query)
            })
            .map(|(index, _)| index)
            .collect::<Vec<_>>();
        if self
            .selected
            .is_none_or(|selected| !matching.contains(&selected))
        {
            self.selected = matching.first().copied();
        }

        egui::TopBottomPanel::top("header")
            .frame(
                egui::Frame::new()
                    .fill(egui::Color32::from_rgb(24, 26, 32))
                    .inner_margin(egui::Margin::symmetric(22, 16)),
            )
            .show(ctx, |ui| {
                ui.horizontal(|ui| {
                    ui.label(
                        egui::RichText::new("PMM")
                            .size(19.0)
                            .strong()
                            .color(egui::Color32::WHITE),
                    );
                    ui.vertical(|ui| {
                        ui.label(
                            egui::RichText::new("App Store Store")
                                .size(20.0)
                                .strong()
                                .color(egui::Color32::from_rgb(238, 241, 247)),
                        );
                        ui.label(
                            egui::RichText::new("Package Manager Manager")
                                .small()
                                .color(egui::Color32::from_rgb(151, 160, 176)),
                        );
                    });
                    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                        ui.label(
                            egui::RichText::new(current_os())
                                .small()
                                .color(egui::Color32::from_rgb(171, 190, 224)),
                        );
                    });
                });
            });

        egui::TopBottomPanel::bottom("status")
            .frame(
                egui::Frame::new()
                    .fill(egui::Color32::from_rgb(24, 26, 32))
                    .inner_margin(egui::Margin::symmetric(18, 10)),
            )
            .show(ctx, |ui| {
                ui.horizontal(|ui| {
                    if self.operation.is_some() {
                        ui.spinner();
                    }
                    ui.label(
                        egui::RichText::new(&self.status)
                            .small()
                            .color(egui::Color32::from_rgb(184, 192, 207)),
                    );
                });
            });

        egui::SidePanel::left("manager-list")
            .resizable(true)
            .default_width(330.0)
            .min_width(260.0)
            .frame(
                egui::Frame::new()
                    .fill(egui::Color32::from_rgb(20, 22, 27))
                    .inner_margin(egui::Margin::symmetric(14, 18)),
            )
            .show(ctx, |ui| {
                ui.heading("Managers");
                ui.add_space(10.0);
                ui.add(
                    egui::TextEdit::singleline(&mut self.search)
                        .hint_text("Search managers…")
                        .desired_width(f32::INFINITY),
                );
                ui.add_space(12.0);
                ui.label(
                    egui::RichText::new(format!("{} available", matching.len()))
                        .small()
                        .color(egui::Color32::from_rgb(151, 160, 176)),
                );
                ui.add_space(6.0);
                if matching.is_empty() {
                    ui.label("No managers match your search.");
                } else {
                    egui::ScrollArea::vertical()
                        .auto_shrink([false, false])
                        .show_rows(ui, 72.0, matching.len(), |ui, rows| {
                            for row in rows {
                                ui.add_space(3.0);
                                self.render_manager_row(ui, matching[row]);
                                ui.add_space(3.0);
                            }
                        });
                }
            });

        egui::CentralPanel::default()
            .frame(
                egui::Frame::new()
                    .fill(egui::Color32::from_rgb(25, 27, 33))
                    .inner_margin(egui::Margin::symmetric(26, 22)),
            )
            .show(ctx, |ui| {
                if let Some(manager) = self.selected_manager().cloned() {
                    ui.horizontal(|ui| {
                        ui.heading(
                            egui::RichText::new(&manager.name)
                                .size(25.0)
                                .strong()
                                .color(egui::Color32::from_rgb(238, 241, 247)),
                        );
                        if self
                            .selected
                            .and_then(|index| self.installation_status[index])
                            .unwrap_or(false)
                        {
                            ui.label(
                                egui::RichText::new("INSTALLED")
                                    .small()
                                    .strong()
                                    .color(egui::Color32::from_rgb(104, 211, 145)),
                            );
                        }
                    });
                    ui.add_space(8.0);
                    ui.label(
                        egui::RichText::new(&manager.description)
                            .color(egui::Color32::from_rgb(184, 192, 207)),
                    );
                    ui.add_space(20.0);

                    egui::Frame::group(ui.style())
                        .fill(egui::Color32::from_rgb(30, 33, 40))
                        .corner_radius(egui::CornerRadius::same(12))
                        .inner_margin(egui::Margin::same(16))
                        .show(ui, |ui| {
                            ui.label(
                                egui::RichText::new("Setup details")
                                    .strong()
                                    .color(egui::Color32::from_rgb(220, 225, 235)),
                            );
                            ui.add_space(8.0);
                            ui.hyperlink_to("Official homepage", &manager.homepage);
                            ui.hyperlink_to("Installation instructions", &manager.install_page);
                            if !manager.requires.is_empty() {
                                ui.add_space(8.0);
                                ui.label(format!(
                                    "Required first: {}",
                                    manager.requires.join(", ")
                                ));
                            }
                            if let Some(installer) = active_installer(&manager)
                                .filter(|installer| !installer.requires.is_empty())
                            {
                                ui.label(format!(
                                    "Installer prerequisites: {}",
                                    installer.requires.join(", ")
                                ));
                            }
                        });

                    ui.add_space(20.0);
                    let installer_ready = manager.plugin_script.is_some()
                        || !has_downloadable_installer(&manager)
                        || installer_path(&manager).is_some_and(|path| path.is_file());
                    ui.horizontal_wrapped(|ui| {
                        let busy = self.operation.is_some();
                        if ui
                            .add_enabled(
                                !busy && has_downloadable_installer(&manager),
                                egui::Button::new("Download installer"),
                            )
                            .clicked()
                        {
                            self.start_operation(manager.clone(), GuiOperation::Download);
                        }
                        if ui
                            .add_enabled(
                                !busy && installer_ready,
                                egui::Button::new("Install / set up"),
                            )
                            .clicked()
                        {
                            if !manager.system_managed && has_installer(&manager) {
                                self.confirm_install = Some(manager.clone());
                            } else {
                                self.start_operation(manager.clone(), GuiOperation::Install);
                            }
                        }
                        if ui
                            .add_enabled(!busy, egui::Button::new("Open official page"))
                            .clicked()
                        {
                            self.start_operation(manager.clone(), GuiOperation::OpenPage);
                        }
                    });
                    ui.add_space(14.0);
                    if !installer_ready {
                        ui.label(
                            egui::RichText::new("Download the installer before running it.")
                                .small()
                                .color(egui::Color32::from_rgb(229, 178, 97)),
                        );
                    } else if has_installer(&manager) && !manager.system_managed {
                        ui.label(
                            egui::RichText::new(
                                "Installers may modify your system. Confirmation is required.",
                            )
                            .small()
                            .color(egui::Color32::from_rgb(151, 160, 176)),
                        );
                    }
                } else {
                    ui.heading("No managers available");
                    ui.label("There are no package managers configured for this operating system.");
                }
            });

        self.render_confirmation(ctx);
    }
}

fn run_gui(managers: Vec<Manager>) -> Result<()> {
    let mut visuals = egui::Visuals::dark();
    visuals.panel_fill = egui::Color32::from_rgb(25, 27, 33);
    visuals.window_fill = egui::Color32::from_rgb(30, 33, 40);
    visuals.extreme_bg_color = egui::Color32::from_rgb(20, 22, 27);
    visuals.faint_bg_color = egui::Color32::from_rgb(35, 38, 46);
    visuals.selection.bg_fill = egui::Color32::from_rgb(66, 111, 196);
    let options = eframe::NativeOptions {
        viewport: egui::ViewportBuilder::default()
            .with_title("App Store Store")
            .with_inner_size([1050.0, 720.0])
            .with_min_inner_size([760.0, 520.0]),
        ..Default::default()
    };
    eframe::run_native(
        "App Store Store",
        options,
        Box::new(move |creation_context| {
            creation_context.egui_ctx.set_visuals(visuals);
            Ok(Box::new(GuiApp::new(managers)))
        }),
    )
    .map_err(|error| anyhow::anyhow!("Could not open desktop window: {error}"))
}

impl Tui {
    fn new(managers: Vec<Manager>) -> Result<Self> {
        let manager_sources = managers
            .iter()
            .filter(|manager| managers_for_os(std::slice::from_ref(manager)).len() == 1)
            .map(serde_yaml::to_string)
            .collect::<std::result::Result<Vec<_>, _>>()
            .context("Could not prepare managers for on-demand loading")?;
        Ok(Self {
            manager_sources,
            visible_managers: Vec::new(),
            visible_start: 0,
            visible_count: 0,
            selected: 0,
            status: "↑/↓ select  d download  i install/setup  o official page  q quit".into(),
            confirm_install: false,
        })
    }

    fn selected_manager(&self) -> Option<&Manager> {
        let visible_index = self.selected.checked_sub(self.visible_start)?;
        self.visible_managers
            .get(visible_index)
            .map(|(manager, _)| manager)
    }

    fn load_visible_window(&mut self, visible_count: usize) -> Result<()> {
        let visible_count = visible_count.max(1);
        let total = self.manager_sources.len();
        let count = visible_count.min(total);
        let start = self
            .selected
            .saturating_sub(count / 2)
            .min(total.saturating_sub(count));
        if start == self.visible_start && count == self.visible_count {
            return Ok(());
        }

        self.visible_managers = self.manager_sources[start..start + count]
            .iter()
            .map(|source| {
                let manager: Manager = serde_yaml::from_str(source)
                    .context("Could not load a visible manager from the registry")?;
                let installed = is_installed(&manager);
                Ok((manager, installed))
            })
            .collect::<Result<Vec<_>>>()?;
        self.visible_start = start;
        self.visible_count = count;
        Ok(())
    }

    fn draw(&mut self, terminal: &mut Terminal<CrosstermBackend<io::Stdout>>) -> Result<()> {
        let size = terminal.size()?;
        let areas = Layout::default()
            .direction(Direction::Vertical)
            .constraints([
                Constraint::Length(3),
                Constraint::Min(8),
                Constraint::Length(7),
                Constraint::Length(3),
                Constraint::Length(1),
            ])
            .split(Rect::new(0, 0, size.width, size.height));
        self.load_visible_window(areas[1].height.saturating_sub(2).max(1) as usize)?;

        terminal.draw(|frame| {
            let areas = Layout::default()
                .direction(Direction::Vertical)
                .constraints([
                    Constraint::Length(3),
                    Constraint::Min(8),
                    Constraint::Length(7),
                    Constraint::Length(3),
                    Constraint::Length(1),
                ])
                .split(frame.area());
            let title = Paragraph::new("PMM  ·  APP STORE STORE  ·  PACKAGE-MANAGER INSTALLER")
                .style(
                    Style::default()
                        .fg(Color::Cyan)
                        .add_modifier(Modifier::BOLD),
                )
                .block(Block::default().borders(Borders::ALL));
            frame.render_widget(title, areas[0]);

            let items: Vec<ListItem> = self
                .visible_managers
                .iter()
                .map(|(manager, is_installed)| {
                    let installed = if *is_installed {
                        Span::styled("  INSTALLED", Style::default().fg(Color::Green))
                    } else {
                        Span::styled("  not installed", Style::default().fg(Color::DarkGray))
                    };
                    let setup = if manager.system_managed {
                        "OS-managed"
                    } else if manager.plugin_script.is_some() {
                        "local plugin"
                    } else if active_installer(manager)
                        .is_some_and(|installer| installer.installer_type == "command")
                    {
                        "runtime command"
                    } else if has_installer(manager) {
                        "downloadable"
                    } else if !manager.requires.is_empty() {
                        "requires runtime"
                    } else {
                        "official page"
                    };
                    ListItem::new(Line::from(vec![
                        Span::styled(
                            format!("{:<22}", manager.name),
                            Style::default().add_modifier(Modifier::BOLD),
                        ),
                        installed,
                        Span::raw(format!("    {setup}")),
                    ]))
                })
                .collect();
            let list = List::new(items)
                .block(
                    Block::default()
                        .title("Package managers")
                        .borders(Borders::ALL),
                )
                .highlight_style(
                    Style::default()
                        .bg(Color::Rgb(31, 45, 67))
                        .fg(Color::White)
                        .add_modifier(Modifier::BOLD),
                )
                .highlight_symbol(" ▶ ");
            let mut list_state = ListState::default()
                .with_selected(Some(self.selected.saturating_sub(self.visible_start)));
            frame.render_stateful_widget(list, areas[1], &mut list_state);

            let detail = self.selected_manager().map_or_else(
                || "No managers configured for this OS.".to_string(),
                |manager| {
                    format!(
                        "{}\n{}\nHomepage: {}\nInstall/setup: {}{}{}",
                        manager.name,
                        manager.description,
                        manager.homepage,
                        manager.install_page,
                        if manager.requires.is_empty() {
                            String::new()
                        } else {
                            format!("\nRequires first: {}", manager.requires.join(", "))
                        },
                        active_installer(manager)
                            .filter(|installer| !installer.requires.is_empty())
                            .map(|installer| format!(
                                "\nInstaller prerequisites: {}",
                                installer.requires.join(", ")
                            ))
                            .unwrap_or_default()
                    )
                },
            );
            frame.render_widget(
                Paragraph::new(Text::from(detail))
                    .wrap(Wrap { trim: true })
                    .block(
                        Block::default()
                            .title("Selected manager")
                            .borders(Borders::ALL),
                    ),
                areas[2],
            );

            let (message, style) = if self.confirm_install {
                (
                    "Run installer? It may modify your system.  [y] Yes  [n/Esc] Cancel",
                    Style::default()
                        .fg(Color::Yellow)
                        .add_modifier(Modifier::BOLD),
                )
            } else {
                (self.status.as_str(), Style::default().fg(Color::Gray))
            };
            frame.render_widget(
                Paragraph::new(message.to_string())
                    .style(style)
                    .block(Block::default().borders(Borders::ALL)),
                areas[3],
            );
            frame.render_widget(
                Paragraph::new(
                    "This tool manages store software, not applications in those stores.",
                ),
                areas[4],
            );
        })?;
        Ok(())
    }

    fn perform(&mut self, action: char) {
        let Some(manager) = self.selected_manager().cloned() else {
            self.status = "No manager selected.".into();
            return;
        };
        let result = match action {
            'd' if manager.plugin_script.is_some() => {
                self.status = "This plugin script is local; press i to run it.".into();
                return;
            }
            'd' if !has_downloadable_installer(&manager) => {
                self.status = if active_installer(&manager)
                    .is_some_and(|installer| installer.installer_type == "command")
                {
                    "This installer uses an installed runtime command; press i to run it.".into()
                } else {
                    "No downloadable installer is configured for this platform.".into()
                };
                return;
            }
            'd' => download_manager(&manager)
                .map(|path| format!("Installer saved to {}", path.display())),
            'i' => {
                if manager.system_managed || !has_installer(&manager) {
                    install_manager(&manager, false)
                } else {
                    self.confirm_install = true;
                    return;
                }
            }
            'o' => open_manager_page(&manager),
            _ => return,
        };
        self.status = result.unwrap_or_else(|error| format!("Error: {error:#}"));
    }

    fn run(&mut self) -> Result<()> {
        let _guard = TerminalGuard::enter()?;
        let mut terminal = Terminal::new(CrosstermBackend::new(io::stdout()))?;
        terminal.clear()?;
        loop {
            self.draw(&mut terminal)?;
            if !event::poll(Duration::from_millis(200))? {
                continue;
            }
            let Event::Key(key) = event::read()? else {
                continue;
            };
            if key.kind == KeyEventKind::Release {
                continue;
            }
            if self.confirm_install {
                match key.code {
                    KeyCode::Char('y') | KeyCode::Char('Y') => {
                        self.confirm_install = false;
                        if let Some(manager) = self.selected_manager().cloned() {
                            self.status =
                                TerminalGuard::run_external(|| install_manager(&manager, true))
                                    .unwrap_or_else(|error| format!("Install failed: {error:#}"));
                            self.visible_count = 0;
                        }
                    }
                    KeyCode::Char('n') | KeyCode::Char('N') | KeyCode::Esc => {
                        self.confirm_install = false;
                        self.status = "Installation cancelled.".into();
                    }
                    _ => {}
                }
                continue;
            }
            match key.code {
                KeyCode::Char('q') | KeyCode::Esc => break,
                KeyCode::Up | KeyCode::Char('k') => {
                    self.selected = self.selected.saturating_sub(1);
                }
                KeyCode::Down | KeyCode::Char('j') => {
                    if self.selected + 1 < self.manager_sources.len() {
                        self.selected += 1;
                    }
                }
                KeyCode::Char(action @ ('d' | 'i' | 'o')) => self.perform(action),
                _ => {}
            }
        }
        Ok(())
    }
}

fn find_manager<'a>(managers: &'a [Manager], id: &str) -> Result<&'a Manager> {
    let manager = managers
        .iter()
        .find(|manager| manager.id == id)
        .with_context(|| format!("Unknown manager id '{id}'"))?;
    if !manager_available_on_os(manager, current_os()) {
        bail!("{} is not available on {}", manager.name, current_os());
    }
    Ok(manager)
}

fn main() -> Result<()> {
    let args = Args::parse();
    let sources_file = args.sources_file.unwrap_or_else(default_sources_file);
    let plugins_dir = args.plugins_dir.unwrap_or_else(default_plugin_directory);
    let mut managers = load_managers(&sources_file)?;
    managers.extend(load_plugin_managers(&plugins_dir)?);
    validate_managers(&managers)?;

    if args.list {
        print_managers(&managers);
        return Ok(());
    }
    if let Some(id) = args.download {
        let manager = find_manager(&managers, &id)?;
        if manager.system_managed
            || !has_downloadable_installer(manager)
            || manager.plugin_script.is_some()
        {
            if manager.plugin_script.is_some() {
                println!(
                    "{} uses a local plugin script; press i in the TUI to run it.",
                    manager.name
                );
            } else if active_installer(manager)
                .is_some_and(|installer| installer.installer_type == "command")
            {
                println!(
                    "{} uses an installed runtime/package-manager command; use --install {} to run it.",
                    manager.name, manager.id
                );
            } else {
                println!(
                    "{} has no standalone installer for this platform. See {}",
                    manager.name, manager.install_page
                );
            }
        } else {
            println!("{}", download_manager(manager)?.display());
        }
        return Ok(());
    }
    if let Some(id) = args.install {
        let manager = find_manager(&managers, &id)?;
        if manager.system_managed || !has_installer(manager) {
            println!("{}", install_manager(manager, false)?);
            return Ok(());
        }
        if manager.plugin_script.is_none()
            && has_downloadable_installer(manager)
            && !installer_path(manager).is_some_and(|path| path.is_file())
        {
            bail!("Download the {} installer first", manager.name);
        }
        if !args.yes {
            print!(
                "Run the {} installer? This may modify your system. [y/N] ",
                manager.name
            );
            io::stdout().flush()?;
            let mut answer = String::new();
            io::stdin().read_line(&mut answer)?;
            if !answer.trim().eq_ignore_ascii_case("y") {
                println!("Installation cancelled.");
                return Ok(());
            }
        }
        println!("{}", install_manager(manager, true)?);
        return Ok(());
    }
    let available = managers_for_os(&managers);
    if available.is_empty() {
        bail!("No manager sources configured for {}", current_os());
    }
    if args.tui {
        Tui::new(managers)?.run()
    } else {
        run_gui(available)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn loads_manager_registry() {
        let managers = load_managers(Path::new("sources.list")).unwrap();
        let ids: HashSet<&str> = managers.iter().map(|manager| manager.id.as_str()).collect();
        for id in [
            "apt",
            "dpkg",
            "pacman",
            "flatpak",
            "brew",
            "winget",
            "choco",
            "scoop",
            "dnf",
            "yum",
            "rpm",
            "apk",
            "portage",
            "zypper",
            "macports",
            "npm",
            "yarn",
            "pnpm",
            "bun",
            "pip",
            "uv",
            "poetry",
            "conda",
            "cargo",
            "composer",
            "rubygems",
            "maven",
            "gradle",
            "vcpkg",
            "conan",
            "go-modules",
            "nix",
            "swift-package-manager",
            "cocoapods",
            "carthage",
            "dart-pub",
            "nuget",
            "hex",
            "cabal",
            "stack",
            "luarocks",
            "cpan",
            "cran",
            "julia-pkg",
            "sbt",
            "mill",
            "leiningen",
            "clojure-cli",
            "opam",
            "zig",
            "helm",
            "docker-hub",
            "ghcr",
            "podman",
            "terraform-registry",
            "snap",
            "appimage",
            "freebsd-ports",
            "bsd-pkg",
            "openbsd-pkg-add",
            "ips",
            "pkgsrc",
            "guix",
            "asdf",
        ] {
            assert!(ids.contains(id), "missing manager {id}");
        }
        assert!(!ids.contains("microsoft-store"));
        for id in ["bun", "uv", "poetry", "cargo", "nix", "pnpm"] {
            let manager = managers.iter().find(|manager| manager.id == id).unwrap();
            assert!(
                !manager.installers.is_empty(),
                "missing installers for {id}"
            );
        }
    }

    #[test]
    fn filters_managers_by_os() {
        let managers = load_managers(Path::new("sources.list")).unwrap();
        let visible = managers_for_os(&managers);
        assert!(
            visible
                .iter()
                .all(|manager| manager_available_on_os(manager, current_os()))
        );
        if current_os() == "windows" {
            assert!(visible.iter().any(|manager| manager.id == "winget"));
            assert!(!visible.iter().any(|manager| manager.id == "macports"));
            assert!(!visible.iter().any(|manager| manager.id == "nix"));
        }
    }

    #[test]
    fn hides_managers_that_do_not_support_the_selected_os() {
        let managers = load_managers(Path::new("sources.list")).unwrap();
        let manager = |id: &str| managers.iter().find(|manager| manager.id == id).unwrap();

        assert!(manager_available_on_os(manager("macports"), "darwin"));
        assert!(!manager_available_on_os(manager("macports"), "windows"));
        assert!(!manager_available_on_os(manager("nix"), "windows"));
        assert!(manager_available_on_os(manager("vcpkg"), "windows"));
        assert!(manager_available_on_os(manager("vcpkg"), "linux"));

        let conda = manager("conda");
        for os in ["windows", "linux", "darwin"] {
            assert!(
                active_installer_for_os(conda, os).is_some(),
                "missing conda installer profile for {os}"
            );
        }
        assert!(
            active_installer_for_os_and_arch(conda, "linux", "aarch64").is_none(),
            "conda must be hidden when no ARM Linux installer is available"
        );
    }

    #[test]
    fn installer_path_is_scoped_to_manager() {
        let manager = Manager {
            id: "example".into(),
            name: "Example".into(),
            os: vec!["any".into()],
            description: "Example".into(),
            homepage: "https://example.org".into(),
            install_page: "https://example.org/install".into(),
            installer_url: "https://example.org/install.sh".into(),
            installer_file: "install.sh".into(),
            installer_type: "bash".into(),
            installers: vec![],
            plugin_script: None,
            installed_check: vec![],
            requires: vec![],
            sha256: String::new(),
            system_managed: false,
        };
        assert!(
            installer_path(&manager)
                .unwrap()
                .ends_with(Path::new("example").join("install.sh"))
        );
    }

    #[test]
    fn tui_loads_only_the_visible_manager_window() {
        let managers = load_managers(Path::new("sources.list")).unwrap();
        let mut tui = Tui::new(managers).unwrap();
        assert!(tui.visible_managers.is_empty());

        tui.load_visible_window(2).unwrap();
        assert_eq!(tui.visible_managers.len(), 2);
        let first_window_start = tui.visible_start;

        tui.selected = tui.manager_sources.len() - 1;
        tui.load_visible_window(2).unwrap();
        assert_eq!(tui.visible_managers.len(), 2);
        assert_ne!(tui.visible_start, first_window_start);
        assert_eq!(
            tui.selected_manager().map(|manager| manager.id.as_str()),
            tui.manager_sources
                .last()
                .and_then(|source| serde_yaml::from_str::<Manager>(source).ok())
                .map(|manager| manager.id)
                .as_deref()
        );
    }

    #[test]
    fn loads_local_plugin_scripts_from_manifest() {
        let unique = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let directory = env::temp_dir().join(format!(
            "app-store-store-plugin-test-{}-{unique}",
            std::process::id()
        ));
        fs::create_dir_all(&directory).unwrap();
        let script = directory.join("install.sh");
        fs::write(&script, "echo install\n").unwrap();

        let manager = Manager {
            id: "test-plugin".into(),
            name: "Test plugin".into(),
            os: vec!["any".into()],
            description: "Test local plugin".into(),
            homepage: "https://example.org".into(),
            install_page: "https://example.org/install".into(),
            installer_url: String::new(),
            installer_file: String::new(),
            installer_type: "bash".into(),
            installers: vec![],
            plugin_script: Some(PathBuf::from("install.sh")),
            installed_check: vec![],
            requires: vec![],
            sha256: String::new(),
            system_managed: false,
        };
        let manifest = serde_yaml::to_string(&manager).unwrap();
        fs::write(directory.join("test.yml"), manifest).unwrap();

        let plugins = load_plugin_managers(&directory).unwrap();
        assert_eq!(plugins.len(), 1);
        assert!(plugins[0].plugin_script.as_ref().unwrap().is_absolute());
        assert!(has_installer(&plugins[0]));

        fs::remove_dir_all(directory).unwrap();
    }

    #[test]
    fn rejects_plugin_scripts_outside_plugin_directory() {
        let unique = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let root = env::temp_dir().join(format!(
            "app-store-store-plugin-path-test-{}-{unique}",
            std::process::id()
        ));
        let directory = root.join("plugins");
        fs::create_dir_all(&directory).unwrap();
        fs::write(root.join("outside.sh"), "echo outside\n").unwrap();
        let manager = Manager {
            id: "unsafe-plugin".into(),
            name: "Unsafe plugin".into(),
            os: vec!["any".into()],
            description: "Path traversal test".into(),
            homepage: "https://example.org".into(),
            install_page: "https://example.org/install".into(),
            installer_url: String::new(),
            installer_file: String::new(),
            installer_type: "bash".into(),
            installers: vec![],
            plugin_script: Some(PathBuf::from("../outside.sh")),
            installed_check: vec![],
            requires: vec![],
            sha256: String::new(),
            system_managed: false,
        };
        fs::write(
            directory.join("unsafe.yml"),
            serde_yaml::to_string(&manager).unwrap(),
        )
        .unwrap();

        assert!(load_plugin_managers(&directory).is_err());
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn selects_installer_profile_for_current_os() {
        let managers = load_managers(Path::new("sources.list")).unwrap();
        for id in ["bun", "uv", "poetry", "cargo", "pnpm"] {
            let manager = managers.iter().find(|manager| manager.id == id).unwrap();
            assert!(
                active_installer(manager).is_some(),
                "no installer for {id} on {}",
                current_os()
            );
        }
        let bun = managers.iter().find(|manager| manager.id == "bun").unwrap();
        assert_eq!(
            active_installer(bun).unwrap().installer_type,
            if current_os() == "windows" {
                "powershell"
            } else {
                "bash"
            }
        );
        assert_eq!(
            active_installer_for_os(bun, "linux")
                .unwrap()
                .installer_type,
            "bash"
        );
        assert_eq!(
            active_installer_for_os(bun, "darwin")
                .unwrap()
                .installer_type,
            "bash"
        );

        let brew = managers
            .iter()
            .find(|manager| manager.id == "brew")
            .unwrap();
        for os in ["linux", "darwin"] {
            assert_eq!(
                active_installer_for_os(brew, os).unwrap().installer_type,
                "bash"
            );
        }
        assert!(active_installer_for_os(brew, "windows").is_none());

        let pnpm = managers
            .iter()
            .find(|manager| manager.id == "pnpm")
            .unwrap();
        assert_eq!(
            active_installer_for_os(pnpm, "freebsd")
                .unwrap()
                .installer_type,
            "bash"
        );
        let nix = managers.iter().find(|manager| manager.id == "nix").unwrap();
        assert!(active_installer_for_os(nix, "windows").is_none());
    }

    #[test]
    fn portable_language_installers_cover_linux_and_macos() {
        let managers = load_managers(Path::new("sources.list")).unwrap();
        for id in ["bun", "uv", "poetry", "cargo", "pnpm", "nix"] {
            let manager = managers.iter().find(|manager| manager.id == id).unwrap();
            for os in ["linux", "darwin"] {
                assert!(
                    active_installer_for_os(manager, os).is_some(),
                    "missing {os} installer profile for {id}"
                );
            }
        }
    }

    #[test]
    fn additional_official_installers_match_supported_platforms() {
        let managers = load_managers(Path::new("sources.list")).unwrap();
        for (id, systems) in [
            ("brew", &["linux", "darwin"][..]),
            ("stack", &["linux", "darwin"][..]),
            ("clojure-cli", &["linux"][..]),
            ("opam", &["linux", "darwin", "windows"][..]),
            ("helm", &["linux", "darwin"][..]),
            ("guix", &["linux"][..]),
        ] {
            let manager = managers.iter().find(|manager| manager.id == id).unwrap();
            for os in systems {
                assert!(
                    active_installer_for_os(manager, os).is_some(),
                    "missing {os} installer profile for {id}"
                );
            }
        }
    }
}
