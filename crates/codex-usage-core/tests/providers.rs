use std::{
    fs,
    path::{Path, PathBuf},
    sync::{
        Arc,
        atomic::{AtomicBool, AtomicU64, AtomicUsize, Ordering},
        mpsc,
    },
    time::{Duration, UNIX_EPOCH},
};

use codex_usage_core::{
    BackendProvider, CoreError, MonitorConfig, MonitorUpdate, UsageBackend, UsageBucket,
    UsageMonitor, UsageSnapshot, UsageWindow, load_persisted_provider, parse_provider_usage_json,
    save_persisted_provider,
};

struct TempDir {
    path: PathBuf,
}

impl TempDir {
    fn new(prefix: &str) -> Self {
        static COUNTER: AtomicU64 = AtomicU64::new(0);
        let id = COUNTER.fetch_add(1, Ordering::Relaxed);
        let path = std::env::temp_dir().join(format!(
            "codex-tray-test-{prefix}-{}-{id}",
            std::process::id()
        ));
        fs::create_dir_all(&path).expect("temp dir should be created");
        Self { path }
    }

    fn path(&self) -> &Path {
        &self.path
    }
}

impl Drop for TempDir {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.path);
    }
}

fn sample_snapshot(id: &str, primary_used: f64, secondary_used: f64) -> UsageSnapshot {
    UsageSnapshot {
        buckets: vec![UsageBucket {
            id: id.to_owned(),
            label: None,
            plan_type: None,
            primary: UsageWindow {
                used_percent: primary_used,
                window_duration: Duration::from_secs(5 * 3600),
                resets_at: UNIX_EPOCH + Duration::from_secs(1_744_372_800),
            },
            secondary: Some(UsageWindow {
                used_percent: secondary_used,
                window_duration: Duration::from_secs(7 * 86_400),
                resets_at: UNIX_EPOCH + Duration::from_secs(1_744_804_800),
            }),
        }],
    }
}

#[test]
fn backend_provider_metadata_default_and_from_id() {
    assert_eq!(BackendProvider::default(), BackendProvider::Codex);
    assert_eq!(
        BackendProvider::ALL,
        [
            BackendProvider::Codex,
            BackendProvider::Claude,
            BackendProvider::CommandCode,
            BackendProvider::Antigravity,
        ]
    );

    let expected = [
        (BackendProvider::Codex, "codex", "Codex"),
        (BackendProvider::Claude, "claude", "Claude"),
        (BackendProvider::CommandCode, "command-code", "Command Code"),
        (BackendProvider::Antigravity, "antigravity", "Antigravity"),
    ];

    for (provider, id, display_name) in expected {
        assert_eq!(provider.id(), id);
        assert_eq!(provider.display_name(), display_name);
        assert_eq!(BackendProvider::from_id(id), Some(provider));
        assert_eq!(
            BackendProvider::from_id(&id.to_ascii_uppercase()),
            Some(provider)
        );
    }

    // Aliases and case-insensitivity for CommandCode
    assert_eq!(
        BackendProvider::from_id("command_code"),
        Some(BackendProvider::CommandCode)
    );
    assert_eq!(
        BackendProvider::from_id("CommandCode"),
        Some(BackendProvider::CommandCode)
    );
    assert_eq!(
        BackendProvider::from_id("COMMAND-CODE"),
        Some(BackendProvider::CommandCode)
    );

    // Unknown identifiers
    assert_eq!(BackendProvider::from_id(""), None);
    assert_eq!(BackendProvider::from_id("unknown"), None);
}

#[test]
fn persisted_provider_round_trips_for_all_providers() {
    let dir = TempDir::new("persist-roundtrip");
    let config_path = dir.path().join("config.json");

    for provider in BackendProvider::ALL {
        save_persisted_provider(&config_path, provider).expect("save should succeed");
        assert_eq!(load_persisted_provider(&config_path), Some(provider));
    }
}

#[test]
fn persisted_provider_handles_missing_invalid_and_nested_dirs() {
    let dir = TempDir::new("persist-edge");

    // Missing file returns None
    let missing = dir.path().join("does-not-exist.json");
    assert_eq!(load_persisted_provider(&missing), None);

    // Invalid JSON returns None
    let invalid = dir.path().join("invalid.json");
    fs::write(&invalid, "{ not valid json").expect("write invalid json");
    assert_eq!(load_persisted_provider(&invalid), None);

    // Creates nested parent directories
    let nested = dir.path().join("a").join("b").join("c").join("config.json");
    save_persisted_provider(&nested, BackendProvider::Antigravity)
        .expect("save should create nested parent dirs");
    assert_eq!(
        load_persisted_provider(&nested),
        Some(BackendProvider::Antigravity)
    );
}

#[test]
fn parses_claude_status_line_json() {
    let json = r#"{
        "rate_limits": {
            "five_hour": { "used_percentage": 15.0, "resets_at": 1744372800 },
            "seven_day": { "used_percentage": 8.0, "resets_at": 1744804800 }
        }
    }"#;

    let snapshot =
        parse_provider_usage_json(BackendProvider::Claude, json).expect("valid Claude JSON");

    assert_eq!(snapshot.buckets.len(), 1);
    let bucket = &snapshot.buckets[0];
    assert_eq!(bucket.primary.used_percent, 15.0);
    assert_eq!(
        bucket.primary.resets_at,
        UNIX_EPOCH + Duration::from_secs(1_744_372_800)
    );
    let secondary = bucket
        .secondary
        .as_ref()
        .expect("seven_day window should be present");
    assert_eq!(secondary.used_percent, 8.0);
    assert_eq!(
        secondary.resets_at,
        UNIX_EPOCH + Duration::from_secs(1_744_804_800)
    );
}

#[test]
fn parses_generic_primary_secondary_json() {
    let json = r#"{
        "primary": { "used_percent": 42.5, "window_duration_mins": 300, "resets_at": 1744372800 },
        "secondary": { "used_percent": 19.0, "window_duration_mins": 10080, "resets_at": 1744804800 }
    }"#;

    let snapshot = parse_provider_usage_json(BackendProvider::CommandCode, json)
        .expect("valid generic primary/secondary JSON");

    assert_eq!(snapshot.buckets.len(), 1);
    let bucket = &snapshot.buckets[0];
    assert_eq!(bucket.primary.used_percent, 42.5);
    assert_eq!(
        bucket.secondary.as_ref().map(|w| w.used_percent),
        Some(19.0)
    );
}

#[test]
fn parses_codex_wire_rate_limits_json() {
    let json = r#"{
        "rateLimits": {
            "limitId": "codex",
            "primary": { "usedPercent": 25.0, "windowDurationMins": 300, "resetsAt": 1744372800 },
            "secondary": { "usedPercent": 10.0, "windowDurationMins": 10080, "resetsAt": 1744804800 }
        }
    }"#;

    let snapshot = parse_provider_usage_json(BackendProvider::Codex, json)
        .expect("valid Codex WireRateLimits JSON");

    assert_eq!(snapshot.buckets.len(), 1);
    assert_eq!(snapshot.buckets[0].id, "codex");
    assert_eq!(snapshot.buckets[0].primary.used_percent, 25.0);
    assert_eq!(
        snapshot.buckets[0]
            .secondary
            .as_ref()
            .map(|w| w.used_percent),
        Some(10.0)
    );
}

#[test]
fn rejects_invalid_percentage_and_timestamp() {
    let over_100 = r#"{
        "rate_limits": {
            "five_hour": { "used_percentage": 105.0, "resets_at": 1744372800 }
        }
    }"#;
    assert!(parse_provider_usage_json(BackendProvider::Claude, over_100).is_err());

    let negative_pct = r#"{
        "rate_limits": {
            "five_hour": { "used_percentage": -1.0, "resets_at": 1744372800 }
        }
    }"#;
    assert!(parse_provider_usage_json(BackendProvider::Claude, negative_pct).is_err());

    let negative_timestamp = r#"{
        "rate_limits": {
            "five_hour": { "used_percentage": 20.0, "resets_at": -5 }
        }
    }"#;
    assert!(parse_provider_usage_json(BackendProvider::Claude, negative_timestamp).is_err());
}

struct MockBackend {
    result: Result<UsageSnapshot, fn() -> CoreError>,
    dropped: Arc<AtomicBool>,
}

impl UsageBackend for MockBackend {
    fn fetch_usage(&mut self) -> Result<UsageSnapshot, CoreError> {
        match &self.result {
            Ok(snapshot) => Ok(snapshot.clone()),
            Err(make_err) => Err(make_err()),
        }
    }
}

impl Drop for MockBackend {
    fn drop(&mut self) {
        self.dropped.store(true, Ordering::SeqCst);
    }
}

#[test]
fn monitor_live_switches_provider_drops_old_backend_and_persists() {
    let dir = TempDir::new("monitor-switch");
    let config_path = dir.path().join("config.json");

    let codex_dropped = Arc::new(AtomicBool::new(false));
    let claude_dropped = Arc::new(AtomicBool::new(false));
    let antigravity_dropped = Arc::new(AtomicBool::new(false));

    let codex_dropped_factory = Arc::clone(&codex_dropped);
    let claude_dropped_factory = Arc::clone(&claude_dropped);
    let antigravity_dropped_factory = Arc::clone(&antigravity_dropped);

    let (tx, rx) = mpsc::channel::<MonitorUpdate>();

    let config = MonitorConfig {
        provider: BackendProvider::Codex,
        config_path: Some(config_path.clone()),
        poll_interval: Duration::from_secs(60),
        retry_interval: Duration::from_secs(60),
        request_timeout: Duration::from_secs(5),
        ..MonitorConfig::default()
    };

    let mut monitor = UsageMonitor::start_with_factory(
        config,
        move |provider, _cfg| match provider {
            BackendProvider::Codex => Ok(Box::new(MockBackend {
                result: Ok(sample_snapshot("codex", 20.0, 10.0)),
                dropped: Arc::clone(&codex_dropped_factory),
            })),
            BackendProvider::Claude => Ok(Box::new(MockBackend {
                result: Ok(sample_snapshot("claude", 35.0, 12.0)),
                dropped: Arc::clone(&claude_dropped_factory),
            })),
            BackendProvider::Antigravity => Ok(Box::new(MockBackend {
                result: Ok(sample_snapshot("antigravity", 50.0, 25.0)),
                dropped: Arc::clone(&antigravity_dropped_factory),
            })),
            BackendProvider::CommandCode => unreachable!("not used in this test"),
        },
        move |update| {
            let _ = tx.send(update);
        },
    )
    .expect("monitor should start");

    // Initial snapshot must be tagged with Codex
    let first = rx
        .recv_timeout(Duration::from_secs(3))
        .expect("should receive initial Codex update");
    match first {
        MonitorUpdate::Snapshot { provider, snapshot } => {
            assert_eq!(provider, BackendProvider::Codex);
            assert_eq!(snapshot.buckets[0].primary.used_percent, 20.0);
        }
        other => panic!("expected Codex snapshot, got {other:?}"),
    }
    assert!(!codex_dropped.load(Ordering::SeqCst));

    // Switch to Claude via UsageMonitor::set_provider
    monitor.set_provider(BackendProvider::Claude);

    let second = rx
        .recv_timeout(Duration::from_secs(3))
        .expect("should receive Claude update after switch");
    match second {
        MonitorUpdate::Snapshot { provider, snapshot } => {
            assert_eq!(provider, BackendProvider::Claude);
            assert_eq!(snapshot.buckets[0].primary.used_percent, 35.0);
        }
        other => panic!("expected Claude snapshot, got {other:?}"),
    }

    assert!(
        codex_dropped.load(Ordering::SeqCst),
        "old Codex backend should be dropped on switch"
    );
    assert_eq!(
        load_persisted_provider(&config_path),
        Some(BackendProvider::Claude),
        "selected provider should be persisted to config_path"
    );

    // Switch to Antigravity via UsageMonitorController::set_provider
    let controller = monitor.controller();
    controller.set_provider(BackendProvider::Antigravity);

    let third = rx
        .recv_timeout(Duration::from_secs(3))
        .expect("should receive Antigravity update after controller switch");
    match third {
        MonitorUpdate::Snapshot { provider, snapshot } => {
            assert_eq!(provider, BackendProvider::Antigravity);
            assert_eq!(snapshot.buckets[0].primary.used_percent, 50.0);
        }
        other => panic!("expected Antigravity snapshot, got {other:?}"),
    }
    assert!(
        claude_dropped.load(Ordering::SeqCst),
        "old Claude backend should be dropped on switch"
    );
    assert_eq!(
        load_persisted_provider(&config_path),
        Some(BackendProvider::Antigravity)
    );

    monitor.stop();
    assert!(antigravity_dropped.load(Ordering::SeqCst));
}

#[test]
fn monitor_emits_provider_tagged_sanitized_error() {
    let (tx, rx) = mpsc::channel::<MonitorUpdate>();

    let config = MonitorConfig {
        provider: BackendProvider::Claude,
        config_path: None,
        poll_interval: Duration::from_secs(60),
        retry_interval: Duration::from_secs(60),
        request_timeout: Duration::from_secs(5),
        ..MonitorConfig::default()
    };

    let mut monitor = UsageMonitor::start_with_factory(
        config,
        |_provider, _cfg| {
            Ok(Box::new(MockBackend {
                result: Err(|| CoreError::InvalidRateLimit("secret-token-leak".to_owned())),
                dropped: Arc::new(AtomicBool::new(false)),
            }))
        },
        move |update| {
            let _ = tx.send(update);
        },
    )
    .expect("monitor should start");

    let update = rx
        .recv_timeout(Duration::from_secs(3))
        .expect("should receive error update");
    match update {
        MonitorUpdate::Error { provider, message } => {
            assert_eq!(provider, BackendProvider::Claude);
            assert!(
                !message.trim().is_empty(),
                "error message must not be empty"
            );
            assert!(
                !message.contains("secret-token-leak"),
                "user_message must be sanitized and not leak raw internal error text"
            );
        }
        other => panic!("expected MonitorUpdate::Error, got {other:?}"),
    }

    monitor.stop();
}

#[test]
fn persisted_provider_uses_expected_json_schema() {
    let dir = TempDir::new("persist-schema");
    let config_path = dir.path().join("config.json");

    save_persisted_provider(&config_path, BackendProvider::Claude).expect("save should succeed");
    let raw = fs::read_to_string(&config_path).expect("config file should be readable");
    let parsed: serde_json::Value =
        serde_json::from_str(&raw).expect("persisted config must be valid JSON");
    assert_eq!(parsed["provider"], "claude");

    fs::write(&config_path, r#"{ "provider": "command-code" }"#).expect("write raw schema");
    assert_eq!(
        load_persisted_provider(&config_path),
        Some(BackendProvider::CommandCode)
    );
}

#[test]
fn parses_generic_primary_secondary_json_with_used_percentage_and_default_durations() {
    let json = r#"{
        "primary": { "used_percentage": 61.5, "resets_at": 1744372800 },
        "secondary": { "used_percentage": 22.0, "resets_at": 1744804800 }
    }"#;

    let snapshot = parse_provider_usage_json(BackendProvider::Antigravity, json)
        .expect("valid generic JSON with used_percentage and omitted durations");

    assert_eq!(snapshot.buckets.len(), 1);
    let bucket = &snapshot.buckets[0];
    assert_eq!(bucket.primary.used_percent, 61.5);
    assert_eq!(
        bucket.primary.window_duration,
        Duration::from_secs(5 * 3600)
    );
    assert_eq!(
        bucket.primary.resets_at,
        UNIX_EPOCH + Duration::from_secs(1_744_372_800)
    );

    let secondary = bucket
        .secondary
        .as_ref()
        .expect("secondary window should be present");
    assert_eq!(secondary.used_percent, 22.0);
    assert_eq!(secondary.window_duration, Duration::from_secs(7 * 86_400));
    assert_eq!(
        secondary.resets_at,
        UNIX_EPOCH + Duration::from_secs(1_744_804_800)
    );

    let primary_only_json = r#"{
        "primary": { "used_percentage": 12.0, "resets_at": 1744372800 }
    }"#;
    let primary_only = parse_provider_usage_json(BackendProvider::CommandCode, primary_only_json)
        .expect("valid generic JSON with only primary window");
    assert_eq!(primary_only.buckets.len(), 1);
    assert_eq!(primary_only.buckets[0].primary.used_percent, 12.0);
    assert_eq!(
        primary_only.buckets[0].primary.window_duration,
        Duration::from_secs(5 * 3600)
    );
    assert!(primary_only.buckets[0].secondary.is_none());
}

#[test]
fn parses_provider_json_edge_cases_and_codex_by_limit_id() {
    let boundary_json = r#"{
        "rate_limits": {
            "five_hour": { "used_percentage": 0.0, "resets_at": 0 },
            "seven_day": { "used_percentage": 100.0, "resets_at": 1744804800 }
        }
    }"#;
    let boundary = parse_provider_usage_json(BackendProvider::Claude, boundary_json)
        .expect("0% and 100% boundaries and epoch 0 timestamp should be accepted");
    assert_eq!(boundary.buckets[0].primary.used_percent, 0.0);
    assert_eq!(boundary.buckets[0].primary.resets_at, UNIX_EPOCH);
    assert_eq!(
        boundary.buckets[0]
            .secondary
            .as_ref()
            .map(|w| w.used_percent),
        Some(100.0)
    );

    let by_limit_id_json = r#"{
        "rateLimitsByLimitId": {
            "codex_extra": {
                "limitId": "codex_extra",
                "primary": { "usedPercent": 40.0, "windowDurationMins": 60, "resetsAt": 1744372800 }
            },
            "codex": {
                "limitId": "codex",
                "primary": { "usedPercent": 15.0, "windowDurationMins": 300, "resetsAt": 1744372800 }
            }
        }
    }"#;
    let codex_multi = parse_provider_usage_json(BackendProvider::Codex, by_limit_id_json)
        .expect("rateLimitsByLimitId JSON should parse");
    assert_eq!(codex_multi.buckets.len(), 2);
    assert_eq!(codex_multi.buckets[0].id, "codex");
    assert_eq!(codex_multi.buckets[0].primary.used_percent, 15.0);

    assert!(parse_provider_usage_json(BackendProvider::Claude, r#"{"foo": "bar"}"#).is_err());
    assert!(parse_provider_usage_json(BackendProvider::Claude, r#"[]"#).is_err());
    assert!(
        parse_provider_usage_json(
            BackendProvider::Claude,
            r#"{"rate_limits": {"seven_day": {"used_percentage": 5.0, "resets_at": 1744804800}}}"#
        )
        .is_err()
    );
}

struct CountingBackend {
    provider_id: &'static str,
    fetches: Arc<AtomicUsize>,
    dropped: Arc<AtomicBool>,
}

impl UsageBackend for CountingBackend {
    fn fetch_usage(&mut self) -> Result<UsageSnapshot, CoreError> {
        let count = self.fetches.fetch_add(1, Ordering::SeqCst) + 1;
        Ok(sample_snapshot(self.provider_id, count as f64 * 10.0, 5.0))
    }
}

impl Drop for CountingBackend {
    fn drop(&mut self) {
        self.dropped.store(true, Ordering::SeqCst);
    }
}

#[test]
fn set_provider_with_same_active_provider_refreshes_without_dropping_backend() {
    let dir = TempDir::new("monitor-same-provider");
    let config_path = dir.path().join("config.json");

    let fetches = Arc::new(AtomicUsize::new(0));
    let dropped = Arc::new(AtomicBool::new(false));
    let factory_calls = Arc::new(AtomicUsize::new(0));

    let fetches_factory = Arc::clone(&fetches);
    let dropped_factory = Arc::clone(&dropped);
    let factory_calls_clone = Arc::clone(&factory_calls);

    let (tx, rx) = mpsc::channel::<MonitorUpdate>();

    let config = MonitorConfig {
        provider: BackendProvider::Codex,
        config_path: Some(config_path.clone()),
        poll_interval: Duration::from_secs(60),
        retry_interval: Duration::from_secs(60),
        request_timeout: Duration::from_secs(5),
        ..MonitorConfig::default()
    };

    let mut monitor = UsageMonitor::start_with_factory(
        config,
        move |provider, _cfg| {
            assert_eq!(provider, BackendProvider::Codex);
            factory_calls_clone.fetch_add(1, Ordering::SeqCst);
            Ok(Box::new(CountingBackend {
                provider_id: "codex",
                fetches: Arc::clone(&fetches_factory),
                dropped: Arc::clone(&dropped_factory),
            }))
        },
        move |update| {
            let _ = tx.send(update);
        },
    )
    .expect("monitor should start");

    let first = rx
        .recv_timeout(Duration::from_secs(3))
        .expect("should receive initial snapshot");
    match first {
        MonitorUpdate::Snapshot { provider, snapshot } => {
            assert_eq!(provider, BackendProvider::Codex);
            assert_eq!(snapshot.buckets[0].primary.used_percent, 10.0);
        }
        other => panic!("expected initial Codex snapshot, got {other:?}"),
    }
    assert!(!dropped.load(Ordering::SeqCst));
    assert_eq!(factory_calls.load(Ordering::SeqCst), 1);

    // Calling set_provider with the same active provider must refresh immediately without dropping
    monitor.set_provider(BackendProvider::Codex);

    let second = rx
        .recv_timeout(Duration::from_secs(3))
        .expect("should receive immediate refresh snapshot for same provider");
    match second {
        MonitorUpdate::Snapshot { provider, snapshot } => {
            assert_eq!(provider, BackendProvider::Codex);
            assert_eq!(snapshot.buckets[0].primary.used_percent, 20.0);
        }
        other => panic!("expected refreshed Codex snapshot, got {other:?}"),
    }
    assert!(
        !dropped.load(Ordering::SeqCst),
        "backend must not be dropped when set_provider is called with the same active provider"
    );
    assert_eq!(factory_calls.load(Ordering::SeqCst), 1);
    assert_eq!(fetches.load(Ordering::SeqCst), 2);
    assert_eq!(
        load_persisted_provider(&config_path),
        Some(BackendProvider::Codex)
    );

    monitor.stop();
    assert!(dropped.load(Ordering::SeqCst));
}

#[test]
fn monitor_switch_recovers_from_error_and_ignores_persistence_io_errors() {
    let dir = TempDir::new("monitor-persist-io-err");
    let blocker_file = dir.path().join("not-a-directory");
    fs::write(&blocker_file, "blocker").expect("write blocker file");
    let unwritable_config_path = blocker_file.join("config.json");

    let (tx, rx) = mpsc::channel::<MonitorUpdate>();

    let config = MonitorConfig {
        provider: BackendProvider::Codex,
        config_path: Some(unwritable_config_path),
        poll_interval: Duration::from_secs(60),
        retry_interval: Duration::from_secs(60),
        request_timeout: Duration::from_secs(5),
        ..MonitorConfig::default()
    };

    let mut monitor = UsageMonitor::start_with_factory(
        config,
        move |provider, _cfg| match provider {
            BackendProvider::Codex => Ok(Box::new(MockBackend {
                result: Err(|| CoreError::AppServerClosed),
                dropped: Arc::new(AtomicBool::new(false)),
            })),
            BackendProvider::Claude => Ok(Box::new(MockBackend {
                result: Ok(sample_snapshot("claude", 28.0, 9.0)),
                dropped: Arc::new(AtomicBool::new(false)),
            })),
            other => unreachable!("unexpected provider {other:?}"),
        },
        move |update| {
            let _ = tx.send(update);
        },
    )
    .expect("monitor should start");

    let first = rx
        .recv_timeout(Duration::from_secs(3))
        .expect("should receive initial error update");
    assert!(matches!(
        first,
        MonitorUpdate::Error {
            provider: BackendProvider::Codex,
            ..
        }
    ));

    // Switch while in retry wait and with an unwritable config_path: must still switch immediately
    monitor.set_provider(BackendProvider::Claude);

    let second = rx
        .recv_timeout(Duration::from_secs(3))
        .expect("should immediately recover and emit snapshot for new provider");
    match second {
        MonitorUpdate::Snapshot { provider, snapshot } => {
            assert_eq!(provider, BackendProvider::Claude);
            assert_eq!(snapshot.buckets[0].primary.used_percent, 28.0);
        }
        other => panic!("expected Claude snapshot after switch, got {other:?}"),
    }

    monitor.stop();
}

#[test]
fn parses_antigravity_accounts_quotas_json() {
    let json = r#"{
        "accounts": [
            {
                "email": "inactive@example.com",
                "active": false,
                "quotas": [
                    { "modelId": "other", "window": "5h", "remainingPercentage": 10.0, "resetAt": "2026-11-21T22:00:00Z" }
                ]
            },
            {
                "email": "active@example.com",
                "active": true,
                "quotas": [
                    { "modelId": "gemini-flash", "group": "Gemini", "window": "5h", "remainingPercentage": 85.0, "resetAt": "2026-11-21T22:00:00Z" },
                    { "modelId": "gemini-weekly", "group": "Gemini", "window": "7d", "remainingPercentage": 60.0, "resetAt": "2026-11-25T00:00:00.500Z" }
                ]
            }
        ]
    }"#;

    let snapshot = parse_provider_usage_json(BackendProvider::Antigravity, json)
        .expect("valid antigravity accounts JSON");
    assert_eq!(snapshot.buckets.len(), 1);
    let bucket = &snapshot.buckets[0];
    assert_eq!(bucket.label.as_deref(), Some("Gemini"));
    assert!((bucket.primary.used_percent - 15.0).abs() < 1e-6);
    assert_eq!(
        bucket.primary.window_duration,
        Duration::from_secs(5 * 3600)
    );
    let secondary = bucket
        .secondary
        .as_ref()
        .expect("7d window should be secondary");
    assert!((secondary.used_percent - 40.0).abs() < 1e-6);
    assert_eq!(secondary.window_duration, Duration::from_secs(7 * 86_400));
}

#[test]
fn parses_antigravity_groups_and_skips_disabled_buckets() {
    let json = r#"{
        "response": {
            "groups": [
                {
                    "displayName": "Gemini Models",
                    "buckets": [
                        { "bucketId": "disabled-one", "window": "5h", "remainingFraction": 0.0, "disabled": true, "resetTime": "2026-11-21T22:00:00Z" },
                        { "bucketId": "gemini-5h", "window": "5h", "remainingFraction": 0.75, "disabled": false, "resetTime": "2026-11-21T22:00:00-03:00" },
                        { "bucketId": "gemini-7d", "window": "7d", "remainingFraction": 0.50, "disabled": false, "resetTime": 1795564800 }
                    ]
                }
            ]
        }
    }"#;

    let snapshot = parse_provider_usage_json(BackendProvider::Antigravity, json)
        .expect("valid antigravity groups JSON");
    assert_eq!(snapshot.buckets.len(), 1);
    let bucket = &snapshot.buckets[0];
    assert_eq!(bucket.label.as_deref(), Some("Gemini Models"));
    assert!((bucket.primary.used_percent - 25.0).abs() < 1e-6);
    let secondary = bucket.secondary.as_ref().expect("secondary window present");
    assert!((secondary.used_percent - 50.0).abs() < 1e-6);
    assert_eq!(
        secondary.resets_at,
        UNIX_EPOCH + Duration::from_secs(1_795_564_800)
    );
}

#[test]
fn rejects_invalid_antigravity_fraction_and_iso8601_timestamp() {
    let bad_fraction = r#"{
        "models": [
            { "modelId": "gemini", "window": "5h", "remainingFraction": 1.5, "resetTime": "2026-11-21T22:00:00Z" }
        ]
    }"#;
    assert!(parse_provider_usage_json(BackendProvider::Antigravity, bad_fraction).is_err());

    let bad_timestamp = r#"{
        "models": [
            { "modelId": "gemini", "window": "5h", "remainingFraction": 0.5, "resetTime": "not-a-timestamp" }
        ]
    }"#;
    assert!(parse_provider_usage_json(BackendProvider::Antigravity, bad_timestamp).is_err());
}

#[test]
fn parses_real_antigravity_usage_cli_models_snapshot() {
    let json = r#"{
        "timestamp": "2026-11-22T16:00:00.000Z",
        "method": "google",
        "models": [
            {
                "label": "Gemini Autocomplete",
                "modelId": "gemini-2.5-flash-002",
                "remainingPercentage": 0.1,
                "isExhausted": false,
                "isAutocompleteOnly": true,
                "resetTime": "2026-11-22T21:00:00Z",
                "timeUntilResetMs": 18000000
            },
            {
                "label": "Unused Model 5h",
                "modelId": "unused-5h",
                "remainingPercentage": 1.0,
                "isExhausted": false,
                "isAutocompleteOnly": false,
                "resetTime": "2026-11-22T21:00:00Z",
                "timeUntilResetMs": 18000000
            },
            {
                "label": "Gemini 3 Flash",
                "modelId": "gemini-3-flash",
                "remainingPercentage": 0.85,
                "isExhausted": false,
                "isAutocompleteOnly": false,
                "resetTime": "2026-11-22T21:00:00Z",
                "timeUntilResetMs": 18000000
            },
            {
                "label": "Claude Sonnet Weekly",
                "modelId": "claude-sonnet",
                "remainingPercentage": 0.4,
                "isExhausted": false,
                "isAutocompleteOnly": false,
                "resetTime": "2026-11-29T00:00:00Z",
                "timeUntilResetMs": 547200000
            }
        ]
    }"#;

    let snapshot = parse_provider_usage_json(BackendProvider::Antigravity, json)
        .expect("real antigravity-usage CLI JSON should parse");
    assert_eq!(snapshot.buckets.len(), 1);
    let bucket = &snapshot.buckets[0];
    // Picks the highest-used 5h model (15% used) instead of the 0% unused model
    assert!((bucket.primary.used_percent - 15.0).abs() < 1e-6);
    assert_eq!(
        bucket.primary.window_duration,
        Duration::from_secs(5 * 3600)
    );
    // Infers 7d window from timeUntilResetMs > 24h
    let secondary = bucket
        .secondary
        .as_ref()
        .expect("weekly model should populate secondary");
    assert!((secondary.used_percent - 60.0).abs() < 1e-6);
    assert_eq!(secondary.window_duration, Duration::from_secs(7 * 86_400));
}

#[test]
fn log_to_path_writes_and_rotates_large_files() {
    let dir = TempDir::new("log-rotation");
    let log_path = dir.path().join("nested").join("codex-tray.log");

    codex_usage_core::log_to_path(&log_path, "INFO", "hello\nworld");
    let content = fs::read_to_string(&log_path).expect("log file should exist");
    assert!(content.contains("[INFO] hello world"));

    // Grow file past 256 KiB to trigger rotation
    fs::write(&log_path, vec![b'x'; 256 * 1024]).expect("seed oversized log");
    codex_usage_core::log_to_path(&log_path, "ERROR", "after-rotate");

    let rotated_path = log_path.with_extension("log.old");
    assert!(rotated_path.is_file(), "rotated log.old should exist");
    let after = fs::read_to_string(&log_path).expect("fresh log should exist");
    assert!(after.contains("[ERROR] after-rotate"));
    assert!(after.len() < 1024);
}

#[test]
fn parses_real_agy_cli_usage_json() {
    let json = r#"{
        "conversation_id": "",
        "status": "SUCCESS",
        "duration_seconds": 0,
        "num_turns": 0,
        "command": {
            "name": "usage",
            "data": {
                "groups": [
                    {
                        "name": "Gemini Models",
                        "buckets": [
                            {
                                "id": "gemini-weekly",
                                "name": "Weekly Limit Remaining",
                                "window": "weekly",
                                "remaining_fraction": 0.9205352663993835,
                                "reset_time": "2026-11-28T17:36:26Z"
                            },
                            {
                                "id": "gemini-5h",
                                "name": "Five Hour Limit Remaining",
                                "window": "5h",
                                "remaining_fraction": 0.9172764420509338,
                                "reset_time": "2026-11-22T20:55:53Z"
                            }
                        ]
                    },
                    {
                        "name": "Claude and GPT models",
                        "buckets": [
                            {
                                "id": "3p-weekly",
                                "name": "Weekly Limit Remaining",
                                "window": "weekly",
                                "remaining_fraction": 1,
                                "reset_time": "2026-11-29T19:15:15Z"
                            },
                            {
                                "id": "3p-5h",
                                "name": "Five Hour Limit Remaining",
                                "window": "5h",
                                "remaining_fraction": 1,
                                "reset_time": "2026-11-23T00:15:15Z"
                            }
                        ]
                    },
                    {
                        "name": "Gemini 4 Argon",
                        "buckets": [
                            {
                                "id": "gemini-argon-wtu-weekly",
                                "name": "Gemini 4 Argon Weekly Limit",
                                "window": "weekly",
                                "remaining_fraction": 1,
                                "reset_time": "2026-11-29T19:15:15Z"
                            }
                        ]
                    }
                ]
            }
        }
    }"#;

    let snapshot = parse_provider_usage_json(BackendProvider::Antigravity, json)
        .expect("real agy -p /usage --output-format json should parse");
    assert_eq!(snapshot.buckets.len(), 3);

    let gemini = &snapshot.buckets[0];
    assert_eq!(gemini.label.as_deref(), Some("Gemini Models"));
    assert_eq!(
        gemini.primary.window_duration,
        Duration::from_secs(5 * 3600)
    );
    assert!((gemini.primary.used_percent - 8.27235579490662).abs() < 1e-4);
    let gemini_weekly = gemini
        .secondary
        .as_ref()
        .expect("weekly window should be secondary");
    assert_eq!(
        gemini_weekly.window_duration,
        Duration::from_secs(7 * 86_400)
    );
    assert!((gemini_weekly.used_percent - 7.94647336006165).abs() < 1e-4);

    // Tray icon rows should show ~8% / ~8% (rounded from 8.27% and 7.95%)
    assert_eq!(snapshot.icon_usage_rows(), ("8".to_owned(), "8".to_owned()));
}
