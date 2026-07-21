// SPDX-License-Identifier: MPL-2.0

//! Experimental libcosmic/Iced desktop overlay.

#[path = "widget/battery.rs"]
mod battery;
#[path = "widget/cache.rs"]
mod cache;
mod config;
mod iced_widget;
#[path = "widget/media.rs"]
mod media;
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

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let _instance_guard = match widget_instance::try_acquire()? {
        Some(guard) => guard,
        None => {
            eprintln!("cosmic-widget is already running; skipping duplicate launch");
            return Ok(());
        }
    };

    iced_widget::run()?;
    Ok(())
}
