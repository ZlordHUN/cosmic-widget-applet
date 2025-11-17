// SPDX-License-Identifier: MPL-2.0

mod app;
mod config;
mod i18n;

fn main() -> cosmic::iced::Result {
    // Initialize logger to write to /tmp/cosmic-monitor.log (shared with widget)
    use std::fs::OpenOptions;
    
    let log_file = OpenOptions::new()
        .create(true)
        .append(true)
        .open("/tmp/cosmic-monitor.log")
        .expect("Failed to open log file");
    
    env_logger::Builder::from_env(env_logger::Env::default().default_filter_or("info"))
        .target(env_logger::Target::Pipe(Box::new(log_file)))
        .init();
    
    log::info!("Starting COSMIC Monitor Applet");
    
    // Get the system's preferred languages.
    let requested_languages = i18n_embed::DesktopLanguageRequester::requested_languages();

    // Enable localizations to be applied.
    i18n::init(&requested_languages);

    // Starts the applet's event loop with `()` as the application's flags.
    cosmic::applet::run::<app::AppModel>(())
}
