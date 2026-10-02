//! Platform-independent access to the Codex App Server usage endpoints.
//!
//! This crate owns the `codex app-server` child process and exposes snapshots
//! that desktop-specific tray applications can render however they prefer.

use std::{
    collections::BTreeMap,
    ffi::OsString,
    io::{self, BufRead, BufReader, BufWriter, Write},
    process::{Child, ChildStdin, Command, Stdio},
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
        mpsc::{self, Receiver, RecvTimeoutError, Sender},
    },
    thread::{self, JoinHandle},
    time::{Duration, SystemTime, UNIX_EPOCH},
};

#[cfg(unix)]
use std::path::Path;
#[cfg(target_os = "linux")]
use std::{cmp::Ordering as CmpOrdering, ffi::OsStr, path::PathBuf};

use serde::Deserialize;
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

    pub fn tooltip(&self, _now: SystemTime) -> String {
        self.summary()
    }
}

fn rounded_percent(value: f64) -> String {
    format!("{:.0}", value.clamp(0.0, 100.0))
}

impl UsageBucket {
    pub fn display_name(&self) -> &str {
        self.label.as_deref().unwrap_or(&self.id)
    }
}

#[derive(Debug, Clone)]
pub struct MonitorConfig {
    pub codex_binary: OsString,
    pub poll_interval: Duration,
    pub retry_interval: Duration,
    pub request_timeout: Duration,
}

impl Default for MonitorConfig {
    fn default() -> Self {
        Self {
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

    #[cfg(target_os = "linux")]
    if !command_is_on_path(OsStr::new("codex"))
        && let Some(nvm_codex) = find_nvm_codex()
    {
        return nvm_codex.into_os_string();
    }

    "codex".into()
}

#[cfg(target_os = "linux")]
fn command_is_on_path(command: &OsStr) -> bool {
    std::env::var_os("PATH")
        .into_iter()
        .flat_map(|path| std::env::split_paths(&path).collect::<Vec<_>>())
        .any(|directory| directory.join(command).is_file())
}

#[cfg(target_os = "linux")]
fn find_nvm_codex() -> Option<PathBuf> {
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
                let codex = bin_dir.join("codex");
                (codex.is_file() && bin_dir.join("node").is_file()).then_some((version, codex))
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
                    .map(|(_, codex)| codex.clone())
            })
            .or_else(|| candidates.first().map(|(_, codex)| codex.clone()))
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

#[derive(Debug, Clone)]
pub enum MonitorUpdate {
    Snapshot(UsageSnapshot),
    Error(String),
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
}

impl UsageMonitor {
    pub fn start<F>(config: MonitorConfig, on_update: F) -> Result<Self, CoreError>
    where
        F: Fn(MonitorUpdate) + Send + Sync + 'static,
    {
        let (commands, command_receiver) = mpsc::channel();
        let stopped = Arc::new(AtomicBool::new(false));
        let worker_stopped = Arc::clone(&stopped);
        let on_update = Arc::new(on_update);
        let worker = thread::Builder::new()
            .name("codex-usage-monitor".to_owned())
            .spawn(move || run_monitor(config, command_receiver, worker_stopped, on_update))?;

        Ok(Self {
            commands,
            stopped,
            worker: Some(worker),
        })
    }

    pub fn refresh(&self) {
        let _ = self.commands.send(MonitorCommand::Refresh);
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
    Stop,
}

fn run_monitor(
    config: MonitorConfig,
    commands: Receiver<MonitorCommand>,
    stopped: Arc<AtomicBool>,
    on_update: Arc<impl Fn(MonitorUpdate) + Send + Sync + 'static>,
) {
    let mut client: Option<CodexAppServer> = None;

    while !stopped.load(Ordering::SeqCst) {
        if client.is_none() {
            match CodexAppServer::spawn(&config) {
                Ok(new_client) => client = Some(new_client),
                Err(error) => {
                    on_update(MonitorUpdate::Error(error.user_message()));
                    if wait_for_command(&commands, config.retry_interval) {
                        break;
                    }
                    continue;
                }
            }
        }

        let delay = match client
            .as_mut()
            .expect("client was initialized")
            .read_rate_limits()
        {
            Ok(snapshot) => {
                on_update(MonitorUpdate::Snapshot(snapshot));
                config.poll_interval
            }
            Err(error) => {
                on_update(MonitorUpdate::Error(error.user_message()));
                client = None;
                config.retry_interval
            }
        };

        if wait_for_command(&commands, delay) {
            break;
        }
    }
}

/// Returns true when the monitor should stop. Refresh commands simply wake it up.
fn wait_for_command(commands: &Receiver<MonitorCommand>, timeout: Duration) -> bool {
    matches!(
        commands.recv_timeout(timeout),
        Ok(MonitorCommand::Stop) | Err(RecvTimeoutError::Disconnected)
    )
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

impl Drop for CodexAppServer {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
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
}

impl CoreError {
    fn user_message(&self) -> String {
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
                "A consulta ao Codex excedeu 15 segundos. Tentaremos novamente em breve.".to_owned()
            }
            Self::AppServerClosed => {
                "O processo `codex app-server` foi encerrado antes de responder.".to_owned()
            }
            Self::Json(_) | Self::MalformedResponse(_) | Self::InvalidRateLimit(_) => {
                "O Codex retornou limites em um formato não reconhecido.".to_owned()
            }
            _ => "Não foi possível atualizar o uso do Codex. Tentaremos novamente em breve.".to_owned(),
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
