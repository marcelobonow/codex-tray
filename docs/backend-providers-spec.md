# Design & Specification: Multi-Provider Backend Selector

## 1. Goal

Add a right-click tray submenu allowing the user to switch the active **backend provider** at runtime to monitor remaining usage from:

1. **Codex** (default)
2. **Claude**
3. **Command Code**
4. **Antigravity**

The selected provider is **persisted on disk** so it is restored automatically after a computer restart.

All changes must preserve workspace guardrails (see `AGENTS.md`):
- **Core/UI separation**: `crates/codex-usage-core` remains free of GUI/tray dependencies.
- **Brazilian Portuguese (`pt-BR`)**: All user-facing labels, tooltips, and error messages in `pt-BR`.
- **Credential safety**: Never read, copy, log, or print auth/credential files; keep `CoreError::user_message()` sanitized.
- **No new crate dependencies**: Use only existing workspace crates (`serde`, `serde_json`, `thiserror`, `std`).

---

## 2. Core Library Specification (`crates/codex-usage-core`)

> **TDD Contract**: Integration tests are already written in `crates/codex-usage-core/tests/providers.rs`. Make all 9 tests pass without weakening their assertions.

### 2.1 `BackendProvider` Enum

```rust
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum BackendProvider {
    #[default]
    Codex,
    Claude,
    CommandCode,
    Antigravity,
}
```

Required public API:
- `pub const ALL: [Self; 4] = [Self::Codex, Self::Claude, Self::CommandCode, Self::Antigravity];`
- `pub fn id(self) -> &'static str` -> `"codex"`, `"claude"`, `"command-code"`, `"antigravity"`.
- `pub fn display_name(self) -> &'static str` -> `"Codex"`, `"Claude"`, `"Command Code"`, `"Antigravity"`.
- `pub fn from_id(value: &str) -> Option<Self>` -> case-insensitive match accepting `"codex"`, `"claude"`, `"command-code"` / `"command_code"` / `"commandcode"`, `"antigravity"`.

### 2.2 On-Disk Persistence

Persisted in a JSON file:
- **Path resolution (`default_config_path()`)**:
  1. `CODEX_TRAY_CONFIG_PATH` environment variable (if set and non-empty).
  2. Linux/Unix: `$XDG_CONFIG_HOME/codex-tray/config.json`, falling back to `$HOME/.config/codex-tray/config.json`.
  3. Windows: `%APPDATA%\codex-tray\config.json`.
- **File schema**:
  ```json
  {
    "provider": "claude"
  }
  ```
- **Required public functions**:
  - `pub fn load_persisted_provider(path: &Path) -> Option<BackendProvider>` (missing file or invalid JSON returns `None`).
  - `pub fn save_persisted_provider(path: &Path, provider: BackendProvider) -> io::Result<()>` (creates parent directories if needed; writes atomically via temp file + rename).
- **Startup precedence in `MonitorConfig::default()`**:
  1. `CODEX_TRAY_PROVIDER` env var (if set and valid via `BackendProvider::from_id`).
  2. `load_persisted_provider(&config_path)`.
  3. `BackendProvider::Codex`.
- **`MonitorConfig` fields**:
  - `pub provider: BackendProvider`
  - `pub config_path: Option<PathBuf>`
  - `pub codex_binary: OsString`
  - `pub poll_interval: Duration`
  - `pub retry_interval: Duration`
  - `pub request_timeout: Duration`

### 2.3 `UsageBackend` Trait, JSON Parser & Provider Backends

```rust
pub trait UsageBackend: Send {
    fn fetch_usage(&mut self) -> Result<UsageSnapshot, CoreError>;
}

pub fn parse_provider_usage_json(
    provider: BackendProvider,
    json: &str,
) -> Result<UsageSnapshot, CoreError>;
```

Supported JSON formats in `parse_provider_usage_json`:
1. **Claude Code Status Line JSON**:
   ```json
   {
     "rate_limits": {
       "five_hour": { "used_percentage": 15.0, "resets_at": 1744372800 },
       "seven_day": { "used_percentage": 8.0, "resets_at": 1744804800 }
     }
   }
   ```
2. **Generic `{ "primary": ..., "secondary": ... }`** JSON (accepting `used_percent` or `used_percentage`, `window_duration_mins` or default 5h/7d, `resets_at`).
3. **Codex `WireRateLimits`** JSON (`rateLimits` / `rateLimitsByLimitId`).

Validation: `used_percent`/`used_percentage` must be in `0.0..=100.0`, and `resets_at` must be a valid non-negative Unix timestamp.

Provider implementations:
- **Codex (`CodexAppServer`)**: existing `codex app-server` JSON-RPC protocol implementing `UsageBackend`.
- **Claude (`ClaudeBackend`)**: executes `CODEX_TRAY_CLAUDE_CMD` (if set) or reads `CODEX_TRAY_CLAUDE_USAGE_FILE` (default: `<config_dir>/claude-usage.json`) and parses via `parse_provider_usage_json`.
- **Command Code (`CommandCodeBackend`)**: executes `CODEX_TRAY_COMMAND_CODE_CMD` (if set) or reads `CODEX_TRAY_COMMAND_CODE_USAGE_FILE` (default: `<config_dir>/command-code-usage.json`) and parses via `parse_provider_usage_json`.
- **Antigravity (`AntigravityBackend`)**: executes `CODEX_TRAY_ANTIGRAVITY_CMD` (if set) or reads `CODEX_TRAY_ANTIGRAVITY_USAGE_FILE` (default: `<config_dir>/antigravity-usage.json`) and parses via `parse_provider_usage_json`.

### 2.4 `MonitorUpdate`, `MonitorCommand`, and Live Switching

```rust
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
```

Required public API:
- `UsageMonitor::start_with_factory<B, F>(config: MonitorConfig, factory: B, on_update: F) -> Result<Self, CoreError>` where:
  - `B: Fn(BackendProvider, &MonitorConfig) -> Result<Box<dyn UsageBackend>, CoreError> + Send + 'static`
  - `F: Fn(MonitorUpdate) + Send + Sync + 'static`
- `UsageMonitor::set_provider(&self, provider: BackendProvider)`
- `UsageMonitorController::set_provider(&self, provider: BackendProvider)`

Worker loop behavior on `SetProvider(next)`:
- Persists `next` via `save_persisted_provider` if `config.config_path` is `Some(path)` (ignore I/O errors so UI switching never fails).
- If `next != active_provider`: drops the current `Box<dyn UsageBackend>` (killing/reaping `codex app-server` if active), updates `active_provider = next`, and immediately fetches usage from the new provider.
- If `next == active_provider`: triggers an immediate refresh.

---

## 3. Tray Frontends Specification

### 3.1 KDE Plasma (`apps/codex-tray-kde/src/main.rs`)

- Store `provider: BackendProvider` in `KdeTray` (initialized from `config.provider`).
- `KdeTray::menu(&self)`:
  1. Disabled status item (`self.status_text`)
  2. `"Atualizar agora"`
  3. `"Provedor"` (`ksni::menu::SubMenu`) containing a `ksni::menu::RadioGroup` over `BackendProvider::ALL`:
     - Selecting a provider updates `tray.provider`, resets `icon_rows = ("—", "—")`, `failed = false`, sets `status_text = format!("Consultando uso de {}…", next.display_name())`, and calls `tray.monitor.set_provider(next)`.
  4. `MenuItem::Separator`
  5. `"Sair"`
- `KdeTray::apply_update(&mut self, update: MonitorUpdate)`:
  - Ignores updates where `provider != self.provider`.
  - Tooltip title includes active provider: `format!("Codex Tray — {}", self.provider.display_name())`.
- Add unit tests in `apps/codex-tray-kde/src/main.rs` verifying `KdeTray::apply_update` ignores stale provider updates and applies active provider updates.

### 3.2 Windows (`apps/codex-tray-windows/src/main.rs`)

- Track `active_provider: BackendProvider` (initialized from `config.provider`).
- Right-click menu:
  1. `status_item` (disabled)
  2. `"Atualizar agora"` (`MenuItem`)
  3. `Submenu::new("Provedor", true)` containing 4 `CheckMenuItem`s (radio behavior).
  4. `PredefinedMenuItem::separator()`
  5. `quit_item` (`"Sair"`)
- Selecting a provider updates checked states, resets status/tooltip/icon, and calls `monitor.set_provider(selected)`.
- Ignores `MonitorUpdate` events where `provider != active_provider`.

---

## 4. Next-Session Handoff Checklist

1. **Already completed**:
   - Integration tests written in `crates/codex-usage-core/tests/providers.rs` (9 tests).
   - `~/.gemini/config/agents/code-implementer.md` and `test-writer.md` updated with `tools: [read_file, edit_file, write_file, run_command, manage_task, search_web]`.
2. **Remaining for `code-implementer` in the next session**:
   - Implement core changes in `crates/codex-usage-core/src/lib.rs` (or submodules).
   - Implement submenu + live switching in `apps/codex-tray-kde/src/main.rs` (plus unit tests) and `apps/codex-tray-windows/src/main.rs`.
   - Update `README.md` with the provider submenu, persistence (`config.json`), and provider env vars.
   - Run verification:
     - `cargo fmt --all -- --check`
     - `cargo test -p codex-usage-core --offline`
     - `cargo test -p codex-tray-kde --offline`
     - `cargo clippy -p codex-usage-core -p codex-tray-kde --offline --all-targets`
