# Core Library & App Server Protocol

Details for `crates/codex-usage-core/src/lib.rs`.

## Public API

- `UsageMonitor`: `start`, `refresh`, `controller`, `stop` (also stops on `Drop`). Runs a `codex-usage-monitor` worker thread and emits `MonitorUpdate::Snapshot(UsageSnapshot)` or `MonitorUpdate::Error(String)`.
- `UsageMonitorController`: cloneable handle exposing `refresh()` to wake the monitor loop immediately.
- `MonitorConfig`: `codex_binary`, `poll_interval`, `retry_interval`, `request_timeout`.
- Domain types: `UsageSnapshot` (`buckets: Vec<UsageBucket>`), `UsageBucket` (`id`, `label`, `plan_type`, `primary`, `secondary`), `UsageWindow` (`used_percent`, `window_duration`, `resets_at`), `CoreError`.

## `CodexAppServer` Lifecycle & JSON-RPC

- Spawns `<codex_binary> app-server` with piped `stdin`/`stdout` and `stderr(Stdio::null())`.
  - On Windows release builds (`#[cfg(all(target_os = "windows", not(debug_assertions)))]`), sets `CREATE_NO_WINDOW` (`0x0800_0000`).
  - Spawns a `codex-app-server-reader` thread parsing stdout lines as `serde_json::Value`.
- Handshake on startup:
  1. Request `initialize` with `clientInfo` (`name: "codex_tray"`, `title: "Codex Tray"`, `version`).
  2. Notification `initialized` (`{}`).
- Polling: calls `account/rateLimits/read` with `params: null`.
- `CodexAppServer::request` discards messages whose `"id"` does not match the active request ID, so push notifications such as `account/rateLimits/updated` are not handled yet.
- On request or parse error, `run_monitor` emits `CoreError::user_message()`, drops `CodexAppServer` (killing and reaping the child process), and retries after `retry_interval`.

## Rate-Limit Parsing & Formatting

- `WireRateLimits` prefers `rateLimitsByLimitId` when non-empty and falls back to legacy `rateLimits` (defaulting `id` to `"codex"`).
- Buckets without a `primary` window are skipped; `usedPercent` must be in `0.0..=100.0` and `resetsAt` must be a valid non-negative Unix timestamp in seconds.
- Buckets are sorted with `"codex"` first, then lexicographically by `id`. Error is returned if no valid buckets remain.
- `UsageSnapshot` helpers (`icon_usage_rows`, `compact_usage`, `summary`, `menu_status`, `tooltip`) currently inspect `self.buckets.first()`:
  - `icon_usage_rows()`: `(primary_pct, secondary_pct_or_em_dash)` (e.g., `("33", "66")` or `("3", "—")`).
  - `menu_status(now)`: `{remaining}: {pct}%/{remaining}: {pct}%` (e.g., `2h35m: 35%/1d8h24m: 24%`).
  - `summary()` / `tooltip()`: `"Uso 5 horas: {pct}% / Semanal: {pct_or_em_dash}"`.
  - `compact_usage()`: `"{p}/{s}"` if `<= 5` chars, otherwise `"{p}"`.

## Timers & Binary Discovery

- `DEFAULT_POLL_INTERVAL = 60s`, overridable via `CODEX_TRAY_INTERVAL_SECS` (`5..=3600`; invalid/out-of-range values fall back to `60s`).
- `DEFAULT_RETRY_INTERVAL = 15s`, `DEFAULT_REQUEST_TIMEOUT = 15s`.
- `default_codex_binary()`:
  1. Uses `CODEX_TRAY_CODEX_BIN` if set.
  2. On Linux, if `codex` is not on `PATH`, searches `NVM_DIR` and `~/.nvm/versions/node/*/bin/codex` (requiring `node` in the same `bin` dir), honoring `alias/default` (`lts/*`, `lts/<name>`, or version prefix) or falling back to highest semver.
  3. Falls back to `"codex"`.
- On Unix, if the resolved binary sits next to `node`, `CodexAppServer::spawn` prepends that directory to the child process's `PATH`.
