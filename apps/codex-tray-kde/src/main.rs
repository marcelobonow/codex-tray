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
        icon_rows: (String, String),
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
                    self.icon_rows = ("!".into(), "!".into());
                    self.failed = true;
                }
            }
        }

        fn apply_snapshot(&mut self, snapshot: UsageSnapshot) {
            self.status_text = snapshot.menu_status(SystemTime::now());
            self.tooltip = snapshot.tooltip(SystemTime::now());
            self.icon_rows = snapshot.icon_usage_rows();
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
            icon_rows: ("—".into(), "—".into()),
            failed: false,
            monitor: monitor.controller(),
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
}

#[cfg(target_os = "linux")]
fn main() -> Result<(), Box<dyn std::error::Error>> {
    linux::run()
}

#[cfg(not(target_os = "linux"))]
fn main() {
    eprintln!("codex-tray-kde deve ser executado em uma sessão KDE Plasma no Linux.");
}
