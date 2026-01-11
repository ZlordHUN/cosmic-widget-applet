// SPDX-License-Identifier: MPL-2.0

//! Internationalization (i18n) Support for COSMIC Monitor
//!
//! This module provides localization support using the Fluent translation system.
//! Translations are stored in the `i18n/` directory in `.ftl` (Fluent) files.
//!
//! # Directory Structure
//!
//! ```text
//! i18n/
//! ├── en/
//! │   └── cosmic_widget_applet.ftl   # English translations (fallback)
//! └── de/                              # German translations (example)
//!     └── cosmic_widget_applet.ftl
//! ```
//!
//! # Usage
//!
//! Use the `fl!()` macro to request localized strings:
//!
//! ```rust
//! use crate::fl;
//!
//! // Simple string lookup
//! let title = fl!("app-title");
//!
//! // String with arguments
//! let greeting = fl!("greeting", name = "User");
//! ```
//!
//! # Adding New Translations
//!
//! 1. Create a new directory under `i18n/` with the locale code (e.g., `fr/`)
//! 2. Copy `en/cosmic_widget_applet.ftl` to the new directory
//! 3. Translate the strings in the new file
//!
//! The system automatically selects the best available translation based on
//! the user's system language preferences.

use std::sync::LazyLock;

use i18n_embed::{
    fluent::{fluent_language_loader, FluentLanguageLoader},
    unic_langid::LanguageIdentifier,
    DefaultLocalizer, LanguageLoader, Localizer,
};
use rust_embed::RustEmbed;

/// Initialize the localization system with the user's preferred languages.
///
/// This should be called once at application startup, typically in `main()`.
/// The language loader will select the best available translation from the
/// user's preference list, falling back to English if no match is found.
///
/// # Arguments
///
/// * `requested_languages` - List of language identifiers in preference order,
///   typically from `i18n_embed::DesktopLanguageRequester::requested_languages()`
pub fn init(requested_languages: &[LanguageIdentifier]) {
    if let Err(why) = localizer().select(requested_languages) {
        eprintln!("error while loading fluent localizations: {why}");
    }
}

/// Creates a boxed Localizer for this application.
///
/// The localizer handles language selection and string resolution.
#[must_use]
pub fn localizer() -> Box<dyn Localizer> {
    Box::from(DefaultLocalizer::new(&*LANGUAGE_LOADER, &Localizations))
}

/// Embedded localization files from the `i18n/` directory.
///
/// The `RustEmbed` derive macro embeds all `.ftl` files at compile time,
/// so the binary doesn't need external translation files at runtime.
#[derive(RustEmbed)]
#[folder = "i18n/"]
struct Localizations;

/// Global Fluent language loader instance.
///
/// Lazily initialized on first access. Loads the fallback language (English)
/// immediately to ensure strings are always available.
pub static LANGUAGE_LOADER: LazyLock<FluentLanguageLoader> = LazyLock::new(|| {
    let loader: FluentLanguageLoader = fluent_language_loader!();

    // Load English as the fallback language
    loader
        .load_fallback_language(&Localizations)
        .expect("Error while loading fallback language");

    loader
});

/// Request a localized string by ID from the translation files.
///
/// # Examples
///
/// ```rust
/// // Simple string lookup
/// let text = fl!("show-cpu");  // Returns "Show CPU" or translated equivalent
///
/// // String with interpolation arguments
/// let msg = fl!("disk-usage", name = "nvme0n1", percent = 75);
/// ```
///
/// If the requested message ID is not found, the ID itself is returned
/// (useful for development and debugging).
#[macro_export]
macro_rules! fl {
    // Simple message lookup without arguments
    ($message_id:literal) => {{
        i18n_embed_fl::fl!($crate::i18n::LANGUAGE_LOADER, $message_id)
    }};

    // Message lookup with named arguments
    ($message_id:literal, $($args:expr),*) => {{
        i18n_embed_fl::fl!($crate::i18n::LANGUAGE_LOADER, $message_id, $($args), *)
    }};
}

