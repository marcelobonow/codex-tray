# Codex Tray

Codex Tray displays Codex account usage limits in the system tray. It includes a KDE Plasma frontend for Linux and a native tray frontend for Windows. Both use the shared `codex-usage-core` library to query the Codex App Server.

## Build and run

### Requirements

- Stable Rust toolchain (edition 2024) with Cargo.
- Codex CLI installed, logged in, and available as `codex` on your `PATH`.
- For the KDE app: Linux running a KDE Plasma session with a user D-Bus session.
- For the Windows app: build and run on Windows.

From the repository root, run the frontend for your platform:

```bash
# KDE Plasma (Linux)
cargo run -p codex-tray-kde
```

```powershell
# Windows
cargo run -p codex-tray-windows
```

Build without launching the app:

```bash
cargo build --release -p codex-tray-kde      # KDE/Linux
cargo build --release -p codex-tray-windows  # Windows
```

The workspace's release profile strips symbols, enables link-time optimization (LTO), uses one codegen unit, and aborts on panic. These settings help reduce executable size; they do not guarantee lower runtime memory use. Cargo writes release binaries under `target/release/`; the Windows executable is `target/release/codex-tray-windows.exe`.

Useful workspace commands:

```bash
cargo check --workspace
cargo test --workspace
cargo fmt --all
```

## Start with the system

### Windows

- **Visual:** Open **Settings → Apps → Startup** and enable Codex Tray if it appears there. If it does not, use `Win+R`, enter `shell:startup`, and place a shortcut to `codex-tray-windows.exe` in that folder.
- **PowerShell:** From the repository root, create a shortcut in the current user's Startup folder:

  ```powershell
  $exe = (Resolve-Path .\target\release\codex-tray-windows.exe).Path; $startup = [Environment]::GetFolderPath('Startup'); $shortcut = (New-Object -ComObject WScript.Shell).CreateShortcut((Join-Path $startup 'Codex Tray.lnk')); $shortcut.TargetPath = $exe; $shortcut.Save()
  ```

### KDE Plasma (Linux)

- **Visual:** Open **System Settings → Autostart → Add… → Add Application**, then select `target/release/codex-tray-kde`.
- **Command:** From the repository root, add the executable to KDE's autostart scripts:

  ```bash
  mkdir -p ~/.config/autostart-scripts && ln -sf "$PWD/target/release/codex-tray-kde" ~/.config/autostart-scripts/codex-tray-kde
  ```

## Configuration

The app starts `codex app-server` and uses the existing Codex CLI login. A standalone API key is not enough to read subscription usage limits. If `codex` is not on `PATH`, the app also checks the default Node version in `~/.nvm` for a Codex CLI installation and adds that version's `bin` directory to the child process's `PATH`. Set `CODEX_TRAY_CODEX_BIN` to a full path to override automatic detection.

Usage is refreshed every 60 seconds by default. Set `CODEX_TRAY_INTERVAL_SECS` to change the interval; accepted values are 5 through 3,600 seconds. After a query failure, the app retries after 15 seconds.

## Project layout

- `apps/codex-tray-kde`: Linux KDE Plasma status tray frontend.
- `apps/codex-tray-windows`: Windows notification area frontend.
- `crates/codex-usage-core`: shared Codex App Server client and usage monitor.

See [ARCHITECTURE.md](ARCHITECTURE.md) for implementation details.
