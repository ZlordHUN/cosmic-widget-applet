// SPDX-License-Identifier: MPL-2.0

//! COSMIC Monitor Settings - Configuration GUI Entry Point
//!
//! This is the entry point for the standalone settings application that allows
//! users to configure all aspects of the COSMIC Monitor widget.
//!
//! # Binary
//!
//! This compiles to `cosmic-widget-settings` and is typically installed to
//! `/usr/bin/` or `~/.local/bin/`. It's launched via:
//! - The panel applet's "Configure" button
//! - The `.desktop` file in applications menu
//! - Direct command line invocation
//!
//! # Features
//!
//! The settings app provides a comprehensive GUI for:
//! - Toggling monitoring sections (CPU, Memory, GPU, etc.)
//! - Configuring weather API credentials
//! - Setting notification preferences
//! - Adjusting widget position (with live drag support)
//! - Reordering widget sections
//! - Enabling/disabling debug logging
//!
//! # Architecture
//!
//! Unlike the panel applet which uses `cosmic::applet`, this uses the full
//! `cosmic::app` framework for a standalone window. Changes are saved to
//! the shared cosmic-config and immediately visible to the widget.

mod config;
mod i18n;
mod settings;

/// Settings application entry point.
///
/// Initializes i18n and starts the COSMIC application event loop
/// with the SettingsApp model defined in `settings.rs`.
fn main() -> cosmic::iced::Result {
    // Initialize internationalization with system language preferences.
    // This loads translations from i18n/en/cosmic_widget_applet.ftl (and other locales).
    let requested_languages = i18n_embed::DesktopLanguageRequester::requested_languages();
    i18n::init(&requested_languages);

    // Start the iced-based settings application.
    // - Settings::default() provides standard window configuration
    // - () is the flags parameter (no initialization data needed)
    cosmic::app::run::<settings::SettingsApp>(cosmic::app::Settings::default(), ())
}
