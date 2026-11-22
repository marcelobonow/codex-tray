#![cfg_attr(
    all(target_os = "windows", not(debug_assertions)),
    windows_subsystem = "windows"
)]

use std::{
    error::Error,
    process::{Command, Stdio},
    thread,
    time::SystemTime,
};

use codex_usage_core::{
    BackendProvider, MonitorConfig, MonitorUpdate, UsageMonitor, UsageSnapshot, default_log_path,
    install_provider_cli, log_info,
};
use tao::{
    event::Event,
    event_loop::{ControlFlow, EventLoopBuilder},
};
use tray_icon::{
    Icon, TrayIconBuilder,
    menu::{CheckMenuItem, Menu, MenuEvent, MenuItem, PredefinedMenuItem, Submenu},
};

enum UserEvent {
    Monitor(MonitorUpdate),
    Menu(MenuEvent),
}

fn main() -> Result<(), Box<dyn Error>> {
    let config = MonitorConfig::default();
    let mut active_provider = config.provider;
    let initial_status = format!("Consultando uso de {}…", active_provider.display_name());
    let initial_tooltip = format!(
        "Codex Tray — {}\n{initial_status}",
        active_provider.display_name()
    );
    eprintln!(
        "[Codex Tray] Iniciado. Consultando limites de {}…",
        active_provider.display_name()
    );

    let event_loop = EventLoopBuilder::<UserEvent>::with_user_event().build();
    let event_proxy = event_loop.create_proxy();
    let menu_proxy = event_proxy.clone();
    MenuEvent::set_event_handler(Some(move |event| {
        let _ = menu_proxy.send_event(UserEvent::Menu(event));
    }));

    let menu = Menu::new();
    let status_item = MenuItem::new(&initial_status, false, None);
    let install_item = MenuItem::new("Instalar antigravity-usage (npm)", false, None);
    let refresh_item = MenuItem::new("Atualizar agora", true, None);
    let provider_submenu = Submenu::new("Provedor", true);
    let provider_items: Vec<(BackendProvider, CheckMenuItem)> = BackendProvider::ALL
        .iter()
        .map(|&provider| {
            let item = CheckMenuItem::new(
                provider.display_name(),
                true,
                provider == active_provider,
                None,
            );
            (provider, item)
        })
        .collect();
    for (_, item) in &provider_items {
        provider_submenu.append(item)?;
    }
    let logs_item = MenuItem::new("Abrir logs", true, None);
    let separator = PredefinedMenuItem::separator();
    let quit_item = MenuItem::new("Sair", true, None);
    menu.append(&status_item)?;
    menu.append(&install_item)?;
    menu.append(&refresh_item)?;
    menu.append(&provider_submenu)?;
    menu.append(&logs_item)?;
    menu.append(&separator)?;
    menu.append(&quit_item)?;

    let tray = TrayIconBuilder::new()
        .with_id("codex-tray-windows")
        .with_menu(Box::new(menu))
        .with_menu_on_left_click(false)
        .with_tooltip(&initial_tooltip)
        .with_icon(usage_icon("—", "—"))
        .build()?;

    let monitor_proxy = event_proxy.clone();
    let monitor = UsageMonitor::start(config, move |update| {
        let _ = monitor_proxy.send_event(UserEvent::Monitor(update));
    })?;

    event_loop.run(move |event, _, control_flow| {
        *control_flow = ControlFlow::Wait;

        match event {
            Event::UserEvent(UserEvent::Monitor(MonitorUpdate::Snapshot {
                provider,
                snapshot,
            })) if provider == active_provider => {
                eprintln!(
                    "[Codex Tray] Uso atualizado ({}): {}",
                    active_provider.display_name(),
                    snapshot.summary()
                );
                install_item.set_enabled(false);
                apply_snapshot(&tray, &status_item, active_provider, &snapshot);
            }
            Event::UserEvent(UserEvent::Monitor(MonitorUpdate::Error { provider, message }))
                if provider == active_provider =>
            {
                eprintln!(
                    "[Codex Tray] Erro ao consultar uso ({}): {message}",
                    active_provider.display_name()
                );
                if let Some(pkg) = provider
                    .default_cli_package()
                    .filter(|pkg| message.contains(pkg) && message.contains("não foi encontrado"))
                {
                    install_item.set_text(format!("Instalar {pkg} (npm)"));
                    install_item.set_enabled(true);
                } else {
                    install_item.set_enabled(false);
                }
                status_item.set_text(&message);
                let tooltip = format!("Codex Tray — {}\n{message}", active_provider.display_name());
                if let Err(error) = tray.set_tooltip(Some(&tooltip)) {
                    eprintln!("[Codex Tray] Erro ao atualizar tooltip: {error}");
                }
                if let Err(error) = tray.set_icon(Some(usage_icon("!", "!"))) {
                    eprintln!("[Codex Tray] Erro ao atualizar ícone: {error}");
                }
            }
            Event::UserEvent(UserEvent::Menu(event)) if event.id() == quit_item.id() => {
                *control_flow = ControlFlow::Exit;
            }
            Event::UserEvent(UserEvent::Menu(event)) if event.id() == install_item.id() => {
                if let Some(package) = active_provider.default_cli_package() {
                    let provider = active_provider;
                    install_item.set_enabled(false);
                    status_item.set_text(format!("Instalando {package} via npm…"));
                    let controller = monitor.controller();
                    let install_proxy = event_proxy.clone();
                    let _ = thread::Builder::new()
                        .name("codex-tray-cli-install".to_owned())
                        .spawn(move || match install_provider_cli(provider) {
                            Ok(()) => controller.refresh(),
                            Err(_) => {
                                let _ = install_proxy.send_event(UserEvent::Monitor(
                                    MonitorUpdate::Error {
                                        provider,
                                        message: format!(
                                            "Falha ao instalar `{package}` via npm. Execute `npm install -g {package}` no terminal."
                                        ),
                                    },
                                ));
                            }
                        });
                }
            }
            Event::UserEvent(UserEvent::Menu(event)) if event.id() == refresh_item.id() => {
                let status = format!("Atualizando uso de {}…", active_provider.display_name());
                status_item.set_text(&status);
                monitor.refresh();
            }
            Event::UserEvent(UserEvent::Menu(event)) if event.id() == logs_item.id() => {
                open_log_file();
            }
            Event::UserEvent(UserEvent::Menu(event)) => {
                if let Some(&(selected, _)) = provider_items
                    .iter()
                    .find(|(_, item)| event.id() == item.id())
                {
                    active_provider = selected;
                    install_item.set_enabled(false);
                    for (provider, item) in &provider_items {
                        item.set_checked(*provider == active_provider);
                    }
                    let status = format!("Consultando uso de {}…", active_provider.display_name());
                    status_item.set_text(&status);
                    let tooltip =
                        format!("Codex Tray — {}\n{status}", active_provider.display_name());
                    if let Err(error) = tray.set_tooltip(Some(&tooltip)) {
                        eprintln!("[Codex Tray] Erro ao atualizar tooltip: {error}");
                    }
                    if let Err(error) = tray.set_icon(Some(usage_icon("—", "—"))) {
                        eprintln!("[Codex Tray] Erro ao atualizar ícone: {error}");
                    }
                    monitor.set_provider(active_provider);
                }
            }
            _ => {}
        }
    });
}

fn open_log_file() {
    log_info("opening log file from tray menu");
    if let Some(path) = default_log_path() {
        let mut command = Command::new("cmd");
        command
            .args(["/C", "start", ""])
            .arg(path)
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null());
        #[cfg(all(target_os = "windows", not(debug_assertions)))]
        {
            use std::os::windows::process::CommandExt;

            const CREATE_NO_WINDOW: u32 = 0x0800_0000;
            command.creation_flags(CREATE_NO_WINDOW);
        }
        let _ = command.spawn();
    }
}

fn apply_snapshot(
    tray: &tray_icon::TrayIcon,
    status_item: &MenuItem,
    provider: BackendProvider,
    snapshot: &UsageSnapshot,
) {
    let now = SystemTime::now();
    status_item.set_text(snapshot.menu_status_lines(now).join("  |  "));
    let tooltip = format!(
        "Codex Tray — {}\n{}",
        provider.display_name(),
        snapshot.tooltip(now)
    );
    if let Err(error) = tray.set_tooltip(Some(&tooltip)) {
        eprintln!("[Codex Tray] Erro ao atualizar tooltip: {error}");
    }
    let (five_hours, weekly) = snapshot.icon_usage_rows();
    if let Err(error) = tray.set_icon(Some(usage_icon(&five_hours, &weekly))) {
        eprintln!("[Codex Tray] Erro ao atualizar ícone: {error}");
    }
}

fn usage_icon(top: &str, bottom: &str) -> Icon {
    let mut rgba = vec![0; 32 * 32 * 4];
    draw_icon_row(&mut rgba, top, 1);
    draw_icon_row(&mut rgba, bottom, 17);

    Icon::from_rgba(rgba, 32, 32).expect("generated tray icon is valid")
}

fn draw_icon_row(rgba: &mut [u8], text: &str, start_y: usize) {
    let glyphs: Vec<[u8; 5]> = text.chars().map(glyph).collect();
    let (scale_x, glyph_stride) = row_layout(glyphs.len());
    let layout = (scale_x, 3, glyph_stride);
    let width = glyphs.len().saturating_mul(glyph_stride);
    let start_x = (32usize.saturating_sub(width)) / 2;

    draw_glyphs(
        rgba,
        &glyphs,
        start_x.saturating_add(1),
        start_y.saturating_add(1),
        layout,
        [0, 0, 0, 220],
    );
    draw_glyphs(
        rgba,
        &glyphs,
        start_x,
        start_y,
        layout,
        [255, 255, 255, 255],
    );
}

fn row_layout(glyph_count: usize) -> (usize, usize) {
    if glyph_count <= 2 { (5, 15) } else { (3, 9) }
}

fn draw_glyphs(
    rgba: &mut [u8],
    glyphs: &[[u8; 5]],
    start_x: usize,
    start_y: usize,
    layout: (usize, usize, usize),
    color: [u8; 4],
) {
    let (scale_x, scale_y, glyph_stride) = layout;
    for (index, rows) in glyphs.iter().enumerate() {
        for (y, row) in rows.iter().enumerate() {
            for x in 0..3 {
                if row & (1 << (2 - x)) == 0 {
                    continue;
                }
                for pixel_y in 0..scale_y {
                    for pixel_x in 0..scale_x {
                        let px = start_x + index * glyph_stride + x * scale_x + pixel_x;
                        let py = start_y + y * scale_y + pixel_y;
                        if px < 32 && py < 32 {
                            let offset = (py * 32 + px) * 4;
                            rgba[offset..offset + 4].copy_from_slice(&color);
                        }
                    }
                }
            }
        }
    }
}

fn glyph(character: char) -> [u8; 5] {
    match character {
        '0' => [0b111, 0b101, 0b101, 0b101, 0b111],
        '1' => [0b010, 0b110, 0b010, 0b010, 0b111],
        '2' => [0b111, 0b001, 0b111, 0b100, 0b111],
        '3' => [0b111, 0b001, 0b111, 0b001, 0b111],
        '4' => [0b101, 0b101, 0b111, 0b001, 0b001],
        '5' => [0b111, 0b100, 0b111, 0b001, 0b111],
        '6' => [0b111, 0b100, 0b111, 0b101, 0b111],
        '7' => [0b111, 0b001, 0b010, 0b010, 0b010],
        '8' => [0b111, 0b101, 0b111, 0b101, 0b111],
        '9' => [0b111, 0b101, 0b111, 0b001, 0b111],
        '/' => [0b001, 0b001, 0b010, 0b100, 0b100],
        '!' => [0b010, 0b010, 0b010, 0b000, 0b010],
        _ => [0b000, 0b000, 0b111, 0b000, 0b000],
    }
}

#[cfg(test)]
mod tests {
    use super::row_layout;

    #[test]
    fn makes_two_digit_rows_wider_than_three_digit_rows() {
        assert_eq!(row_layout(2), (5, 15));
        assert_eq!(row_layout(3), (3, 9));
    }
}
