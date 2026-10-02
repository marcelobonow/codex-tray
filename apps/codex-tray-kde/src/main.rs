#[cfg(target_os = "linux")]
mod linux {
    use std::{error::Error, sync::mpsc, time::SystemTime};

    use codex_usage_core::{
        MonitorConfig, MonitorUpdate, UsageMonitor, UsageMonitorController, UsageSnapshot,
    };
    use ksni::{
        Status, ToolTip,
        blocking::TrayMethods,
        menu::{MenuItem, StandardItem},
    };

    #[derive(Debug)]
    struct KdeTray {
        status_text: String,
        tooltip: String,
        failed: bool,
        monitor: UsageMonitorController,
    }

    impl KdeTray {
        fn apply_update(&mut self, update: MonitorUpdate) {
            match update {
                MonitorUpdate::Snapshot(snapshot) => self.apply_snapshot(snapshot),
                MonitorUpdate::Error(message) => {
                    self.status_text = message.clone();
                    self.tooltip = format!("Codex Tray\n{message}");
                    self.failed = true;
                }
            }
        }

        fn apply_snapshot(&mut self, snapshot: UsageSnapshot) {
            self.status_text = snapshot.menu_status(SystemTime::now());
            self.tooltip = snapshot.tooltip(SystemTime::now());
            self.failed = false;
        }
    }

    impl ksni::Tray for KdeTray {
        const MENU_ON_ACTIVATE: bool = true;

        fn id(&self) -> String {
            "codex-tray-kde".into()
        }

        fn title(&self) -> String {
            self.status_text.clone()
        }

        fn icon_name(&self) -> String {
            if self.failed {
                "dialog-error".into()
            } else {
                "utilities-terminal".into()
            }
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
                title: "Codex Tray".into(),
                description: self.tooltip.clone(),
                ..Default::default()
            }
        }

        fn menu(&self) -> Vec<MenuItem<Self>> {
            vec![
                StandardItem {
                    label: self.status_text.clone(),
                    enabled: false,
                    ..Default::default()
                }
                .into(),
                StandardItem {
                    label: "Atualizar agora".into(),
                    activate: Box::new(|tray: &mut Self| {
                        tray.status_text = "Atualizando uso do Codex…".into();
                        tray.monitor.refresh();
                    }),
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
            ]
        }
    }

    pub fn run() -> Result<(), Box<dyn Error>> {
        let (updates, receiver) = mpsc::channel();
        let monitor = UsageMonitor::start(MonitorConfig::default(), move |update| {
            let _ = updates.send(update);
        })?;
        let tray = KdeTray {
            status_text: "Consultando uso do Codex…".into(),
            tooltip: "Codex Tray\nConsultando uso…".into(),
            failed: false,
            monitor: monitor.controller(),
        };
        let handle = tray.spawn()?;

        for update in receiver {
            handle.update(|tray| tray.apply_update(update));
        }

        Ok(())
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
