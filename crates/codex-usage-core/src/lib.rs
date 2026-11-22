//! Platform-independent access to the Codex App Server usage endpoints.
//!
//! This crate owns the `codex app-server` child process and exposes snapshots
//! that desktop-specific tray applications can render however they prefer.

use std::{
    collections::BTreeMap,
    ffi::OsString,
    fs,
    io::{self, BufRead, BufReader, BufWriter, Write},
    path::{Path, PathBuf},
    process::{Child, ChildStdin, Command, Stdio},
    sync::{
        Arc,
        atomic::{AtomicBool, AtomicU64, Ordering},
        mpsc::{self, Receiver, RecvTimeoutError, Sender},
    },
    thread::{self, JoinHandle},
    time::{Duration, SystemTime, UNIX_EPOCH},
};

#[cfg(target_os = "linux")]
use std::{cmp::Ordering as CmpOrdering, ffi::OsStr};

use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use thiserror::Error;

pub const DEFAULT_POLL_INTERVAL: Duration = Duration::from_secs(60);
const DEFAULT_RETRY_INTERVAL: Duration = Duration::from_secs(15);
const DEFAULT_REQUEST_TIMEOUT: Duration = Duration::from_secs(15);

#[derive(Debug, Clone, PartialEq)]
pub struct UsageWindow {
    pub used_percent: f64,
    pub window_duration: Duration,
    pub resets_at: SystemTime,
}

#[derive(Debug, Clone, PartialEq)]
pub struct UsageBucket {
    pub id: String,
    pub label: Option<String>,
    pub plan_type: Option<String>,
    pub primary: UsageWindow,
    pub secondary: Option<UsageWindow>,
}

#[derive(Debug, Clone, PartialEq, Default)]
pub struct UsageSnapshot {
    pub buckets: Vec<UsageBucket>,
}

impl UsageSnapshot {
    pub fn icon_usage_rows(&self) -> (String, String) {
        let Some(bucket) = self.buckets.first() else {
            return ("—".to_owned(), "—".to_owned());
        };

        (
            rounded_percent(bucket.primary.used_percent),
            bucket
                .secondary
                .as_ref()
                .map(|window| rounded_percent(window.used_percent))
                .unwrap_or_else(|| "—".to_owned()),
        )
    }

    pub fn compact_usage(&self) -> String {
        let Some(bucket) = self.buckets.first() else {
            return "—/—".to_owned();
        };

        let compact = format!(
            "{}/{}",
            rounded_percent(bucket.primary.used_percent),
            bucket
                .secondary
                .as_ref()
                .map(|window| rounded_percent(window.used_percent))
                .unwrap_or_else(|| "—".to_owned())
        );

        if compact.chars().count() <= 5 {
            compact
        } else {
            rounded_percent(bucket.primary.used_percent)
        }
    }

    pub fn summary(&self) -> String {
        let Some(bucket) = self.buckets.first() else {
            return "Uso 5 horas: — / Semanal: —".to_owned();
        };

        let weekly = bucket
            .secondary
            .as_ref()
            .map(|window| format!("{}%", rounded_percent(window.used_percent)))
            .unwrap_or_else(|| "—".to_owned());

        format!(
            "Uso 5 horas: {}% / Semanal: {weekly}",
            rounded_percent(bucket.primary.used_percent)
        )
    }

    pub fn menu_status(&self, now: SystemTime) -> String {
        let Some(bucket) = self.buckets.first() else {
            return "—/—".to_owned();
        };
        format_bucket_status(bucket, now)
    }

    pub fn menu_status_lines(&self, now: SystemTime) -> Vec<String> {
        if self.buckets.len() <= 1 {
            return vec![self.menu_status(now)];
        }

        self.buckets
            .iter()
            .map(|bucket| {
                format!(
                    "{}: {}",
                    bucket.display_name(),
                    format_bucket_status(bucket, now)
                )
            })
            .collect()
    }

    pub fn tooltip(&self, _now: SystemTime) -> String {
        self.summary()
    }
}

fn format_bucket_status(bucket: &UsageBucket, now: SystemTime) -> String {
    let format_window = |window: &UsageWindow| {
        let minutes = window
            .resets_at
            .duration_since(now)
            .unwrap_or_default()
            .as_secs()
            / 60;
        let days = minutes / 1440;
        let hours = minutes % 1440 / 60;
        let minutes = minutes % 60;
        let mut remaining = String::new();
        if days > 0 {
            remaining.push_str(&format!("{days}d"));
        }
        if hours > 0 {
            remaining.push_str(&format!("{hours}h"));
        }
        if minutes > 0 || remaining.is_empty() {
            remaining.push_str(&format!("{minutes}m"));
        }
        format!("{remaining}: {}%", rounded_percent(window.used_percent))
    };
    format!(
        "{}/{}",
        format_window(&bucket.primary),
        bucket
            .secondary
            .as_ref()
            .map(format_window)
            .unwrap_or_else(|| "—".to_owned())
    )
}

fn rounded_percent(value: f64) -> String {
    format!("{:.0}", value.clamp(0.0, 100.0))
}

impl UsageBucket {
    pub fn display_name(&self) -> &str {
        self.label.as_deref().unwrap_or(&self.id)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum BackendProvider {
    #[default]
    Codex,
    Claude,
    CommandCode,
    Antigravity,
}

impl BackendProvider {
    pub const ALL: [Self; 4] = [
        Self::Codex,
        Self::Claude,
        Self::CommandCode,
        Self::Antigravity,
    ];

    pub fn id(self) -> &'static str {
        match self {
            Self::Codex => "codex",
            Self::Claude => "claude",
            Self::CommandCode => "command-code",
            Self::Antigravity => "antigravity",
        }
    }

    pub fn display_name(self) -> &'static str {
        match self {
            Self::Codex => "Codex",
            Self::Claude => "Claude",
            Self::CommandCode => "Command Code",
            Self::Antigravity => "Antigravity",
        }
    }

    pub fn from_id(value: &str) -> Option<Self> {
        match value.trim().to_ascii_lowercase().as_str() {
            "codex" => Some(Self::Codex),
            "claude" => Some(Self::Claude),
            "command-code" | "command_code" | "commandcode" => Some(Self::CommandCode),
            "antigravity" => Some(Self::Antigravity),
            _ => None,
        }
    }

    pub fn default_cli_package(self) -> Option<&'static str> {
        None
    }
}

pub fn install_provider_cli(provider: BackendProvider) -> Result<(), CoreError> {
    let Some(package) = provider.default_cli_package() else {
        return Err(CoreError::ProviderCommandFailed { provider });
    };

    let npm_binary = {
        #[cfg(windows)]
        {
            OsString::from("npm.cmd")
        }
        #[cfg(not(windows))]
        {
            resolve_node_cli_binary("npm")
        }
    };

    let mut command = Command::new(&npm_binary);
    command
        .args(["install", "-g", package])
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null());

    #[cfg(unix)]
    if let Some(node_directory) = Path::new(&npm_binary)
        .parent()
        .filter(|directory| directory.join("node").is_file())
    {
        let mut paths = vec![node_directory.to_path_buf()];
        paths.extend(
            std::env::var_os("PATH")
                .into_iter()
                .flat_map(|path| std::env::split_paths(&path).collect::<Vec<_>>()),
        );
        if let Ok(path) = std::env::join_paths(paths) {
            command.env("PATH", path);
        }
    }

    #[cfg(all(target_os = "windows", not(debug_assertions)))]
    {
        use std::os::windows::process::CommandExt;

        const CREATE_NO_WINDOW: u32 = 0x0800_0000;
        command.creation_flags(CREATE_NO_WINDOW);
    }

    let status = command.status().map_err(|error| {
        log_error(&format!(
            "failed to launch npm for `{package}` (provider={}): {error}",
            provider.id()
        ));
        if error.kind() == io::ErrorKind::NotFound {
            CoreError::ProviderCliNotFound {
                provider,
                binary: "npm",
            }
        } else {
            CoreError::ProviderCommandFailed { provider }
        }
    })?;

    if status.success() {
        log_info(&format!(
            "installed `{package}` via npm (provider={})",
            provider.id()
        ));
        Ok(())
    } else {
        log_error(&format!(
            "npm install -g {package} failed with status {:?} (provider={})",
            status.code(),
            provider.id()
        ));
        Err(CoreError::ProviderCommandFailed { provider })
    }
}

#[derive(Debug, Serialize, Deserialize)]
struct PersistedConfig {
    provider: BackendProvider,
}

pub fn default_config_path() -> Option<PathBuf> {
    if let Some(configured) = std::env::var_os("CODEX_TRAY_CONFIG_PATH")
        && !configured.is_empty()
    {
        return Some(PathBuf::from(configured));
    }

    #[cfg(unix)]
    {
        if let Some(xdg) = std::env::var_os("XDG_CONFIG_HOME")
            && !xdg.is_empty()
        {
            return Some(PathBuf::from(xdg).join("codex-tray").join("config.json"));
        }
        if let Some(home) = std::env::var_os("HOME")
            && !home.is_empty()
        {
            return Some(
                PathBuf::from(home)
                    .join(".config")
                    .join("codex-tray")
                    .join("config.json"),
            );
        }
    }

    #[cfg(windows)]
    {
        if let Some(appdata) = std::env::var_os("APPDATA")
            && !appdata.is_empty()
        {
            return Some(
                PathBuf::from(appdata)
                    .join("codex-tray")
                    .join("config.json"),
            );
        }
    }

    None
}

pub fn default_log_path() -> Option<PathBuf> {
    if let Some(configured) = std::env::var_os("CODEX_TRAY_LOG_PATH")
        && !configured.is_empty()
    {
        return Some(PathBuf::from(configured));
    }

    default_config_path()
        .and_then(|path| path.parent().map(Path::to_path_buf))
        .map(|dir| dir.join("codex-tray.log"))
}

const MAX_LOG_BYTES: u64 = 256 * 1024;

pub fn log_to_path(path: &Path, level: &str, message: &str) {
    if let Some(parent) = path.parent()
        && !parent.as_os_str().is_empty()
    {
        let _ = fs::create_dir_all(parent);
    }

    if let Ok(metadata) = fs::metadata(path)
        && metadata.len() >= MAX_LOG_BYTES
    {
        let rotated = path.with_extension("log.old");
        let _ = fs::rename(path, rotated);
    }

    let ts = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0);
    let clean = message.replace(['\r', '\n'], " ");
    if let Ok(mut file) = fs::OpenOptions::new().create(true).append(true).open(path) {
        let _ = writeln!(file, "[{ts}] [{level}] {clean}");
    }
}

pub fn log_info(message: &str) {
    if std::env::var("CODEX_TRAY_LOG").as_deref() == Ok("0") {
        return;
    }
    if let Some(path) = default_log_path() {
        log_to_path(&path, "INFO", message);
    }
}

pub fn log_error(message: &str) {
    if std::env::var("CODEX_TRAY_LOG").as_deref() == Ok("0") {
        return;
    }
    if let Some(path) = default_log_path() {
        log_to_path(&path, "ERROR", message);
    }
}

pub fn load_persisted_provider(path: &Path) -> Option<BackendProvider> {
    let contents = fs::read_to_string(path).ok()?;
    let persisted: PersistedConfig = serde_json::from_str(&contents).ok()?;
    Some(persisted.provider)
}

pub fn save_persisted_provider(path: &Path, provider: BackendProvider) -> io::Result<()> {
    if let Some(parent) = path.parent()
        && !parent.as_os_str().is_empty()
    {
        fs::create_dir_all(parent)?;
    }

    let payload =
        serde_json::to_vec_pretty(&PersistedConfig { provider }).map_err(io::Error::other)?;

    static TMP_COUNTER: AtomicU64 = AtomicU64::new(0);
    let tmp_id = TMP_COUNTER.fetch_add(1, Ordering::Relaxed);
    let file_name = path
        .file_name()
        .and_then(|name| name.to_str())
        .unwrap_or("config.json");
    let tmp_path = path.with_file_name(format!(".{file_name}.tmp.{}.{tmp_id}", std::process::id()));

    let write_result = (|| -> io::Result<()> {
        let mut file = fs::File::create(&tmp_path)?;
        file.write_all(&payload)?;
        file.sync_all()?;
        Ok(())
    })();

    if let Err(error) = write_result {
        let _ = fs::remove_file(&tmp_path);
        return Err(error);
    }

    if let Err(error) = fs::rename(&tmp_path, path) {
        let _ = fs::remove_file(&tmp_path);
        return Err(error);
    }

    Ok(())
}

#[derive(Debug, Clone)]
pub struct MonitorConfig {
    pub provider: BackendProvider,
    pub config_path: Option<PathBuf>,
    pub codex_binary: OsString,
    pub poll_interval: Duration,
    pub retry_interval: Duration,
    pub request_timeout: Duration,
}

impl Default for MonitorConfig {
    fn default() -> Self {
        let config_path = default_config_path();
        let provider = std::env::var("CODEX_TRAY_PROVIDER")
            .ok()
            .and_then(|value| BackendProvider::from_id(&value))
            .or_else(|| config_path.as_deref().and_then(load_persisted_provider))
            .unwrap_or_default();

        Self {
            provider,
            config_path,
            codex_binary: default_codex_binary(),
            poll_interval: interval_from_environment(
                "CODEX_TRAY_INTERVAL_SECS",
                DEFAULT_POLL_INTERVAL,
            ),
            retry_interval: DEFAULT_RETRY_INTERVAL,
            request_timeout: DEFAULT_REQUEST_TIMEOUT,
        }
    }
}

fn default_codex_binary() -> OsString {
    if let Some(configured) = std::env::var_os("CODEX_TRAY_CODEX_BIN") {
        return configured;
    }

    resolve_node_cli_binary("codex")
}

fn resolve_node_cli_binary(binary_name: &str) -> OsString {
    #[cfg(target_os = "linux")]
    if !command_is_on_path(OsStr::new(binary_name)) {
        if let Some(home_bin) = find_home_cli_binary(binary_name) {
            return home_bin.into_os_string();
        }
        if let Some(nvm_bin) = find_nvm_binary(binary_name) {
            return nvm_bin.into_os_string();
        }
    }

    binary_name.into()
}

#[cfg(target_os = "linux")]
fn find_home_cli_binary(binary_name: &str) -> Option<PathBuf> {
    let home = PathBuf::from(std::env::var_os("HOME")?);
    let candidates = [
        home.join(".local/bin").join(binary_name),
        home.join(".gemini/antigravity-cli/bin").join(binary_name),
    ];
    candidates.into_iter().find(|path| path.is_file())
}

#[cfg(target_os = "linux")]
fn command_is_on_path(command: &OsStr) -> bool {
    std::env::var_os("PATH")
        .into_iter()
        .flat_map(|path| std::env::split_paths(&path).collect::<Vec<_>>())
        .any(|directory| directory.join(command).is_file())
}

#[cfg(target_os = "linux")]
fn find_nvm_binary(binary_name: &str) -> Option<PathBuf> {
    let mut nvm_directories = Vec::new();
    if let Some(nvm_dir) = std::env::var_os("NVM_DIR") {
        nvm_directories.push(PathBuf::from(nvm_dir));
    }
    if let Some(home_dir) = std::env::var_os("HOME") {
        let home_nvm_dir = PathBuf::from(home_dir).join(".nvm");
        if !nvm_directories.contains(&home_nvm_dir) {
            nvm_directories.push(home_nvm_dir);
        }
    }

    nvm_directories.iter().find_map(|nvm_dir| {
        let versions_dir = nvm_dir.join("versions/node");
        let entries = std::fs::read_dir(versions_dir).ok()?;
        let mut candidates: Vec<_> = entries
            .filter_map(Result::ok)
            .filter_map(|entry| {
                let version = entry.file_name().into_string().ok()?;
                let bin_dir = entry.path().join("bin");
                let binary = bin_dir.join(binary_name);
                (binary.is_file() && bin_dir.join("node").is_file()).then_some((version, binary))
            })
            .collect();

        candidates.sort_by(|(left, _), (right, _)| compare_nvm_versions(right, left));

        let preferred_version = preferred_nvm_version(nvm_dir);
        preferred_version
            .as_deref()
            .and_then(|preferred| {
                candidates
                    .iter()
                    .find(|(version, _)| nvm_version_matches_alias(version, preferred))
                    .map(|(_, binary)| binary.clone())
            })
            .or_else(|| candidates.first().map(|(_, binary)| binary.clone()))
    })
}

#[cfg(target_os = "linux")]
fn preferred_nvm_version(nvm_dir: &Path) -> Option<String> {
    let alias = std::fs::read_to_string(nvm_dir.join("alias/default")).ok()?;
    let alias = alias.trim();

    if let Some(lts_name) = alias.strip_prefix("lts/") {
        if lts_name != "*" {
            return std::fs::read_to_string(nvm_dir.join("alias/lts").join(lts_name))
                .ok()
                .map(|version| version.trim().to_owned());
        }

        return std::fs::read_dir(nvm_dir.join("alias/lts"))
            .ok()?
            .filter_map(Result::ok)
            .filter_map(|entry| std::fs::read_to_string(entry.path()).ok())
            .map(|version| version.trim().to_owned())
            .max_by(|left, right| compare_nvm_versions(left, right));
    }

    Some(alias.to_owned())
}

#[cfg(target_os = "linux")]
fn nvm_version_matches_alias(version: &str, alias: &str) -> bool {
    let version = nvm_version_components(version);
    let alias = nvm_version_components(alias);
    !alias.is_empty() && version.starts_with(&alias)
}

#[cfg(target_os = "linux")]
fn compare_nvm_versions(left: &str, right: &str) -> CmpOrdering {
    nvm_version_components(left).cmp(&nvm_version_components(right))
}

#[cfg(target_os = "linux")]
fn nvm_version_components(version: &str) -> Vec<u64> {
    version
        .trim_start_matches('v')
        .split('.')
        .map(|component| {
            component
                .chars()
                .take_while(char::is_ascii_digit)
                .collect::<String>()
                .parse()
                .unwrap_or_default()
        })
        .collect()
}

pub trait UsageBackend: Send {
    fn fetch_usage(&mut self) -> Result<UsageSnapshot, CoreError>;
}

#[derive(Debug, Clone, PartialEq)]
pub enum MonitorUpdate {
    Snapshot {
        provider: BackendProvider,
        snapshot: UsageSnapshot,
    },
    Error {
        provider: BackendProvider,
        message: String,
    },
}

pub struct UsageMonitor {
    commands: Sender<MonitorCommand>,
    stopped: Arc<AtomicBool>,
    worker: Option<JoinHandle<()>>,
}

#[derive(Debug, Clone)]
pub struct UsageMonitorController {
    commands: Sender<MonitorCommand>,
}

impl UsageMonitorController {
    pub fn refresh(&self) {
        let _ = self.commands.send(MonitorCommand::Refresh);
    }

    pub fn set_provider(&self, provider: BackendProvider) {
        let _ = self.commands.send(MonitorCommand::SetProvider(provider));
    }
}

impl UsageMonitor {
    pub fn start<F>(config: MonitorConfig, on_update: F) -> Result<Self, CoreError>
    where
        F: Fn(MonitorUpdate) + Send + Sync + 'static,
    {
        Self::start_with_factory(config, create_default_backend, on_update)
    }

    pub fn start_with_factory<B, F>(
        config: MonitorConfig,
        factory: B,
        on_update: F,
    ) -> Result<Self, CoreError>
    where
        B: FnMut(BackendProvider, &MonitorConfig) -> Result<Box<dyn UsageBackend>, CoreError>
            + Send
            + 'static,
        F: Fn(MonitorUpdate) + Send + Sync + 'static,
    {
        let (commands, command_receiver) = mpsc::channel();
        let stopped = Arc::new(AtomicBool::new(false));
        let worker_stopped = Arc::clone(&stopped);
        let on_update = Arc::new(on_update);
        let worker = thread::Builder::new()
            .name("codex-usage-monitor".to_owned())
            .spawn(move || {
                run_monitor(config, factory, command_receiver, worker_stopped, on_update)
            })?;

        Ok(Self {
            commands,
            stopped,
            worker: Some(worker),
        })
    }

    pub fn refresh(&self) {
        let _ = self.commands.send(MonitorCommand::Refresh);
    }

    pub fn set_provider(&self, provider: BackendProvider) {
        let _ = self.commands.send(MonitorCommand::SetProvider(provider));
    }

    pub fn controller(&self) -> UsageMonitorController {
        UsageMonitorController {
            commands: self.commands.clone(),
        }
    }

    pub fn stop(&mut self) {
        if self.stopped.swap(true, Ordering::SeqCst) {
            return;
        }

        let _ = self.commands.send(MonitorCommand::Stop);
        if let Some(worker) = self.worker.take() {
            let _ = worker.join();
        }
    }
}

impl Drop for UsageMonitor {
    fn drop(&mut self) {
        self.stop();
    }
}

#[derive(Debug)]
enum MonitorCommand {
    Refresh,
    SetProvider(BackendProvider),
    Stop,
}

enum WaitOutcome {
    Continue,
    SetProvider(BackendProvider),
    Stop,
}

fn run_monitor<B>(
    config: MonitorConfig,
    mut factory: B,
    commands: Receiver<MonitorCommand>,
    stopped: Arc<AtomicBool>,
    on_update: Arc<impl Fn(MonitorUpdate) + Send + Sync + 'static>,
) where
    B: FnMut(BackendProvider, &MonitorConfig) -> Result<Box<dyn UsageBackend>, CoreError>,
{
    let mut active_provider = config.provider;
    let mut backend: Option<Box<dyn UsageBackend>> = None;
    log_info(&format!(
        "monitor started (provider={}, poll={}s)",
        active_provider.id(),
        config.poll_interval.as_secs()
    ));

    while !stopped.load(Ordering::SeqCst) {
        if backend.is_none() {
            match factory(active_provider, &config) {
                Ok(new_backend) => backend = Some(new_backend),
                Err(error) => {
                    log_error(&format!(
                        "backend init failed (provider={}): {error}",
                        active_provider.id()
                    ));
                    on_update(MonitorUpdate::Error {
                        provider: active_provider,
                        message: error.user_message_for_provider(active_provider),
                    });
                    match wait_for_command(&commands, config.retry_interval) {
                        WaitOutcome::Stop => break,
                        WaitOutcome::Continue => continue,
                        WaitOutcome::SetProvider(next) => {
                            apply_provider_switch(
                                &config,
                                &mut active_provider,
                                &mut backend,
                                next,
                            );
                            continue;
                        }
                    }
                }
            }
        }

        let delay = match backend
            .as_mut()
            .expect("backend was initialized")
            .fetch_usage()
        {
            Ok(snapshot) => {
                log_info(&format!(
                    "usage updated (provider={}): {}",
                    active_provider.id(),
                    snapshot.summary()
                ));
                on_update(MonitorUpdate::Snapshot {
                    provider: active_provider,
                    snapshot,
                });
                config.poll_interval
            }
            Err(error) => {
                log_error(&format!(
                    "usage fetch failed (provider={}): {error}",
                    active_provider.id()
                ));
                on_update(MonitorUpdate::Error {
                    provider: active_provider,
                    message: error.user_message_for_provider(active_provider),
                });
                backend = None;
                config.retry_interval
            }
        };

        match wait_for_command(&commands, delay) {
            WaitOutcome::Stop => break,
            WaitOutcome::Continue => {}
            WaitOutcome::SetProvider(next) => {
                apply_provider_switch(&config, &mut active_provider, &mut backend, next);
            }
        }
    }
}

fn apply_provider_switch(
    config: &MonitorConfig,
    active_provider: &mut BackendProvider,
    backend: &mut Option<Box<dyn UsageBackend>>,
    next: BackendProvider,
) {
    if let Some(path) = &config.config_path {
        let _ = save_persisted_provider(path, next);
    }
    if next != *active_provider {
        log_info(&format!(
            "switching provider: {} -> {}",
            active_provider.id(),
            next.id()
        ));
        *backend = None;
        *active_provider = next;
    } else {
        log_info(&format!("refreshing provider: {}", next.id()));
    }
}

/// Waits for a monitor command or timeout.
fn wait_for_command(commands: &Receiver<MonitorCommand>, timeout: Duration) -> WaitOutcome {
    match commands.recv_timeout(timeout) {
        Ok(MonitorCommand::Stop) | Err(RecvTimeoutError::Disconnected) => WaitOutcome::Stop,
        Ok(MonitorCommand::SetProvider(provider)) => WaitOutcome::SetProvider(provider),
        Ok(MonitorCommand::Refresh) | Err(RecvTimeoutError::Timeout) => WaitOutcome::Continue,
    }
}

fn create_default_backend(
    provider: BackendProvider,
    config: &MonitorConfig,
) -> Result<Box<dyn UsageBackend>, CoreError> {
    match provider {
        BackendProvider::Codex => Ok(Box::new(CodexAppServer::spawn(config)?)),
        BackendProvider::Claude => Ok(Box::new(JsonProviderBackend::new(
            BackendProvider::Claude,
            "CODEX_TRAY_CLAUDE_CMD",
            "CODEX_TRAY_CLAUDE_USAGE_FILE",
            "claude-usage.json",
            None,
            config.config_path.as_deref(),
        ))),
        BackendProvider::CommandCode => Ok(Box::new(JsonProviderBackend::new(
            BackendProvider::CommandCode,
            "CODEX_TRAY_COMMAND_CODE_CMD",
            "CODEX_TRAY_COMMAND_CODE_USAGE_FILE",
            "command-code-usage.json",
            None,
            config.config_path.as_deref(),
        ))),
        BackendProvider::Antigravity => Ok(Box::new(JsonProviderBackend::new(
            BackendProvider::Antigravity,
            "CODEX_TRAY_ANTIGRAVITY_CMD",
            "CODEX_TRAY_ANTIGRAVITY_USAGE_FILE",
            "antigravity-usage.json",
            Some(("agy", &["-p", "/usage", "--output-format", "json"])),
            config.config_path.as_deref(),
        ))),
    }
}

struct CodexAppServer {
    child: Child,
    input: BufWriter<ChildStdin>,
    messages: Receiver<Value>,
    next_request_id: u64,
    request_timeout: Duration,
}

impl CodexAppServer {
    fn spawn(config: &MonitorConfig) -> Result<Self, CoreError> {
        let mut command = Command::new(&config.codex_binary);
        command
            .arg("app-server")
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::null());

        #[cfg(unix)]
        if let Some(node_directory) = Path::new(&config.codex_binary)
            .parent()
            .filter(|directory| directory.join("node").is_file())
        {
            let mut paths = vec![node_directory.to_path_buf()];
            paths.extend(
                std::env::var_os("PATH")
                    .into_iter()
                    .flat_map(|path| std::env::split_paths(&path).collect::<Vec<_>>()),
            );
            if let Ok(path) = std::env::join_paths(paths) {
                command.env("PATH", path);
            }
        }

        #[cfg(all(target_os = "windows", not(debug_assertions)))]
        {
            use std::os::windows::process::CommandExt;

            const CREATE_NO_WINDOW: u32 = 0x0800_0000;
            command.creation_flags(CREATE_NO_WINDOW);
        }

        let mut child = command.spawn().map_err(|error| CoreError::StartAppServer {
            command: config.codex_binary.to_string_lossy().into_owned(),
            source: error,
        })?;

        let stdin = child.stdin.take().ok_or(CoreError::MissingStdin)?;
        let stdout = child.stdout.take().ok_or(CoreError::MissingStdout)?;
        let (message_sender, messages) = mpsc::channel();
        thread::Builder::new()
            .name("codex-app-server-reader".to_owned())
            .spawn(move || read_messages(stdout, message_sender))?;

        let mut server = Self {
            child,
            input: BufWriter::new(stdin),
            messages,
            next_request_id: 1,
            request_timeout: config.request_timeout,
        };

        server.request(
            "initialize",
            json!({
                "clientInfo": {
                    "name": "codex_tray",
                    "title": "Codex Tray",
                    "version": env!("CARGO_PKG_VERSION"),
                },
            }),
        )?;
        server.notify("initialized", json!({}))?;
        Ok(server)
    }

    fn read_rate_limits(&mut self) -> Result<UsageSnapshot, CoreError> {
        let result = self.request("account/rateLimits/read", Value::Null)?;
        let response: WireRateLimits = serde_json::from_value(result)?;
        UsageSnapshot::try_from(response)
    }

    fn request(&mut self, method: &str, params: Value) -> Result<Value, CoreError> {
        let id = self.next_request_id;
        self.next_request_id = self
            .next_request_id
            .checked_add(1)
            .ok_or(CoreError::RequestIdOverflow)?;
        self.send(json!({ "method": method, "id": id, "params": params }))?;

        loop {
            let message = self
                .messages
                .recv_timeout(self.request_timeout)
                .map_err(|error| match error {
                    RecvTimeoutError::Timeout => CoreError::TimedOut(method.to_owned()),
                    RecvTimeoutError::Disconnected => CoreError::AppServerClosed,
                })?;

            if message.get("id").and_then(Value::as_u64) != Some(id) {
                continue;
            }

            if let Some(error) = message.get("error") {
                let description = error
                    .get("message")
                    .and_then(Value::as_str)
                    .unwrap_or("unknown app-server error");
                return Err(CoreError::AppServer(description.to_owned()));
            }

            return message
                .get("result")
                .cloned()
                .ok_or_else(|| CoreError::MalformedResponse(method.to_owned()));
        }
    }

    fn notify(&mut self, method: &str, params: Value) -> Result<(), CoreError> {
        self.send(json!({ "method": method, "params": params }))
    }

    fn send(&mut self, message: Value) -> Result<(), CoreError> {
        serde_json::to_writer(&mut self.input, &message)?;
        self.input.write_all(b"\n")?;
        self.input.flush()?;
        Ok(())
    }
}

impl UsageBackend for CodexAppServer {
    fn fetch_usage(&mut self) -> Result<UsageSnapshot, CoreError> {
        self.read_rate_limits()
    }
}

impl Drop for CodexAppServer {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

struct JsonProviderBackend {
    provider: BackendProvider,
    cmd_env: &'static str,
    file_env: &'static str,
    default_file_name: &'static str,
    default_cli: Option<(&'static str, &'static [&'static str])>,
    config_dir: Option<PathBuf>,
}

impl JsonProviderBackend {
    fn new(
        provider: BackendProvider,
        cmd_env: &'static str,
        file_env: &'static str,
        default_file_name: &'static str,
        default_cli: Option<(&'static str, &'static [&'static str])>,
        config_path: Option<&Path>,
    ) -> Self {
        let config_dir = config_path
            .and_then(Path::parent)
            .filter(|dir| !dir.as_os_str().is_empty())
            .map(Path::to_path_buf);

        Self {
            provider,
            cmd_env,
            file_env,
            default_file_name,
            default_cli,
            config_dir,
        }
    }

    fn explicit_usage_file(&self) -> Option<PathBuf> {
        std::env::var_os(self.file_env)
            .filter(|configured| !configured.is_empty())
            .map(PathBuf::from)
    }

    fn default_usage_file(&self) -> Option<PathBuf> {
        self.config_dir
            .as_ref()
            .map(|dir| dir.join(self.default_file_name))
    }

    fn read_usage_file(&self, path: &Path) -> Result<UsageSnapshot, CoreError> {
        let contents = fs::read_to_string(path).map_err(|error| {
            if error.kind() == io::ErrorKind::NotFound {
                CoreError::ProviderUsageFileNotFound {
                    provider: self.provider,
                }
            } else {
                CoreError::Io(error)
            }
        })?;

        parse_provider_usage_json(self.provider, &contents)
    }

    fn run_default_cli(
        &self,
        binary_name: &'static str,
        args: &[&str],
    ) -> Result<UsageSnapshot, CoreError> {
        let resolved_bin = resolve_node_cli_binary(binary_name);
        let mut command = Command::new(&resolved_bin);
        command
            .args(args)
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());

        #[cfg(unix)]
        if let Some(node_directory) = Path::new(&resolved_bin)
            .parent()
            .filter(|directory| directory.join("node").is_file())
        {
            let mut paths = vec![node_directory.to_path_buf()];
            paths.extend(
                std::env::var_os("PATH")
                    .into_iter()
                    .flat_map(|path| std::env::split_paths(&path).collect::<Vec<_>>()),
            );
            if let Ok(path) = std::env::join_paths(paths) {
                command.env("PATH", path);
            }
        }

        #[cfg(all(target_os = "windows", not(debug_assertions)))]
        {
            use std::os::windows::process::CommandExt;

            const CREATE_NO_WINDOW: u32 = 0x0800_0000;
            command.creation_flags(CREATE_NO_WINDOW);
        }

        let output = match command.output() {
            Ok(output) => output,
            Err(error) if error.kind() == io::ErrorKind::NotFound => {
                if let Some(path) = self.default_usage_file()
                    && path.is_file()
                {
                    return self.read_usage_file(&path);
                }
                return Err(CoreError::ProviderCliNotFound {
                    provider: self.provider,
                    binary: binary_name,
                });
            }
            Err(_) => {
                return Err(CoreError::ProviderCommandFailed {
                    provider: self.provider,
                });
            }
        };

        if !output.status.success() {
            let exit_code = output
                .status
                .code()
                .map(|code| code.to_string())
                .unwrap_or_else(|| "signal".to_owned());
            log_error(&format!(
                "default CLI `{}` exited with status {exit_code} (provider={})",
                resolved_bin.to_string_lossy(),
                self.provider.id()
            ));
            let combined_err = format!(
                "{}\n{}",
                String::from_utf8_lossy(&output.stdout),
                String::from_utf8_lossy(&output.stderr)
            )
            .to_ascii_lowercase();
            if combined_err.contains("login")
                || combined_err.contains("not logged in")
                || combined_err.contains("no auth")
                || combined_err.contains("not running")
                || combined_err.contains("antigravity")
            {
                return Err(CoreError::ProviderNotAuthenticated {
                    provider: self.provider,
                });
            }
            return Err(CoreError::ProviderCommandFailed {
                provider: self.provider,
            });
        }

        let stdout = String::from_utf8(output.stdout).map_err(|_| {
            CoreError::InvalidRateLimit("provider command output was not valid UTF-8".into())
        })?;
        parse_provider_usage_json(self.provider, &stdout)
    }
}

impl UsageBackend for JsonProviderBackend {
    fn fetch_usage(&mut self) -> Result<UsageSnapshot, CoreError> {
        if let Ok(cmd) = std::env::var(self.cmd_env)
            && !cmd.trim().is_empty()
        {
            let mut command = {
                #[cfg(windows)]
                {
                    let mut c = Command::new("cmd");
                    c.args(["/C", &cmd]);
                    #[cfg(not(debug_assertions))]
                    {
                        use std::os::windows::process::CommandExt;
                        const CREATE_NO_WINDOW: u32 = 0x0800_0000;
                        c.creation_flags(CREATE_NO_WINDOW);
                    }
                    c
                }
                #[cfg(not(windows))]
                {
                    let mut c = Command::new("sh");
                    c.args(["-c", &cmd]);
                    c
                }
            };

            let output = command
                .stdin(Stdio::null())
                .stdout(Stdio::piped())
                .stderr(Stdio::null())
                .output()
                .map_err(|_| CoreError::ProviderCommandFailed {
                    provider: self.provider,
                })?;

            if !output.status.success() {
                return Err(CoreError::ProviderCommandFailed {
                    provider: self.provider,
                });
            }

            let stdout = String::from_utf8(output.stdout).map_err(|_| {
                CoreError::InvalidRateLimit("provider command output was not valid UTF-8".into())
            })?;
            return parse_provider_usage_json(self.provider, &stdout);
        }

        if let Some(path) = self.explicit_usage_file() {
            return self.read_usage_file(&path);
        }

        if let Some((binary_name, args)) = self.default_cli {
            return self.run_default_cli(binary_name, args);
        }

        let Some(path) = self.default_usage_file() else {
            return Err(CoreError::ProviderUsageFileNotFound {
                provider: self.provider,
            });
        };

        self.read_usage_file(&path)
    }
}

pub fn parse_provider_usage_json(
    provider: BackendProvider,
    json: &str,
) -> Result<UsageSnapshot, CoreError> {
    let value: Value = serde_json::from_str(json)?;
    let Some(object) = value.as_object() else {
        return Err(CoreError::InvalidRateLimit(
            "expected a JSON object".to_owned(),
        ));
    };

    if object.contains_key("rateLimits") || object.contains_key("rateLimitsByLimitId") {
        let wire: WireRateLimits = serde_json::from_value(value)?;
        return UsageSnapshot::try_from(wire);
    }

    if let Some(rate_limits) = object.get("rate_limits") {
        let parsed: ClaudeRateLimits = serde_json::from_value(rate_limits.clone())?;
        let primary_raw = parsed.five_hour.ok_or_else(|| {
            CoreError::InvalidRateLimit("missing rate_limits.five_hour window".to_owned())
        })?;
        let primary = parse_flexible_window(primary_raw, 300)?;
        let secondary = parsed
            .seven_day
            .map(|w| parse_flexible_window(w, 10_080))
            .transpose()?;

        return Ok(UsageSnapshot {
            buckets: vec![UsageBucket {
                id: provider.id().to_owned(),
                label: None,
                plan_type: None,
                primary,
                secondary,
            }],
        });
    }

    if object.contains_key("primary") {
        let parsed: GenericUsagePayload = serde_json::from_value(value)?;
        let primary = parse_flexible_window(parsed.primary, 300)?;
        let secondary = parsed
            .secondary
            .map(|w| parse_flexible_window(w, 10_080))
            .transpose()?;

        return Ok(UsageSnapshot {
            buckets: vec![UsageBucket {
                id: parsed.limit_id.unwrap_or_else(|| provider.id().to_owned()),
                label: parsed.label,
                plan_type: parsed.plan_type,
                primary,
                secondary,
            }],
        });
    }

    if let Some(snapshot) = try_parse_antigravity_usage_json(provider, &value)? {
        return Ok(snapshot);
    }

    Err(CoreError::InvalidRateLimit(
        "unrecognized usage JSON schema".to_owned(),
    ))
}

#[derive(Debug, Deserialize)]
struct AntigravityQuotaEntry {
    #[serde(default)]
    group: Option<String>,
    #[serde(default)]
    window: Option<String>,
    #[serde(default, alias = "windowDurationMins")]
    window_duration_mins: Option<u64>,
    #[serde(default, alias = "used_percentage", alias = "usedPercent")]
    used_percent: Option<f64>,
    #[serde(default, alias = "remainingPercentage", alias = "remaining_percentage")]
    remaining_percentage: Option<f64>,
    #[serde(default, alias = "remainingFraction", alias = "remaining_fraction")]
    remaining_fraction: Option<f64>,
    #[serde(
        default,
        alias = "resetsAt",
        alias = "resetAt",
        alias = "resetTime",
        alias = "reset_time"
    )]
    resets_at: Option<Value>,
    #[serde(default, alias = "timeUntilResetMs")]
    time_until_reset_ms: Option<u64>,
    #[serde(default, alias = "isExhausted")]
    is_exhausted: Option<bool>,
    #[serde(default, alias = "isAutocompleteOnly")]
    is_autocomplete_only: Option<bool>,
    #[serde(default)]
    disabled: bool,
}

#[derive(Debug, Deserialize)]
struct AntigravityGroup {
    #[serde(default, alias = "groupId")]
    id: Option<String>,
    #[serde(default, alias = "displayName", alias = "name")]
    label: Option<String>,
    #[serde(default, alias = "quotas", alias = "models")]
    buckets: Vec<AntigravityQuotaEntry>,
}

#[derive(Debug, Deserialize)]
struct AntigravityAccount {
    #[serde(default)]
    active: bool,
    #[serde(default, alias = "models", alias = "buckets")]
    quotas: Vec<AntigravityQuotaEntry>,
    #[serde(default)]
    groups: Vec<AntigravityGroup>,
}

fn try_parse_antigravity_usage_json(
    provider: BackendProvider,
    value: &Value,
) -> Result<Option<UsageSnapshot>, CoreError> {
    if let Some(groups) = value
        .get("groups")
        .or_else(|| value.get("response").and_then(|r| r.get("groups")))
        .or_else(|| {
            value
                .get("command")
                .and_then(|c| c.get("data"))
                .and_then(|d| d.get("groups"))
        })
    {
        let parsed_groups: Vec<AntigravityGroup> = serde_json::from_value(groups.clone())?;
        return parse_antigravity_groups(provider, parsed_groups).map(Some);
    }

    if let Some(accounts) = value.get("accounts") {
        let parsed_accounts: Vec<AntigravityAccount> = serde_json::from_value(accounts.clone())?;
        let selected = parsed_accounts
            .iter()
            .find(|account| {
                account.active && (!account.quotas.is_empty() || !account.groups.is_empty())
            })
            .or_else(|| {
                parsed_accounts
                    .iter()
                    .find(|account| !account.quotas.is_empty() || !account.groups.is_empty())
            })
            .ok_or_else(|| {
                CoreError::InvalidRateLimit("antigravity accounts payload had no quotas".to_owned())
            })?;

        if !selected.groups.is_empty() {
            let groups = serde_json::from_value::<Vec<AntigravityGroup>>(
                value["accounts"]
                    .as_array()
                    .and_then(|arr| {
                        arr.iter()
                            .find(|a| a.get("active").and_then(Value::as_bool) == Some(true))
                            .or_else(|| arr.first())
                    })
                    .and_then(|a| a.get("groups"))
                    .cloned()
                    .unwrap_or(Value::Array(Vec::new())),
            )?;
            return parse_antigravity_groups(provider, groups).map(Some);
        }

        let quotas = value["accounts"]
            .as_array()
            .and_then(|arr| {
                arr.iter()
                    .find(|a| a.get("active").and_then(Value::as_bool) == Some(true))
                    .or_else(|| arr.first())
            })
            .and_then(|a| {
                a.get("quotas")
                    .or_else(|| a.get("models"))
                    .or_else(|| a.get("buckets"))
            })
            .cloned()
            .unwrap_or(Value::Array(Vec::new()));
        let entries: Vec<AntigravityQuotaEntry> = serde_json::from_value(quotas)?;
        return parse_antigravity_flat_entries(provider, entries).map(Some);
    }

    if let Some(entries_val) = value.get("quotas").or_else(|| value.get("models")) {
        let entries: Vec<AntigravityQuotaEntry> = serde_json::from_value(entries_val.clone())?;
        return parse_antigravity_flat_entries(provider, entries).map(Some);
    }

    Ok(None)
}

fn parse_antigravity_groups(
    provider: BackendProvider,
    groups: Vec<AntigravityGroup>,
) -> Result<UsageSnapshot, CoreError> {
    let mut buckets = Vec::new();
    for (index, group) in groups.into_iter().enumerate() {
        let windows = parse_antigravity_entries(group.buckets)?;
        if windows.is_empty() {
            continue;
        }
        let id = group
            .id
            .or_else(|| group.label.as_ref().map(|label| label.to_ascii_lowercase()))
            .unwrap_or_else(|| format!("{}-{index}", provider.id()));
        buckets.push(build_antigravity_bucket(id, group.label, windows));
    }

    if buckets.is_empty() {
        return Err(CoreError::InvalidRateLimit(
            "antigravity payload contained no active quota buckets".to_owned(),
        ));
    }

    // Prioritize the main Gemini Models group as the primary preview bucket (index 0).
    if let Some(gemini_idx) = buckets.iter().position(|bucket| {
        let name = bucket.display_name().to_ascii_lowercase();
        name.contains("gemini") && !name.contains("argon")
    }) && gemini_idx != 0
    {
        let gemini = buckets.remove(gemini_idx);
        buckets.insert(0, gemini);
    }

    Ok(UsageSnapshot { buckets })
}

fn parse_antigravity_flat_entries(
    provider: BackendProvider,
    entries: Vec<AntigravityQuotaEntry>,
) -> Result<UsageSnapshot, CoreError> {
    let mut grouped: Vec<(String, Option<String>, Vec<AntigravityQuotaEntry>)> = Vec::new();
    for entry in entries {
        if entry.disabled {
            continue;
        }
        let key = entry
            .group
            .clone()
            .unwrap_or_else(|| provider.id().to_owned());
        let label = entry.group.clone();
        if let Some((_, _, list)) = grouped.iter_mut().find(|(k, _, _)| k == &key) {
            list.push(entry);
        } else {
            grouped.push((key, label, vec![entry]));
        }
    }

    let mut buckets = Vec::new();
    for (key, label, group_entries) in grouped {
        let windows = parse_antigravity_entries(group_entries)?;
        if windows.is_empty() {
            continue;
        }
        buckets.push(build_antigravity_bucket(
            key.to_ascii_lowercase(),
            label,
            windows,
        ));
    }

    if buckets.is_empty() {
        return Err(CoreError::InvalidRateLimit(
            "antigravity payload contained no active quotas".to_owned(),
        ));
    }

    Ok(UsageSnapshot { buckets })
}

fn parse_antigravity_entries(
    entries: Vec<AntigravityQuotaEntry>,
) -> Result<Vec<UsageWindow>, CoreError> {
    let now = SystemTime::now();
    let mut windows = Vec::new();
    for entry in entries {
        if entry.disabled || entry.is_autocomplete_only == Some(true) {
            continue;
        }
        let used_percent = extract_antigravity_used_percent(&entry)?;
        let resets_at = match entry.resets_at {
            Some(value) => parse_flexible_timestamp(&value)?,
            None => match entry.time_until_reset_ms {
                Some(ms) => now + Duration::from_millis(ms),
                None => now + Duration::from_secs(300 * 60),
            },
        };
        let duration_mins = entry
            .window_duration_mins
            .or_else(|| entry.window.as_deref().and_then(parse_window_label_mins))
            .unwrap_or_else(|| {
                let remaining_secs = entry
                    .time_until_reset_ms
                    .map(|ms| ms / 1_000)
                    .or_else(|| resets_at.duration_since(now).ok().map(|d| d.as_secs()))
                    .unwrap_or(0);
                if remaining_secs > 24 * 3_600 {
                    10_080
                } else {
                    300
                }
            });
        windows.push(UsageWindow {
            used_percent,
            window_duration: Duration::from_secs(duration_mins.saturating_mul(60)),
            resets_at,
        });
    }
    Ok(windows)
}

fn build_antigravity_bucket(
    id: String,
    label: Option<String>,
    windows: Vec<UsageWindow>,
) -> UsageBucket {
    // Keep the highest-usage (most constrained) window per distinct window duration.
    let mut by_duration: Vec<UsageWindow> = Vec::new();
    for window in windows {
        if let Some(existing) = by_duration
            .iter_mut()
            .find(|w| w.window_duration == window.window_duration)
        {
            if window.used_percent > existing.used_percent {
                *existing = window;
            }
        } else {
            by_duration.push(window);
        }
    }

    by_duration.sort_by_key(|w| w.window_duration);
    let primary = by_duration.remove(0);
    let secondary = by_duration.into_iter().last();
    UsageBucket {
        id,
        label,
        plan_type: None,
        primary,
        secondary,
    }
}

fn extract_antigravity_used_percent(entry: &AntigravityQuotaEntry) -> Result<f64, CoreError> {
    if let Some(used) = entry.used_percent {
        if !(0.0..=100.0).contains(&used) {
            return Err(CoreError::InvalidRateLimit(
                "used_percent is outside 0..=100".to_owned(),
            ));
        }
        return Ok(used);
    }

    if let Some(remaining_pct) = entry.remaining_percentage {
        if !(0.0..=100.0).contains(&remaining_pct) {
            return Err(CoreError::InvalidRateLimit(
                "remainingPercentage is outside 0..=100".to_owned(),
            ));
        }
        let is_antigravity_cli_fraction = remaining_pct <= 1.0
            && (entry.is_exhausted.is_some()
                || entry.time_until_reset_ms.is_some()
                || entry.is_autocomplete_only.is_some());
        if is_antigravity_cli_fraction {
            return Ok(((1.0 - remaining_pct) * 100.0).clamp(0.0, 100.0));
        }
        return Ok((100.0 - remaining_pct).clamp(0.0, 100.0));
    }

    if let Some(remaining_frac) = entry.remaining_fraction {
        if !(0.0..=1.0).contains(&remaining_frac) {
            return Err(CoreError::InvalidRateLimit(
                "remainingFraction is outside 0..=1".to_owned(),
            ));
        }
        return Ok(((1.0 - remaining_frac) * 100.0).clamp(0.0, 100.0));
    }

    if entry.is_exhausted == Some(true) {
        return Ok(100.0);
    }

    Err(CoreError::InvalidRateLimit(
        "antigravity quota entry missing used_percent, remainingPercentage, or remainingFraction"
            .to_owned(),
    ))
}

fn parse_window_label_mins(label: &str) -> Option<u64> {
    let trimmed = label.trim().to_ascii_lowercase();
    match trimmed.as_str() {
        "weekly" | "week" | "seven_day" | "seven-day" => return Some(10_080),
        "daily" | "day" => return Some(1_440),
        "five_hour" | "five-hour" => return Some(300),
        _ => {}
    }
    if let Some(days) = trimmed.strip_suffix('d') {
        return days.parse::<u64>().ok().map(|d| d.saturating_mul(1_440));
    }
    if let Some(hours) = trimmed.strip_suffix('h') {
        return hours.parse::<u64>().ok().map(|h| h.saturating_mul(60));
    }
    if let Some(mins) = trimmed.strip_suffix('m') {
        return mins.parse::<u64>().ok();
    }
    None
}

fn parse_flexible_timestamp(value: &Value) -> Result<SystemTime, CoreError> {
    if let Some(seconds) = value.as_i64() {
        return u64::try_from(seconds)
            .ok()
            .and_then(|secs| UNIX_EPOCH.checked_add(Duration::from_secs(secs)))
            .ok_or_else(|| {
                CoreError::InvalidRateLimit("resets_at is not a valid Unix timestamp".to_owned())
            });
    }

    if let Some(text) = value.as_str() {
        return parse_iso8601_utc(text).ok_or_else(|| {
            CoreError::InvalidRateLimit("reset timestamp is not valid ISO-8601".to_owned())
        });
    }

    Err(CoreError::InvalidRateLimit(
        "reset timestamp must be a Unix integer or ISO-8601 string".to_owned(),
    ))
}

/// Parses an RFC 3339 / ISO-8601 timestamp (e.g. `2026-11-21T22:00:00Z` or `+00:00`) into `SystemTime`.
fn parse_iso8601_utc(input: &str) -> Option<SystemTime> {
    let s = input.trim();
    let (date_part, time_and_tz) = s.split_once('T').or_else(|| s.split_once('t'))?;
    let mut date_iter = date_part.split('-');
    let year: i64 = date_iter.next()?.parse().ok()?;
    let month: u32 = date_iter.next()?.parse().ok()?;
    let day: u32 = date_iter.next()?.parse().ok()?;
    if date_iter.next().is_some() || !(1..=12).contains(&month) || !(1..=31).contains(&day) {
        return None;
    }

    let (time_part, offset_secs): (&str, i64) =
        if let Some(stripped) = time_and_tz.strip_suffix(['Z', 'z']) {
            (stripped, 0)
        } else {
            let idx = time_and_tz.rfind(['+', '-'])?;
            let (t, tz) = time_and_tz.split_at(idx);
            let sign: i64 = if tz.starts_with('+') { 1 } else { -1 };
            let (tz_h, tz_m) = tz[1..].split_once(':')?;
            let h: i64 = tz_h.parse().ok()?;
            let m: i64 = tz_m.parse().ok()?;
            if h > 23 || m > 59 {
                return None;
            }
            (t, sign * (h * 3600 + m * 60))
        };

    let time_whole = time_part.split('.').next()?;
    let mut time_iter = time_whole.split(':');
    let hour: u64 = time_iter.next()?.parse().ok()?;
    let minute: u64 = time_iter.next()?.parse().ok()?;
    let second: u64 = time_iter.next()?.parse().ok()?;
    if time_iter.next().is_some() || hour > 23 || minute > 59 || second > 60 {
        return None;
    }

    let days = days_from_civil(year, month, day)?;
    let epoch_secs = days
        .checked_mul(86_400)?
        .checked_add((hour * 3600 + minute * 60 + second) as i64)?
        .checked_sub(offset_secs)?;
    let secs = u64::try_from(epoch_secs).ok()?;
    UNIX_EPOCH.checked_add(Duration::from_secs(secs))
}

/// Howard Hinnant's algorithm converting a Gregorian calendar date to days since 1970-01-01.
fn days_from_civil(year: i64, month: u32, day: u32) -> Option<i64> {
    let max_day = match month {
        1 | 3 | 5 | 7 | 8 | 10 | 12 => 31,
        4 | 6 | 9 | 11 => 30,
        2 => {
            if (year % 4 == 0 && year % 100 != 0) || year % 400 == 0 {
                29
            } else {
                28
            }
        }
        _ => return None,
    };
    if day == 0 || day > max_day {
        return None;
    }
    let y = year - i64::from(month <= 2);
    let era = y.div_euclid(400);
    let yoe = (y - era * 400) as u64;
    let m = u64::from(month);
    let d = u64::from(day);
    let doy = (153 * (if m > 2 { m - 3 } else { m + 9 }) + 2) / 5 + d - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    Some(era * 146_097 + (doe as i64) - 719_468)
}

#[derive(Debug, Deserialize)]
struct ClaudeRateLimits {
    five_hour: Option<FlexibleWindow>,
    seven_day: Option<FlexibleWindow>,
}

#[derive(Debug, Deserialize)]
struct GenericUsagePayload {
    #[serde(alias = "limitId")]
    limit_id: Option<String>,
    #[serde(alias = "limitName")]
    label: Option<String>,
    #[serde(alias = "planType")]
    plan_type: Option<String>,
    primary: FlexibleWindow,
    secondary: Option<FlexibleWindow>,
}

#[derive(Debug, Deserialize)]
struct FlexibleWindow {
    #[serde(alias = "used_percentage", alias = "usedPercent")]
    used_percent: f64,
    #[serde(default, alias = "windowDurationMins")]
    window_duration_mins: Option<u64>,
    #[serde(alias = "resetsAt")]
    resets_at: i64,
}

fn parse_flexible_window(
    window: FlexibleWindow,
    default_duration_mins: u64,
) -> Result<UsageWindow, CoreError> {
    if !(0.0..=100.0).contains(&window.used_percent) {
        return Err(CoreError::InvalidRateLimit(
            "used_percent is outside 0..=100".to_owned(),
        ));
    }

    let resets_at = u64::try_from(window.resets_at)
        .ok()
        .and_then(|seconds| UNIX_EPOCH.checked_add(Duration::from_secs(seconds)))
        .ok_or_else(|| {
            CoreError::InvalidRateLimit("resets_at is not a valid Unix timestamp".to_owned())
        })?;

    let duration_mins = window.window_duration_mins.unwrap_or(default_duration_mins);

    Ok(UsageWindow {
        used_percent: window.used_percent,
        window_duration: Duration::from_secs(duration_mins.saturating_mul(60)),
        resets_at,
    })
}

fn read_messages(stdout: impl io::Read, sender: Sender<Value>) {
    for line in BufReader::new(stdout).lines() {
        let Ok(line) = line else {
            break;
        };
        let Ok(message) = serde_json::from_str(&line) else {
            continue;
        };
        if sender.send(message).is_err() {
            break;
        }
    }
}

#[derive(Debug, Error)]
pub enum CoreError {
    #[error("could not start `{command} app-server`: {source}")]
    StartAppServer { command: String, source: io::Error },
    #[error("the app-server stdin was not available")]
    MissingStdin,
    #[error("the app-server stdout was not available")]
    MissingStdout,
    #[error("could not communicate with the app-server: {0}")]
    Io(#[from] io::Error),
    #[error("could not decode an app-server response: {0}")]
    Json(#[from] serde_json::Error),
    #[error("the app-server returned an error: {0}")]
    AppServer(String),
    #[error("the app-server did not answer `{0}` in time")]
    TimedOut(String),
    #[error("the app-server process closed its output stream")]
    AppServerClosed,
    #[error("the app-server response for `{0}` did not include a result")]
    MalformedResponse(String),
    #[error("the JSON-RPC request counter overflowed")]
    RequestIdOverflow,
    #[error("invalid rate-limit response: {0}")]
    InvalidRateLimit(String),
    #[error("usage file not found for provider {}", provider.id())]
    ProviderUsageFileNotFound { provider: BackendProvider },
    #[error("usage CLI `{binary}` not found for provider {}", provider.id())]
    ProviderCliNotFound {
        provider: BackendProvider,
        binary: &'static str,
    },
    #[error("provider {} is not authenticated", provider.id())]
    ProviderNotAuthenticated { provider: BackendProvider },
    #[error("usage command failed for provider {}", provider.id())]
    ProviderCommandFailed { provider: BackendProvider },
}

impl CoreError {
    pub fn user_message(&self) -> String {
        self.user_message_for_provider(BackendProvider::Codex)
    }

    fn user_message_for_provider(&self, provider: BackendProvider) -> String {
        let name = provider.display_name();
        match self {
            Self::StartAppServer { source, .. } if source.kind() == io::ErrorKind::NotFound => {
                "O comando `codex` não foi encontrado. Instale a Codex CLI ou configure CODEX_TRAY_CODEX_BIN."
                    .to_owned()
            }
            Self::StartAppServer { .. } => "Não foi possível iniciar `codex app-server`.".to_owned(),
            Self::AppServer(message)
                if message.contains("Not logged in") || message.contains("authentication") =>
            {
                "O Codex não está autenticado. Faça login na CLI ou no app do ChatGPT.".to_owned()
            }
            Self::TimedOut(_) => {
                format!("A consulta a {name} excedeu 15 segundos. Tentaremos novamente em breve.")
            }
            Self::AppServerClosed => {
                "O processo `codex app-server` foi encerrado antes de responder.".to_owned()
            }
            Self::ProviderUsageFileNotFound { provider } => {
                format!(
                    "Arquivo de uso de {} não encontrado. Configure a variável de ambiente do provedor.",
                    provider.display_name()
                )
            }
            Self::ProviderCliNotFound { provider, binary } => match provider {
                BackendProvider::Antigravity => {
                    "O comando `agy` não foi encontrado. Instale a CLI do Antigravity ou configure CODEX_TRAY_ANTIGRAVITY_CMD."
                        .to_owned()
                }
                _ => format!(
                    "O comando `{binary}` não foi encontrado para {}. Instale a CLI ou configure a variável de ambiente do provedor.",
                    provider.display_name()
                ),
            },
            Self::ProviderNotAuthenticated { provider } => match provider {
                BackendProvider::Antigravity => {
                    "Antigravity não está autenticado. Execute `agy` no terminal para fazer login."
                        .to_owned()
                }
                _ => format!(
                    "{} não está autenticado. Faça login na CLI do provedor.",
                    provider.display_name()
                ),
            },
            Self::ProviderCommandFailed { provider } => {
                format!(
                    "Não foi possível executar o comando de uso de {}.",
                    provider.display_name()
                )
            }
            Self::Json(_) | Self::MalformedResponse(_) | Self::InvalidRateLimit(_) => {
                format!("{name} retornou limites em um formato não reconhecido.")
            }
            _ => {
                format!("Não foi possível atualizar o uso de {name}. Tentaremos novamente em breve.")
            }
        }
    }
}

#[derive(Debug, Deserialize)]
struct WireRateLimits {
    #[serde(rename = "rateLimits")]
    rate_limits: Option<WireBucket>,
    #[serde(rename = "rateLimitsByLimitId", default)]
    rate_limits_by_id: BTreeMap<String, WireBucket>,
}

#[derive(Debug, Deserialize)]
struct WireBucket {
    #[serde(rename = "limitId")]
    limit_id: Option<String>,
    #[serde(rename = "limitName")]
    limit_name: Option<String>,
    #[serde(rename = "planType")]
    plan_type: Option<String>,
    primary: Option<WireWindow>,
    secondary: Option<WireWindow>,
}

#[derive(Debug, Deserialize)]
struct WireWindow {
    #[serde(rename = "usedPercent")]
    used_percent: f64,
    #[serde(rename = "windowDurationMins")]
    window_duration_minutes: u64,
    #[serde(rename = "resetsAt")]
    resets_at: i64,
}

impl TryFrom<WireRateLimits> for UsageSnapshot {
    type Error = CoreError;

    fn try_from(response: WireRateLimits) -> Result<Self, Self::Error> {
        let source = if response.rate_limits_by_id.is_empty() {
            response
                .rate_limits
                .map(|bucket| {
                    let id = bucket
                        .limit_id
                        .clone()
                        .unwrap_or_else(|| "codex".to_owned());
                    BTreeMap::from([(id, bucket)])
                })
                .unwrap_or_default()
        } else {
            response.rate_limits_by_id
        };

        let mut buckets = Vec::new();
        for (fallback_id, bucket) in source {
            let Some(primary) = bucket.primary else {
                continue;
            };
            let id = bucket.limit_id.unwrap_or(fallback_id);
            buckets.push(UsageBucket {
                id,
                label: bucket.limit_name,
                plan_type: bucket.plan_type,
                primary: primary.try_into()?,
                secondary: bucket.secondary.map(TryInto::try_into).transpose()?,
            });
        }

        buckets.sort_by(|left, right| {
            let left_is_codex = left.id == "codex";
            let right_is_codex = right.id == "codex";
            right_is_codex
                .cmp(&left_is_codex)
                .then_with(|| left.id.cmp(&right.id))
        });

        if buckets.is_empty() {
            return Err(CoreError::InvalidRateLimit(
                "no metered quota windows were returned".to_owned(),
            ));
        }

        Ok(Self { buckets })
    }
}

impl TryFrom<WireWindow> for UsageWindow {
    type Error = CoreError;

    fn try_from(window: WireWindow) -> Result<Self, Self::Error> {
        if !(0.0..=100.0).contains(&window.used_percent) {
            return Err(CoreError::InvalidRateLimit(
                "usedPercent is outside 0..=100".to_owned(),
            ));
        }
        let resets_at = u64::try_from(window.resets_at)
            .ok()
            .and_then(|seconds| UNIX_EPOCH.checked_add(Duration::from_secs(seconds)))
            .ok_or_else(|| {
                CoreError::InvalidRateLimit("resetsAt is not a valid Unix timestamp".to_owned())
            })?;

        Ok(Self {
            used_percent: window.used_percent,
            window_duration: Duration::from_secs(window.window_duration_minutes.saturating_mul(60)),
            resets_at,
        })
    }
}

fn interval_from_environment(name: &str, fallback: Duration) -> Duration {
    std::env::var(name)
        .ok()
        .and_then(|value| value.parse::<u64>().ok())
        .filter(|seconds| (5..=3_600).contains(seconds))
        .map(Duration::from_secs)
        .unwrap_or(fallback)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_multiple_rate_limit_buckets() {
        let response = serde_json::from_value::<WireRateLimits>(json!({
            "rateLimits": {
                "limitId": "codex",
                "primary": { "usedPercent": 25, "windowDurationMins": 15, "resetsAt": 1730947200 },
            },
            "rateLimitsByLimitId": {
                "codex_other": {
                    "limitId": "codex_other",
                    "limitName": "Janela adicional",
                    "primary": { "usedPercent": 42, "windowDurationMins": 60, "resetsAt": 1730950800 },
                },
                "codex": {
                    "limitId": "codex",
                    "planType": "plus",
                    "primary": { "usedPercent": 25, "windowDurationMins": 15, "resetsAt": 1730947200 },
                },
            },
        }))
        .expect("fixture should deserialize");

        let snapshot = UsageSnapshot::try_from(response).expect("fixture should be valid");

        assert_eq!(snapshot.buckets.len(), 2);
        assert_eq!(snapshot.buckets[0].id, "codex");
        assert_eq!(snapshot.buckets[0].plan_type.as_deref(), Some("plus"));
        assert_eq!(snapshot.buckets[1].display_name(), "Janela adicional");
        assert_eq!(snapshot.summary(), "Uso 5 horas: 25% / Semanal: —");
    }

    #[test]
    fn falls_back_to_the_legacy_single_bucket() {
        let response = serde_json::from_value::<WireRateLimits>(json!({
            "rateLimits": {
                "limitId": "codex",
                "primary": { "usedPercent": 34, "windowDurationMins": 300, "resetsAt": 1730947200 },
                "secondary": null,
            },
        }))
        .expect("fixture should deserialize");

        let snapshot = UsageSnapshot::try_from(response).expect("fixture should be valid");

        assert_eq!(snapshot.buckets.len(), 1);
        assert_eq!(
            snapshot.buckets[0].primary.window_duration,
            Duration::from_secs(18_000)
        );
    }

    #[test]
    fn formats_primary_and_secondary_usage_for_tray_surfaces() {
        let snapshot = UsageSnapshot {
            buckets: vec![UsageBucket {
                id: "codex".into(),
                label: None,
                plan_type: Some("plus".into()),
                primary: UsageWindow {
                    used_percent: 33.4,
                    window_duration: Duration::from_secs(5 * 60 * 60),
                    resets_at: UNIX_EPOCH,
                },
                secondary: Some(UsageWindow {
                    used_percent: 7.6,
                    window_duration: Duration::from_secs(7 * 24 * 60 * 60),
                    resets_at: UNIX_EPOCH,
                }),
            }],
        };

        assert_eq!(snapshot.compact_usage(), "33/8");
        assert_eq!(snapshot.summary(), "Uso 5 horas: 33% / Semanal: 8%");
        assert_eq!(snapshot.tooltip(SystemTime::now()), snapshot.summary());
    }

    #[test]
    fn reports_unavailable_secondary_window_explicitly() {
        let snapshot = UsageSnapshot {
            buckets: vec![UsageBucket {
                id: "codex".into(),
                label: None,
                plan_type: None,
                primary: UsageWindow {
                    used_percent: 3.0,
                    window_duration: Duration::from_secs(5 * 60 * 60),
                    resets_at: UNIX_EPOCH,
                },
                secondary: None,
            }],
        };

        assert_eq!(snapshot.compact_usage(), "3/—");
        assert_eq!(snapshot.summary(), "Uso 5 horas: 3% / Semanal: —");
    }

    #[test]
    fn compact_usage_never_exceeds_five_characters() {
        let snapshot = UsageSnapshot {
            buckets: vec![UsageBucket {
                id: "codex".into(),
                label: None,
                plan_type: None,
                primary: UsageWindow {
                    used_percent: 100.0,
                    window_duration: Duration::from_secs(5 * 60 * 60),
                    resets_at: UNIX_EPOCH,
                },
                secondary: Some(UsageWindow {
                    used_percent: 100.0,
                    window_duration: Duration::from_secs(7 * 24 * 60 * 60),
                    resets_at: UNIX_EPOCH,
                }),
            }],
        };

        assert_eq!(snapshot.compact_usage(), "100");
        assert!(snapshot.compact_usage().chars().count() <= 5);
    }

    #[test]
    fn exposes_usage_as_two_icon_rows() {
        let snapshot = UsageSnapshot {
            buckets: vec![UsageBucket {
                id: "codex".into(),
                label: None,
                plan_type: None,
                primary: UsageWindow {
                    used_percent: 33.0,
                    window_duration: Duration::from_secs(5 * 60 * 60),
                    resets_at: UNIX_EPOCH,
                },
                secondary: Some(UsageWindow {
                    used_percent: 66.0,
                    window_duration: Duration::from_secs(7 * 24 * 60 * 60),
                    resets_at: UNIX_EPOCH,
                }),
            }],
        };

        assert_eq!(snapshot.icon_usage_rows(), ("33".into(), "66".into()));
    }
}
