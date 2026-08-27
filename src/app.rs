// SPDX-License-Identifier: MPL-2.0

//! Panel Applet UI and Logic
//!
//! This module implements the COSMIC panel applet - a small icon in the panel
//! that provides quick access to the monitoring widget and settings.
//!
//! # Features
//!
//! - **Panel Icon**: Displays a system monitor icon (`utilities-system-monitor-symbolic`)
//! - **Popup Menu**: Shows options to show/hide the widget and open settings
//! - **Widget Management**: Spawns and kills the standalone widget process
//! - **Auto-start**: Optionally launches the widget when the applet loads
//!
//! # Architecture
//!
//! The applet uses the `cosmic::Application` trait to integrate with the COSMIC
//! desktop. It maintains minimal state - just tracking whether the widget is
//! running and the current configuration.
//!
//! The actual monitoring widget runs as a separate process (`cosmic-widget`)
//! to allow for layer-shell positioning and independent lifecycle management.

use crate::config::Config;
use crate::fl;
use cosmic::cosmic_config::{self, CosmicConfigEntry};
use cosmic::iced::platform_specific::shell::commands::popup::{destroy_popup, get_popup};
use cosmic::iced::{Limits, Subscription, window::Id};
use cosmic::prelude::*;
use cosmic::widget;
use futures_util::SinkExt;
use std::io;
use std::path::PathBuf;
use std::process::Command;
use std::time::Duration;

const WIDGET_STARTUP_DELAY: Duration = Duration::from_secs(2);
const WIDGET_RETRY_DELAY: Duration = Duration::from_secs(3);
const WIDGET_STARTUP_ATTEMPTS: usize = 5;

// ============================================================================
// Application Model
// ============================================================================

/// Main application state for the panel applet.
///
/// This struct holds all runtime state needed to render the UI and handle
/// user interactions. It's managed by the COSMIC/iced runtime.
#[derive(Default)]
pub struct AppModel {
    /// COSMIC runtime core - provides access to system integration features
    /// like window management, styling, and configuration watching.
    core: cosmic::Core,

    /// Window ID of the currently open popup, if any.
    /// Used to track and close the popup when clicking elsewhere.
    popup: Option<Id>,

    /// Current configuration loaded from cosmic-config.
    /// Updated via subscription when settings change externally.
    config: Config,

    /// Handle to cosmic-config for saving configuration changes.
    /// None if config system failed to initialize.
    config_handler: Option<cosmic_config::Config>,

    /// Whether the widget process is currently running.
    /// Updated when opening the popup and after toggle operations.
    widget_running: bool,
}

// ============================================================================
// Message Types
// ============================================================================

/// Messages that drive the applet's state machine.
///
/// Each variant represents a user action or system event that the applet
/// needs to respond to.
#[derive(Debug, Clone)]
pub enum Message {
    /// User clicked the panel icon - toggle popup visibility.
    TogglePopup,

    /// Popup window was closed (clicked outside or pressed escape).
    PopupClosed(Id),

    /// Subscription channel initialized (used for async setup).
    SubscriptionChannel,

    /// Configuration changed externally (e.g., from settings app).
    UpdateConfig(Config),

    /// User clicked "Show/Hide Widget" in the popup menu.
    ToggleWidget,

    /// User clicked "Configure" in the popup menu.
    OpenSettings,
}

// ============================================================================
// Helper Methods
// ============================================================================

impl AppModel {
    /// Persists the current configuration to disk.
    ///
    /// Called after any configuration change (like toggling auto-start).
    /// Silently logs errors rather than panicking.
    fn save_config(&self) {
        if let Some(ref config_handler) = self.config_handler {
            if let Err(err) = self.config.write_entry(config_handler) {
                eprintln!("Failed to save config: {}", err);
            }
        }
    }

    /// Checks if the widget process is currently running.
    ///
    /// Uses `pgrep` to search for the `cosmic-widget` process.
    /// This is a simple but effective approach that doesn't require
    /// tracking PIDs manually.
    ///
    /// # Returns
    /// `true` if the widget process is found, `false` otherwise.
    fn check_widget_running() -> bool {
        if let Ok(output) = Command::new("pgrep")
            .args(["-r", "R,S,D,T,t,W,I"])
            .arg("-x")
            .arg("cosmic-widget")
            .output()
        {
            // pgrep returns empty output if no matching process found
            !output.stdout.is_empty()
        } else {
            false
        }
    }

    /// Resolves an installed companion binary next to the running applet.
    /// Falling back to PATH keeps development builds and custom installs working.
    fn companion_binary(name: &str) -> PathBuf {
        std::env::current_exe()
            .ok()
            .and_then(|executable| executable.parent().map(|parent| parent.join(name)))
            .filter(|path| path.is_file())
            .unwrap_or_else(|| PathBuf::from(name))
    }

    /// Spawns a child and waits for it on a background thread so it cannot become
    /// a zombie after exiting.
    fn spawn_reaped(mut command: Command) -> io::Result<()> {
        let mut child = command.spawn()?;
        std::thread::spawn(move || {
            let _ = child.wait();
        });
        Ok(())
    }

    fn spawn_companion(name: &str) -> io::Result<()> {
        Self::spawn_reaped(Command::new(Self::companion_binary(name)))
    }

    fn auto_start_widget() {
        std::thread::sleep(WIDGET_STARTUP_DELAY);

        for attempt in 1..=WIDGET_STARTUP_ATTEMPTS {
            if Self::check_widget_running() {
                log::info!("Widget is running; auto-start finished");
                return;
            }

            match Self::spawn_companion("cosmic-widget") {
                Ok(()) => log::info!(
                    "Widget auto-start attempt {}/{} launched",
                    attempt,
                    WIDGET_STARTUP_ATTEMPTS
                ),
                Err(error) => log::error!(
                    "Widget auto-start attempt {}/{} failed: {}",
                    attempt,
                    WIDGET_STARTUP_ATTEMPTS,
                    error
                ),
            }

            std::thread::sleep(WIDGET_RETRY_DELAY);
        }

        if Self::check_widget_running() {
            log::info!("Widget is running; auto-start finished");
        } else {
            log::error!(
                "Widget failed to remain running after {} auto-start attempts",
                WIDGET_STARTUP_ATTEMPTS
            );
        }
    }
}

// ============================================================================
// COSMIC Application Implementation
// ============================================================================

impl cosmic::Application for AppModel {
    /// Use COSMIC's default async executor (tokio-based).
    type Executor = cosmic::executor::Default;

    /// No initialization flags needed - configuration is loaded internally.
    type Flags = ();

    /// The message type for this application.
    type Message = Message;

    /// Unique application identifier in reverse domain name notation.
    /// Used for:
    /// - Configuration storage path (~/.config/cosmic/com.github.zoliviragh.CosmicWidget/)
    /// - D-Bus registration
    /// - Desktop file identification
    const APP_ID: &'static str = "com.github.zoliviragh.CosmicWidget";

    fn core(&self) -> &cosmic::Core {
        &self.core
    }

    fn core_mut(&mut self) -> &mut cosmic::Core {
        &mut self.core
    }

    /// Initialize the applet when it's first loaded.
    ///
    /// This runs once when the panel starts or when the applet is added.
    /// Loads configuration and optionally auto-starts the widget.
    fn init(
        core: cosmic::Core,
        _flags: Self::Flags,
    ) -> (Self, Task<cosmic::Action<Self::Message>>) {
        // Initialize cosmic-config handler for this app's configuration
        let config_handler = cosmic_config::Config::new(Self::APP_ID, Config::VERSION).ok();

        // Load existing config or use defaults if none exists
        let config = config_handler
            .as_ref()
            .map(|context| match Config::get_entry(context) {
                Ok(config) => config,
                Err((_errors, config)) => config, // Use defaults on parse error
            })
            .unwrap_or_default();

        // Auto-start widget if configured to do so
        // Note: We add a small delay to give the compositor time to fully initialize
        // its layer-shell input routing. Without this, the widget may not receive
        // pointer events when started too early at boot.
        let widget_running = if config.widget_autostart {
            if Self::check_widget_running() {
                log::info!("Auto-start enabled, but the widget is already running");
            } else {
                log::info!("Auto-start enabled; waiting for the compositor before launching");
                std::thread::spawn(Self::auto_start_widget);
            }
            true
        } else {
            // Check if widget is already running (user may have started it manually)
            log::info!("Auto-start disabled, checking if widget is already running");
            Self::check_widget_running()
        };

        let app = AppModel {
            core,
            config,
            config_handler,
            widget_running,
            ..Default::default()
        };

        (app, Task::none())
    }

    /// Handle window close requests.
    ///
    /// When the popup is closed by the user (clicking outside, pressing Esc),
    /// this converts the close event into a PopupClosed message.
    fn on_close_requested(&self, id: Id) -> Option<Message> {
        Some(Message::PopupClosed(id))
    }

    /// Render the panel icon.
    ///
    /// This is what appears in the COSMIC panel. It's a simple icon button
    /// that shows the system monitor icon and opens the popup when clicked.
    fn view(&self) -> Element<'_, Self::Message> {
        self.core
            .applet
            .icon_button("utilities-system-monitor-symbolic")
            .on_press(Message::TogglePopup)
            .into()
    }

    /// Render the popup menu content.
    ///
    /// Shows two options:
    /// 1. "Show Widget" / "Hide Widget" - toggles the monitoring widget
    /// 2. "Configure" - opens the settings application
    ///
    /// The popup uses COSMIC's standard applet popup styling.
    fn view_window(&self, _id: Id) -> Element<'_, Self::Message> {
        // Dynamic text based on widget state
        let widget_text = if self.widget_running {
            fl!("hide-widget") // From i18n: "Hide Widget"
        } else {
            fl!("show-widget") // From i18n: "Show Widget"
        };

        let content_list = widget::list_column()
            // Widget toggle button
            .add(widget::settings::item(
                widget_text,
                widget::button::icon(widget::icon::from_name("applications-system-symbolic"))
                    .on_press(Message::ToggleWidget),
            ))
            // Settings button
            .add(widget::settings::item(
                fl!("configure"), // From i18n: "Configure"
                widget::button::icon(widget::icon::from_name("preferences-system-symbolic"))
                    .on_press(Message::OpenSettings),
            ));

        self.core.applet.popup_container(content_list).into()
    }

    /// Set up background tasks and event listeners.
    ///
    /// Two subscriptions are active:
    /// 1. A channel subscription (currently unused, placeholder for future features)
    /// 2. Configuration watcher that syncs changes from settings app
    fn subscription(&self) -> Subscription<Self::Message> {
        struct MySubscription;

        Subscription::batch(vec![
            // Placeholder subscription channel for future async features
            Subscription::run_with(std::any::TypeId::of::<MySubscription>(), |_| {
                cosmic::iced::stream::channel(
                    4,
                    move |mut channel: cosmic::iced::futures::channel::mpsc::Sender<Message>| async move {
                        _ = channel.send(Message::SubscriptionChannel).await;
                        // Keep the subscription alive indefinitely
                        futures_util::future::pending().await
                    },
                )
            }),
            // Watch for configuration changes from the settings app
            // This keeps the applet in sync when settings are modified externally
            self.core()
                .watch_config::<Config>(Self::APP_ID)
                .map(|update| Message::UpdateConfig(update.config)),
        ])
    }

    /// Process messages and update application state.
    ///
    /// This is the heart of the iced architecture - all state changes
    /// happen here in response to messages.
    fn update(&mut self, message: Self::Message) -> Task<cosmic::Action<Self::Message>> {
        match message {
            Message::SubscriptionChannel => {
                // Placeholder for future async initialization
            }

            Message::UpdateConfig(config) => {
                // External config change (from settings app) - update our copy
                self.config = config;
            }

            Message::ToggleWidget => {
                if self.widget_running {
                    // Kill the widget process
                    // Use exact match to avoid killing cosmic-widget-applet too
                    log::info!("Stopping widget via pkill");
                    let mut command = Command::new("pkill");
                    command.arg("-x").arg("cosmic-widget");
                    let _ = Self::spawn_reaped(command);
                    self.widget_running = false;

                    // Disable auto-start since user explicitly hid the widget
                    self.config.widget_autostart = false;
                    self.save_config();
                } else {
                    // Launch the widget process
                    log::info!("Launching widget");
                    if Self::spawn_companion("cosmic-widget").is_ok() {
                        self.widget_running = true;

                        // Enable auto-start since user explicitly showed the widget
                        self.config.widget_autostart = true;
                        self.save_config();
                        log::info!("Widget launched successfully");
                    } else {
                        log::error!("Failed to launch widget");
                    }
                }
            }

            Message::OpenSettings => {
                // Launch the settings application as a separate process
                let _ = Self::spawn_companion("cosmic-widget-settings");
            }

            Message::TogglePopup => {
                return if let Some(p) = self.popup.take() {
                    // Popup is open - close it
                    destroy_popup(p)
                } else {
                    // Popup is closed - open it
                    // First refresh widget status (it may have been killed externally)
                    self.widget_running = Self::check_widget_running();

                    // Create a new popup window
                    let new_id = Id::unique();
                    self.popup.replace(new_id);

                    // Configure popup positioning and size constraints
                    let mut popup_settings = self.core.applet.get_popup_settings(
                        self.core.main_window_id().unwrap(),
                        new_id,
                        None, // No keyboard interactivity
                        None, // Default anchor
                        None, // Default gravity
                    );

                    // Set size limits for the popup
                    popup_settings.positioner.size_limits = Limits::NONE
                        .max_width(372.0)
                        .min_width(300.0)
                        .min_height(200.0)
                        .max_height(1080.0);

                    get_popup(popup_settings)
                };
            }

            Message::PopupClosed(id) => {
                // Clear popup state when closed
                if self.popup.as_ref() == Some(&id) {
                    self.popup = None;
                }
            }
        }
        Task::none()
    }

    /// Apply COSMIC applet styling.
    ///
    /// This ensures the applet matches the system theme and other applets.
    fn style(&self) -> Option<cosmic::iced::theme::Style> {
        Some(cosmic::applet::style())
    }
}
