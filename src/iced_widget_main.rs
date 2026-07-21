// SPDX-License-Identifier: MPL-2.0

//! Experimental libcosmic/Iced desktop overlay.

#[path = "widget/battery.rs"]
mod battery;
#[path = "widget/cache.rs"]
mod cache;
mod config;
#[path = "widget/disk_io.rs"]
mod disk_io;
mod iced_widget;
#[path = "widget/media.rs"]
mod media;
#[path = "widget/network.rs"]
mod network;
#[path = "widget/notifications.rs"]
mod notifications;
#[path = "widget/nvidia.rs"]
mod nvidia;
#[path = "widget/storage.rs"]
mod storage;
#[path = "widget/temperature.rs"]
mod temperature;
#[path = "widget/utilization.rs"]
mod utilization;
#[path = "widget/weather.rs"]
mod weather;
mod widget_instance;
mod widget_logging;

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let _instance_guard = match widget_instance::try_acquire()? {
        Some(guard) => guard,
        None => {
            eprintln!("cosmic-widget is already running; skipping duplicate launch");
            return Ok(());
        }
    };

    use cosmic::cosmic_config::CosmicConfigEntry;
    let logging_enabled = cosmic::cosmic_config::Config::new(
        "com.github.zoliviragh.CosmicWidget",
        config::Config::VERSION,
    )
    .ok()
    .and_then(|handler| config::Config::get_entry(&handler).ok())
    .is_some_and(|config| config.enable_logging);
    widget_logging::init(logging_enabled);
    log::info!("Starting COSMIC Widget Iced overlay");

    iced_widget::run()?;
    Ok(())
}
