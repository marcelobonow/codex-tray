---
name: build-and-verify
description: >-
  Run formatting, tests, lints, and platform-specific or size-optimized release builds for
  codex-tray. Use when verifying Rust changes across codex-usage-core, codex-tray-kde, or
  codex-tray-windows.
---

# Build and Verify (`codex-tray`)

Use the appropriate commands for the host OS and environment (online vs. offline sandbox).

## 1. Formatting (Always Run)

```bash
cargo fmt --all -- --check
```

## 2. Linux Verification (Core + KDE Frontend)

On Linux, prefer targeting `codex-usage-core` and `codex-tray-kde` directly. Running `--workspace` can fail in offline environments if `codex-tray-windows` crates (`tao`, `tray-icon`) are not cached locally.

```bash
cargo test -p codex-usage-core --offline
cargo test -p codex-tray-kde --offline
cargo clippy -p codex-usage-core -p codex-tray-kde --offline --all-targets
```

Drop `--offline` when running outside an offline sandbox if new crates were added to `Cargo.lock`.

## 3. Windows / Full Workspace Verification

Run on Windows (with MSVC Build Tools + Windows SDK) or in an environment where Windows crate dependencies are available:

```powershell
cargo check --workspace
cargo test --workspace
cargo clippy --workspace --all-targets
```

## 4. Size-Optimized Release Builds

`Cargo.toml` already sets `strip = true`, `lto = true`, `codegen-units = 1`, and `panic = "abort"` on `[profile.release]`. `README.md` documents the `x86_64` size-priority builds:

- **Manjaro KDE (Linux)**:
  ```bash
  CARGO_PROFILE_RELEASE_OPT_LEVEL=z \
  CARGO_PROFILE_RELEASE_LTO=fat \
  CARGO_PROFILE_RELEASE_CODEGEN_UNITS=1 \
  CARGO_PROFILE_RELEASE_STRIP=symbols \
  CARGO_PROFILE_RELEASE_DEBUG=0 \
  CARGO_PROFILE_RELEASE_PANIC=abort \
  CARGO_PROFILE_RELEASE_INCREMENTAL=false \
  cargo +stable build --locked --release -p codex-tray-kde --target x86_64-unknown-linux-gnu
  ```
- **Windows 11 (PowerShell)**: see the `Build com prioridade para tamanho` section in `README.md`.
