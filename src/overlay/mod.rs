// SPDX-License-Identifier: MPL-2.0

//! Desktop overlay built with libcosmic and Iced.
//!
//! Monitoring is shared with the legacy renderer through `crate::monitors`.
//! This module owns presentation, animation, and the Wayland surface.

mod animation;
mod app;
mod components;
mod layout;
mod media_state;
mod notification_state;
mod sections;
mod stats;
mod surface;
mod ticks;
mod view;

#[cfg(test)]
mod tests;

use app::Message;

/// Start a single desktop overlay with the configured logging preference.
pub fn run() -> Result<(), Box<dyn std::error::Error>> {
    let _instance_guard = match crate::runtime::instance::try_acquire()? {
        Some(guard) => guard,
        None => {
            eprintln!("cosmic-widget is already running; skipping duplicate launch");
            return Ok(());
        }
    };

    use cosmic::cosmic_config::CosmicConfigEntry;
    let logging_enabled = cosmic::cosmic_config::Config::new(
        "com.github.zoliviragh.CosmicWidget",
        crate::config::Config::VERSION,
    )
    .ok()
    .and_then(|handler| crate::config::Config::get_entry(&handler).ok())
    .is_some_and(|config| config.enable_logging);
    crate::runtime::logging::init(logging_enabled);
    log::info!("Starting COSMIC Widget Iced overlay");

    app::run()?;
    Ok(())
}
