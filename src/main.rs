// SPDX-License-Identifier: MPL-2.0

//! COSMIC Monitor Applet - Panel Integration Entry Point
//!
//! This is the main entry point for the **panel applet** component of COSMIC Monitor.
//! The applet runs inside the COSMIC panel and provides:
//! - A clickable tray icon that spawns the standalone widget
//! - Quick access to the settings application
//!
//! # Component Overview
//!
//! COSMIC Monitor consists of three separate binaries:
//! 1. **cosmic-widget-applet** (this binary): Panel integration
//! 2. **cosmic-widget**: Standalone desktop widget (see `widget_main.rs`)
//! 3. **cosmic-widget-settings**: Configuration GUI (see `settings_main.rs`)
//!
//! # Architecture
//!
//! The applet uses the `cosmic::applet` framework which provides:
//! - Automatic integration with the COSMIC panel
//! - Proper styling that matches the system theme
//! - Popup window support for menus
//!
//! The actual system monitoring happens in the widget process, not here.
//! This separation allows the widget to use Wayland layer-shell for positioning
//! while the applet stays integrated with the panel.

mod app;
mod config;
mod i18n;

/// Panel applet entry point.
///
/// Initializes logging and internationalization, then starts the iced event loop
/// for the panel applet. The applet itself is defined in `app.rs`.
fn main() -> cosmic::iced::Result {
    // Initialize logger to write to /tmp/cosmic-widget.log
    // This log file is shared with the widget process for unified debugging.
    // Note: Logging is always enabled for the applet (it's lightweight).
    use std::fs::OpenOptions;
    
    let log_file = OpenOptions::new()
        .create(true)      // Create file if it doesn't exist
        .append(true)      // Append to existing content (don't truncate)
        .open("/tmp/cosmic-widget.log")
        .expect("Failed to open log file");
    
    env_logger::Builder::from_env(env_logger::Env::default().default_filter_or("info"))
        .target(env_logger::Target::Pipe(Box::new(log_file)))
        .init();
    
    log::info!("Starting COSMIC Monitor Applet");
    
    // Initialize internationalization (i18n) support.
    // Uses the system's preferred language list to select the appropriate
    // translation files from the i18n/ directory.
    let requested_languages = i18n_embed::DesktopLanguageRequester::requested_languages();
    i18n::init(&requested_languages);

    // Start the applet's iced event loop.
    // The `()` parameter means we're not passing any initialization flags.
    // AppModel (defined in app.rs) handles all UI and message processing.
    cosmic::applet::run::<app::AppModel>(())
}
