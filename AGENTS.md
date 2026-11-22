# Codex Tray — Agent Context

`codex-tray` is a lightweight Rust (Edition 2024, Cargo resolver v3) system tray monitor for ChatGPT/Codex subscription rate limits on Linux (KDE Plasma) and Windows 11.

## Workspace Map

- `Cargo.toml` — Workspace manifest (`version = "0.1.0"`, `license = "MIT"`) and release profile (`strip = true`, `lto = true`, `codegen-units = 1`, `panic = "abort"`).
- `crates/codex-usage-core/src/lib.rs` — Platform-independent core (`UsageMonitor`, `CodexAppServer`, `UsageSnapshot`, `CoreError`).
- `apps/codex-tray-kde/src/main.rs` — Linux KDE Plasma tray frontend (`ksni` / `StatusNotifierItem`).
- `apps/codex-tray-windows/src/main.rs` — Windows native tray frontend (`tao` + `tray-icon`).
- `README.md` — **Human-facing documentation** (in `pt-BR`: prerequisites, builds, autostart, size optimization, env vars). Keep it clear for end users and update it whenever user-facing behavior, build steps, or configuration options change.
- `ARCHITECTURE.md` — Initial design notes (`pt-BR`); see linked docs below for current implementation details where code has evolved beyond it.

## Rules & Guardrails

- **Core/UI Separation**: Keep `crates/codex-usage-core` free of GUI, tray, or desktop-toolkit dependencies.
- **User-Facing Strings**: All UI labels, tooltips, `UsageSnapshot::summary()`, and `CoreError::user_message()` strings are in Brazilian Portuguese (`pt-BR`).
- **Credential Safety**: Never read, copy, log, or print Codex auth/credential files. Keep `CoreError::user_message()` sanitized.
- **Formatting**: Always pass `cargo fmt --all -- --check`.

## Verification Commands

`cargo check --workspace` / `cargo test --workspace` pulls `codex-tray-windows` dependencies (`tao`, `tray-icon`), which fails in offline/sandboxed Linux environments if Windows crates are not cached.

- **Linux / offline sandbox**:
  - `cargo fmt --all -- --check`
  - `cargo test -p codex-usage-core --offline`
  - `cargo test -p codex-tray-kde --offline`
  - `cargo clippy -p codex-usage-core -p codex-tray-kde --offline --all-targets`
- **Windows / full cache**:
  - `cargo check --workspace && cargo test --workspace`

## Detailed Feature Docs (Read On Demand)

- [docs/core-and-protocol.md](docs/core-and-protocol.md) — `CodexAppServer` JSON-RPC protocol, `WireRateLimits` parsing, timers, env vars, and Linux `nvm` binary resolution.
- [docs/tray-frontends.md](docs/tray-frontends.md) — KDE and Windows tray implementations, 32x32 bitmap icon rendering, and current differences vs. `ARCHITECTURE.md`.
- [.agents/skills/build-and-verify/SKILL.md](.agents/skills/build-and-verify/SKILL.md) — Cross-platform verification and size-optimized release build runbook.
