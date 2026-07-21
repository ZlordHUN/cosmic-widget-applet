// SPDX-License-Identifier: MPL-2.0

//! Runtime-switchable file logging for the Iced overlay.

use chrono::Local;
use log::{LevelFilter, Log, Metadata, Record};
use std::fs::{File, OpenOptions};
use std::io::Write;
use std::sync::Mutex;
use std::sync::atomic::{AtomicBool, Ordering};

const LOG_PATH: &str = "/tmp/cosmic-widget.log";

static LOGGER: OverlayLogger = OverlayLogger {
    enabled: AtomicBool::new(false),
    file: Mutex::new(None),
};

struct OverlayLogger {
    enabled: AtomicBool,
    file: Mutex<Option<File>>,
}

impl Log for OverlayLogger {
    fn enabled(&self, metadata: &Metadata<'_>) -> bool {
        self.enabled.load(Ordering::Relaxed)
            && metadata.level() <= log::Level::Debug
            && is_overlay_target(metadata.target())
    }

    fn log(&self, record: &Record<'_>) {
        if !self.enabled(record.metadata()) {
            return;
        }

        let Ok(mut file) = self.file.lock() else {
            return;
        };
        if let Some(file) = file.as_mut() {
            let _ = writeln!(
                file,
                "{} [{}] {}: {}",
                Local::now().format("%Y-%m-%d %H:%M:%S%.3f"),
                record.level(),
                record.target(),
                record.args()
            );
        }
    }

    fn flush(&self) {
        if let Ok(mut file) = self.file.lock()
            && let Some(file) = file.as_mut()
        {
            let _ = file.flush();
        }
    }
}

fn is_overlay_target(target: &str) -> bool {
    target == "cosmic_widget_iced" || target.starts_with("cosmic_widget_iced::")
}

pub fn init(enabled: bool) {
    let _ = log::set_logger(&LOGGER);
    log::set_max_level(LevelFilter::Debug);
    set_enabled(enabled);
}

pub fn set_enabled(enabled: bool) {
    let Ok(mut file) = LOGGER.file.lock() else {
        return;
    };

    if enabled && file.is_none() {
        *file = OpenOptions::new()
            .create(true)
            .append(true)
            .open(LOG_PATH)
            .ok();
    } else if !enabled {
        *file = None;
    }
    LOGGER
        .enabled
        .store(enabled && file.is_some(), Ordering::Relaxed);
}

#[cfg(test)]
mod tests {
    use super::is_overlay_target;

    #[test]
    fn logging_keeps_overlay_targets_and_rejects_dependencies() {
        assert!(is_overlay_target("cosmic_widget_iced"));
        assert!(is_overlay_target("cosmic_widget_iced::media"));
        assert!(!is_overlay_target("cosmic_config"));
        assert!(!is_overlay_target("reqwest::connect"));
    }
}
