#[cfg(target_os = "linux")]
mod linux {
    use std::{
        error::Error,
        process::{Command, Stdio},
        sync::mpsc,
        thread,
        time::SystemTime,
    };

    use codex_usage_core::{
        BackendProvider, MonitorConfig, MonitorUpdate, UsageMonitor, UsageMonitorController,
        UsageSnapshot, default_log_path, install_provider_cli, log_info,
    };
    use ksni::{
        Status, ToolTip,
        blocking::TrayMethods,
        menu::{MenuItem, RadioGroup, RadioItem, StandardItem, SubMenu},
    };

    #[derive(Debug)]
    struct KdeTray {
        provider: BackendProvider,
        status_lines: Vec<String>,
        tooltip: String,
        icon_rows: (String, String),
        failed: bool,
        missing_cli: Option<&'static str>,
        installing_cli: bool,
        monitor: UsageMonitorController,
        updates: mpsc::Sender<MonitorUpdate>,
    }

    impl KdeTray {
        fn apply_update(&mut self, update: MonitorUpdate) {
            match update {
                MonitorUpdate::Snapshot { provider, snapshot } if provider == self.provider => {
                    self.apply_snapshot(snapshot);
                }
                MonitorUpdate::Error { provider, message } if provider == self.provider => {
                    self.status_lines = vec![message.clone()];
                    self.tooltip =
                        format!("Codex Tray — {}\n{message}", self.provider.display_name());
                    self.icon_rows = ("!".into(), "!".into());
                    self.failed = true;
                    self.installing_cli = false;
                    self.missing_cli = provider.default_cli_package().filter(|pkg| {
                        message.contains(pkg) && message.contains("não foi encontrado")
                    });
                }
                _ => {}
            }
        }

        fn apply_snapshot(&mut self, snapshot: UsageSnapshot) {
            let now = SystemTime::now();
            self.status_lines = snapshot.menu_status_lines(now);
            self.tooltip = snapshot.tooltip(now);
            self.icon_rows = snapshot.icon_usage_rows();
            self.failed = false;
            self.missing_cli = None;
            self.installing_cli = false;
        }

        fn select_provider(&mut self, next: BackendProvider) {
            self.provider = next;
            self.icon_rows = ("—".into(), "—".into());
            self.failed = false;
            self.missing_cli = None;
            self.installing_cli = false;
            self.status_lines = vec![format!("Consultando uso de {}…", next.display_name())];
            self.tooltip = format!("Codex Tray — {}\nConsultando uso…", next.display_name());
            self.monitor.set_provider(next);
        }

        fn start_cli_install(&mut self, package: &'static str) {
            if self.installing_cli {
                return;
            }
            let provider = self.provider;
            self.installing_cli = true;
            self.status_lines = vec![format!("Instalando {package} via npm…")];
            self.tooltip = format!(
                "Codex Tray — {}\nInstalando {package} via npm…",
                provider.display_name()
            );

            let monitor = self.monitor.clone();
            let updates = self.updates.clone();
            let _ = thread::Builder::new()
                .name("codex-tray-cli-install".to_owned())
                .spawn(move || match install_provider_cli(provider) {
                    Ok(()) => monitor.refresh(),
                    Err(_) => {
                        let _ = updates.send(MonitorUpdate::Error {
                            provider,
                            message: format!(
                                "Falha ao instalar `{package}` via npm. Execute `npm install -g {package}` no terminal."
                            ),
                        });
                    }
                });
        }
    }

    impl ksni::Tray for KdeTray {
        const MENU_ON_ACTIVATE: bool = true;

        fn id(&self) -> String {
            "codex-tray-kde".into()
        }

        fn title(&self) -> String {
            self.status_lines.first().cloned().unwrap_or_default()
        }

        fn icon_name(&self) -> String {
            String::new()
        }

        fn icon_pixmap(&self) -> Vec<ksni::Icon> {
            vec![usage_icon(
                &self.icon_rows.0,
                &self.icon_rows.1,
                self.failed,
            )]
        }

        fn status(&self) -> Status {
            if self.failed {
                Status::NeedsAttention
            } else {
                Status::Active
            }
        }

        fn tool_tip(&self) -> ToolTip {
            ToolTip {
                icon_name: self.icon_name(),
                icon_pixmap: self.icon_pixmap(),
                title: format!("Codex Tray — {}", self.provider.display_name()),
                description: self.tooltip.clone(),
                ..Default::default()
            }
        }

        fn menu(&self) -> Vec<MenuItem<Self>> {
            let selected = BackendProvider::ALL
                .iter()
                .position(|&provider| provider == self.provider)
                .unwrap_or(0);

            let mut items: Vec<MenuItem<Self>> = self
                .status_lines
                .iter()
                .map(|line| {
                    StandardItem {
                        label: line.clone(),
                        enabled: false,
                        ..Default::default()
                    }
                    .into()
                })
                .collect();

            if self.status_lines.len() > 1 {
                items.push(MenuItem::Separator);
            }

            if let Some(package) = self.missing_cli {
                items.push(
                    StandardItem {
                        label: if self.installing_cli {
                            format!("Instalando {package}…")
                        } else {
                            format!("Instalar {package} (npm)")
                        },
                        icon_name: "system-software-install".into(),
                        enabled: !self.installing_cli,
                        activate: Box::new(move |tray: &mut Self| {
                            tray.start_cli_install(package);
                        }),
                        ..Default::default()
                    }
                    .into(),
                );
            }

            items.extend([
                StandardItem {
                    label: "Atualizar agora".into(),
                    activate: Box::new(|tray: &mut Self| {
                        tray.status_lines = vec![format!(
                            "Atualizando uso de {}…",
                            tray.provider.display_name()
                        )];
                        tray.monitor.refresh();
                    }),
                    ..Default::default()
                }
                .into(),
                SubMenu {
                    label: "Provedor".into(),
                    submenu: vec![
                        RadioGroup {
                            selected,
                            select: Box::new(|tray: &mut Self, index: usize| {
                                let Some(&next) = BackendProvider::ALL.get(index) else {
                                    return;
                                };
                                tray.select_provider(next);
                            }),
                            options: BackendProvider::ALL
                                .iter()
                                .map(|provider| RadioItem {
                                    label: provider.display_name().into(),
                                    ..Default::default()
                                })
                                .collect(),
                        }
                        .into(),
                    ],
                    ..Default::default()
                }
                .into(),
                StandardItem {
                    label: "Abrir logs".into(),
                    icon_name: "text-x-log".into(),
                    activate: Box::new(|_| open_log_file()),
                    ..Default::default()
                }
                .into(),
                MenuItem::Separator,
                StandardItem {
                    label: "Sair".into(),
                    icon_name: "application-exit".into(),
                    activate: Box::new(|_| std::process::exit(0)),
                    ..Default::default()
                }
                .into(),
            ]);

            items
        }
    }

    fn open_log_file() {
        log_info("opening log file from tray menu");
        if let Some(path) = default_log_path() {
            let _ = Command::new("xdg-open")
                .arg(path)
                .stdin(Stdio::null())
                .stdout(Stdio::null())
                .stderr(Stdio::null())
                .spawn();
        }
    }

    pub fn run() -> Result<(), Box<dyn Error>> {
        let (updates, receiver) = mpsc::channel();
        let config = MonitorConfig::default();
        let initial_provider = config.provider;
        let monitor_updates = updates.clone();
        let monitor = UsageMonitor::start(config, move |update| {
            let _ = monitor_updates.send(update);
        })?;
        let tray = KdeTray {
            provider: initial_provider,
            status_lines: vec![format!(
                "Consultando uso de {}…",
                initial_provider.display_name()
            )],
            tooltip: format!(
                "Codex Tray — {}\nConsultando uso…",
                initial_provider.display_name()
            ),
            icon_rows: ("—".into(), "—".into()),
            failed: false,
            missing_cli: None,
            installing_cli: false,
            monitor: monitor.controller(),
            updates,
        };
        let handle = tray.spawn()?;

        for update in receiver {
            handle.update(|tray| tray.apply_update(update));
        }

        Ok(())
    }

    fn usage_icon(top: &str, bottom: &str, failed: bool) -> ksni::Icon {
        let mut rgba = vec![0; 32 * 32 * 4];
        draw_icon_row(&mut rgba, top, 1, failed);
        draw_icon_row(&mut rgba, bottom, 17, failed);

        for pixel in rgba.chunks_exact_mut(4) {
            pixel.rotate_right(1);
        }

        ksni::Icon {
            width: 32,
            height: 32,
            data: rgba,
        }
    }

    fn draw_icon_row(rgba: &mut [u8], text: &str, start_y: usize, failed: bool) {
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
            if failed {
                [255, 80, 80, 255]
            } else {
                [255, 255, 255, 255]
            },
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
        use std::time::{Duration, UNIX_EPOCH};

        use codex_usage_core::{CoreError, UsageBackend, UsageBucket, UsageMonitor, UsageWindow};
        use ksni::Tray;

        use super::*;

        struct NoopBackend;

        impl UsageBackend for NoopBackend {
            fn fetch_usage(&mut self) -> Result<UsageSnapshot, CoreError> {
                Ok(UsageSnapshot::default())
            }
        }

        fn make_test_tray(provider: BackendProvider) -> (KdeTray, UsageMonitor) {
            let config = MonitorConfig {
                provider,
                config_path: None,
                poll_interval: Duration::from_secs(60),
                retry_interval: Duration::from_secs(60),
                request_timeout: Duration::from_secs(5),
                ..MonitorConfig::default()
            };
            let monitor =
                UsageMonitor::start_with_factory(config, |_, _| Ok(Box::new(NoopBackend)), |_| {})
                    .expect("test monitor should start");
            let (updates, _rx) = mpsc::channel();
            let tray = KdeTray {
                provider,
                status_lines: vec![format!("Consultando uso de {}…", provider.display_name())],
                tooltip: format!("Codex Tray — {}\nConsultando uso…", provider.display_name()),
                icon_rows: ("—".into(), "—".into()),
                failed: false,
                missing_cli: None,
                installing_cli: false,
                monitor: monitor.controller(),
                updates,
            };
            (tray, monitor)
        }

        fn sample_snapshot(primary_used: f64, secondary_used: f64) -> UsageSnapshot {
            UsageSnapshot {
                buckets: vec![UsageBucket {
                    id: "test".into(),
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
        fn apply_update_ignores_stale_provider_and_applies_active_provider() {
            let (mut tray, mut monitor) = make_test_tray(BackendProvider::Codex);

            tray.apply_update(MonitorUpdate::Snapshot {
                provider: BackendProvider::Claude,
                snapshot: sample_snapshot(88.0, 44.0),
            });
            assert_eq!(tray.icon_rows, ("—".into(), "—".into()));
            assert_eq!(tray.status_lines, vec!["Consultando uso de Codex…"]);
            assert!(!tray.failed);

            tray.apply_update(MonitorUpdate::Error {
                provider: BackendProvider::Antigravity,
                message: "Erro antigo".into(),
            });
            assert_eq!(tray.icon_rows, ("—".into(), "—".into()));
            assert!(!tray.failed);

            tray.apply_update(MonitorUpdate::Snapshot {
                provider: BackendProvider::Codex,
                snapshot: sample_snapshot(42.0, 15.0),
            });
            assert_eq!(tray.icon_rows, ("42".into(), "15".into()));
            assert!(!tray.failed);
            assert_eq!(tray.tool_tip().title, "Codex Tray — Codex");

            tray.apply_update(MonitorUpdate::Error {
                provider: BackendProvider::Codex,
                message: "Falha temporária".into(),
            });
            assert!(tray.failed);
            assert_eq!(tray.icon_rows, ("!".into(), "!".into()));
            assert_eq!(tray.status_lines, vec!["Falha temporária"]);

            monitor.stop();
        }

        #[test]
        fn select_provider_resets_state_and_updates_menu_selection() {
            let (mut tray, mut monitor) = make_test_tray(BackendProvider::Codex);
            tray.failed = true;
            tray.icon_rows = ("!".into(), "!".into());

            tray.select_provider(BackendProvider::CommandCode);

            assert_eq!(tray.provider, BackendProvider::CommandCode);
            assert_eq!(tray.icon_rows, ("—".into(), "—".into()));
            assert!(!tray.failed);
            assert_eq!(tray.status_lines, vec!["Consultando uso de Command Code…"]);
            assert_eq!(tray.tool_tip().title, "Codex Tray — Command Code");

            let menu = tray.menu();
            assert_eq!(menu.len(), 6);
            let MenuItem::SubMenu(submenu) = &menu[2] else {
                panic!("expected SubMenu for Provedor");
            };
            assert_eq!(submenu.label, "Provedor");
            let MenuItem::RadioGroup(group) = &submenu.submenu[0] else {
                panic!("expected RadioGroup inside Provedor submenu");
            };
            assert_eq!(group.selected, 2);
            assert_eq!(group.options.len(), BackendProvider::ALL.len());

            monitor.stop();
        }

        #[test]
        fn apply_update_handles_error_for_active_and_stale_providers() {
            let (mut tray, mut monitor) = make_test_tray(BackendProvider::Codex);

            tray.apply_update(MonitorUpdate::Snapshot {
                provider: BackendProvider::Codex,
                snapshot: sample_snapshot(42.0, 15.0),
            });
            let previous_status = tray.status_lines.clone();
            let previous_tooltip = tray.tooltip.clone();
            assert!(!tray.failed);
            assert_eq!(tray.status(), Status::Active);

            // Stale error from another provider must be ignored completely
            tray.apply_update(MonitorUpdate::Error {
                provider: BackendProvider::Claude,
                message: "Falha antiga do Claude".into(),
            });
            assert!(!tray.failed);
            assert_eq!(tray.status(), Status::Active);
            assert_eq!(tray.icon_rows, ("42".into(), "15".into()));
            assert_eq!(tray.status_lines, previous_status);
            assert_eq!(tray.tooltip, previous_tooltip);
            assert_eq!(tray.tool_tip().title, "Codex Tray — Codex");
            assert_eq!(tray.tool_tip().description, previous_tooltip);

            // Error for the active provider must set failed = true and update status/tooltip/icon
            tray.apply_update(MonitorUpdate::Error {
                provider: BackendProvider::Codex,
                message: "Falha temporária".into(),
            });
            assert!(tray.failed);
            assert_eq!(tray.status(), Status::NeedsAttention);
            assert_eq!(tray.icon_rows, ("!".into(), "!".into()));
            assert_eq!(tray.status_lines, vec!["Falha temporária"]);
            assert_eq!(tray.tooltip, "Codex Tray — Codex\nFalha temporária");
            assert_eq!(tray.tool_tip().title, "Codex Tray — Codex");
            assert_eq!(
                tray.tool_tip().description,
                "Codex Tray — Codex\nFalha temporária"
            );

            // Subsequent valid snapshot for the active provider clears the failure state
            tray.apply_update(MonitorUpdate::Snapshot {
                provider: BackendProvider::Codex,
                snapshot: sample_snapshot(10.0, 5.0),
            });
            assert!(!tray.failed);
            assert_eq!(tray.status(), Status::Active);
            assert_eq!(tray.icon_rows, ("10".into(), "5".into()));

            monitor.stop();
        }

        #[test]
        fn menu_provider_submenu_lists_all_providers_and_radio_callback_switches() {
            let (mut tray, mut monitor) = make_test_tray(BackendProvider::Codex);

            let menu = tray.menu();
            assert_eq!(menu.len(), 6);

            let MenuItem::Standard(status_item) = &menu[0] else {
                panic!("expected disabled status StandardItem");
            };
            assert!(!status_item.enabled);
            assert_eq!(status_item.label, "Consultando uso de Codex…");

            let MenuItem::Standard(refresh_item) = &menu[1] else {
                panic!("expected refresh StandardItem");
            };
            assert_eq!(refresh_item.label, "Atualizar agora");

            let MenuItem::SubMenu(submenu) = &menu[2] else {
                panic!("expected SubMenu for Provedor");
            };
            assert_eq!(submenu.label, "Provedor");
            let MenuItem::RadioGroup(group) = &submenu.submenu[0] else {
                panic!("expected RadioGroup inside Provedor submenu");
            };
            assert_eq!(group.selected, 0);
            let labels: Vec<&str> = group.options.iter().map(|opt| opt.label.as_str()).collect();
            assert_eq!(
                labels,
                vec!["Codex", "Claude", "Command Code", "Antigravity"]
            );

            let MenuItem::Standard(logs_item) = &menu[3] else {
                panic!("expected logs StandardItem");
            };
            assert_eq!(logs_item.label, "Abrir logs");

            assert!(matches!(menu[4], MenuItem::Separator));
            let MenuItem::Standard(quit_item) = &menu[5] else {
                panic!("expected quit StandardItem");
            };
            assert_eq!(quit_item.label, "Sair");

            // Selecting an out-of-bounds index is a safe no-op
            (group.select)(&mut tray, 99);
            assert_eq!(tray.provider, BackendProvider::Codex);

            // Selecting Claude via RadioGroup callback updates active provider and state
            (group.select)(&mut tray, 1);
            assert_eq!(tray.provider, BackendProvider::Claude);
            assert_eq!(tray.status_lines, vec!["Consultando uso de Claude…"]);
            assert_eq!(tray.tool_tip().title, "Codex Tray — Claude");

            let updated_menu = tray.menu();
            let MenuItem::SubMenu(updated_submenu) = &updated_menu[2] else {
                panic!("expected SubMenu for Provedor");
            };
            let MenuItem::RadioGroup(updated_group) = &updated_submenu.submenu[0] else {
                panic!("expected RadioGroup inside Provedor submenu");
            };
            assert_eq!(updated_group.selected, 1);

            monitor.stop();
        }

        #[test]
        fn multi_group_snapshot_shows_gemini_in_preview_and_all_groups_in_menu() {
            let (mut tray, mut monitor) = make_test_tray(BackendProvider::Antigravity);
            let snapshot = UsageSnapshot {
                buckets: vec![
                    UsageBucket {
                        id: "gemini".into(),
                        label: Some("Gemini Models".into()),
                        plan_type: None,
                        primary: UsageWindow {
                            used_percent: 8.0,
                            window_duration: Duration::from_secs(5 * 3600),
                            resets_at: UNIX_EPOCH + Duration::from_secs(1_744_372_800),
                        },
                        secondary: Some(UsageWindow {
                            used_percent: 7.0,
                            window_duration: Duration::from_secs(7 * 86_400),
                            resets_at: UNIX_EPOCH + Duration::from_secs(1_744_804_800),
                        }),
                    },
                    UsageBucket {
                        id: "claude-gpt".into(),
                        label: Some("Claude and GPT models".into()),
                        plan_type: None,
                        primary: UsageWindow {
                            used_percent: 0.0,
                            window_duration: Duration::from_secs(5 * 3600),
                            resets_at: UNIX_EPOCH + Duration::from_secs(1_744_372_800),
                        },
                        secondary: Some(UsageWindow {
                            used_percent: 0.0,
                            window_duration: Duration::from_secs(7 * 86_400),
                            resets_at: UNIX_EPOCH + Duration::from_secs(1_744_804_800),
                        }),
                    },
                    UsageBucket {
                        id: "argon".into(),
                        label: Some("Gemini 4 Argon".into()),
                        plan_type: None,
                        primary: UsageWindow {
                            used_percent: 0.0,
                            window_duration: Duration::from_secs(7 * 86_400),
                            resets_at: UNIX_EPOCH + Duration::from_secs(1_744_804_800),
                        },
                        secondary: None,
                    },
                ],
            };

            tray.apply_update(MonitorUpdate::Snapshot {
                provider: BackendProvider::Antigravity,
                snapshot,
            });

            // Preview (icon + tooltip) shows only Gemini Models
            assert_eq!(tray.icon_rows, ("8".into(), "7".into()));
            assert_eq!(tray.tooltip, "Uso 5 horas: 8% / Semanal: 7%");

            // Right-click menu lists all 3 groups separately followed by a separator
            let menu = tray.menu();
            assert_eq!(menu.len(), 9);
            let MenuItem::Standard(g0) = &menu[0] else {
                panic!("expected first group item");
            };
            let MenuItem::Standard(g1) = &menu[1] else {
                panic!("expected second group item");
            };
            let MenuItem::Standard(g2) = &menu[2] else {
                panic!("expected third group item");
            };
            assert!(g0.label.starts_with("Gemini Models: "));
            assert!(g1.label.starts_with("Claude and GPT models: "));
            assert!(g2.label.starts_with("Gemini 4 Argon: "));
            assert!(!g0.enabled && !g1.enabled && !g2.enabled);
            assert!(matches!(menu[3], MenuItem::Separator));

            monitor.stop();
        }
    }
}

#[cfg(target_os = "linux")]
fn main() -> Result<(), Box<dyn std::error::Error>> {
    linux::run()
}

#[cfg(not(target_os = "linux"))]
fn main() {
    eprintln!("codex-tray-kde deve ser executado em uma sessão KDE Plasma no Linux.");
}
