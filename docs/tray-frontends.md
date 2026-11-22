# Tray Frontends & Current Gaps vs. `ARCHITECTURE.md`

Details for `apps/codex-tray-kde/src/main.rs` and `apps/codex-tray-windows/src/main.rs`.

## KDE Plasma (`apps/codex-tray-kde`)

- Uses `ksni` (`0.3`, `blocking` feature) under `[target.'cfg(target_os = "linux")'.dependencies]` and `#[cfg(target_os = "linux")]`. Non-Linux targets compile a stub `main` printing a diagnostic message.
- `KdeTray` (`id = "codex-tray-kde"`, `MENU_ON_ACTIVATE = true`):
  - **Menu**: disabled `status_text`, `"Atualizar agora"` (sets `"Atualizando uso do Codex…"` and calls `UsageMonitorController::refresh()`), separator, and `"Sair"` (`std::process::exit(0)`).
  - **Status**: `Status::Active` normally, `Status::NeedsAttention` on error.
  - **Icon**: converts 32x32 RGBA buffer to ARGB via `pixel.rotate_right(1)` for `ksni::Icon`; draws white text on snapshots and red `[255, 80, 80, 255]` `"!"` / `"!"` on error.

## Windows (`apps/codex-tray-windows`)

- Uses `tao` (`0.34`) and `tray-icon` (`0.25`) as unconditional dependencies in `apps/codex-tray-windows/Cargo.toml`.
- Sets `#![cfg_attr(all(target_os = "windows", not(debug_assertions)), windows_subsystem = "windows")]` so release builds do not open a console window.
- **Menu**: disabled `status_item`, separator, and `"Sair"` (`*control_flow = ControlFlow::Exit`).
- **Icon**: 32x32 RGBA (`Icon::from_rgba(rgba, 32, 32)`) with white text and dark shadow (draws `"!"` / `"!"` on error).

## 32x32 Bitmap Icon Rendering

- Both apps render two centered rows (`start_y = 1` for primary, `start_y = 17` for secondary) using a `3x5` bitmap font (`0`–`9`, `/`, `!`, and horizontal bar fallback for `—`).
- `row_layout(glyph_count)`: `(scale_x = 5, stride = 15)` for `<= 2` glyphs, `(3, 9)` for `>= 3` glyphs (`scale_y = 3`).
- Foreground text is drawn over a `[0, 0, 0, 220]` drop shadow offset by `(+1, +1)`.

## Current Implementation vs. `ARCHITECTURE.md`

1. **No percentage color thresholds**: `ARCHITECTURE.md` describes green (`<70%`), yellow (`70–89%`), and red (`>=90%`) icon states; currently only white text (plus red `"!"`/`"!"` on KDE error) is implemented.
2. **Duplicated icon helpers**: `usage_icon`, `draw_icon_row`, `row_layout`, `draw_glyphs`, and `glyph` are duplicated between `apps/codex-tray-kde/src/main.rs` and `apps/codex-tray-windows/src/main.rs`.
3. **Windows menu parity**: `codex-tray-windows` does not yet expose `"Atualizar agora"` (KDE does).
4. **Push notifications**: `account/rateLimits/updated` server notifications are not handled yet; updates happen via polling and manual refresh.
