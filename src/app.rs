// SPDX-License-Identifier: MPL-2.0

use crate::config::Config;
use crate::fl;
use cosmic::cosmic_config::{self, CosmicConfigEntry};
use cosmic::iced::{window::Id, Limits, Subscription};
use cosmic::iced_winit::commands::popup::{destroy_popup, get_popup};
use cosmic::prelude::*;
use cosmic::widget;
use futures_util::SinkExt;

/// The application model stores app-specific state used to describe its interface and
/// drive its logic.
#[derive(Default)]
pub struct AppModel {
    /// Application state which is managed by the COSMIC runtime.
    core: cosmic::Core,
    /// The popup id.
    popup: Option<Id>,
    /// Configuration data that persists between application runs.
    config: Config,
    /// Helper to save config changes.
    config_handler: Option<cosmic_config::Config>,
    /// Temporary state for the interval text input
    interval_input: String,
    /// Track if widget is currently running
    widget_running: bool,
}

/// Messages emitted by the application and its widgets.
#[derive(Debug, Clone)]
pub enum Message {
    TogglePopup,
    PopupClosed(Id),
    SubscriptionChannel,
    UpdateConfig(Config),
    ToggleWidget,
    OpenSettings,
}

impl AppModel {
    fn save_config(&self) {
        if let Some(ref config_handler) = self.config_handler {
            if let Err(err) = self.config.write_entry(config_handler) {
                eprintln!("Failed to save config: {}", err);
            }
        }
    }
    
    fn check_widget_running() -> bool {
        // Check if cosmic-monitor-widget process is running
        if let Ok(output) = std::process::Command::new("pgrep")
            .arg("-f")
            .arg("cosmic-monitor-widget")
            .output()
        {
            !output.stdout.is_empty()
        } else {
            false
        }
    }
}

/// Create a COSMIC application from the app model
impl cosmic::Application for AppModel {
    /// The async executor that will be used to run your application's commands.
    type Executor = cosmic::executor::Default;

    /// Data that your application receives to its init method.
    type Flags = ();

    /// Messages which the application and its widgets will emit.
    type Message = Message;

    /// Unique identifier in RDNN (reverse domain name notation) format.
    const APP_ID: &'static str = "com.github.zoliviragh.CosmicMonitor";

    fn core(&self) -> &cosmic::Core {
        &self.core
    }

    fn core_mut(&mut self) -> &mut cosmic::Core {
        &mut self.core
    }

    /// Initializes the application with any given flags and startup commands.
    fn init(
        core: cosmic::Core,
        _flags: Self::Flags,
    ) -> (Self, Task<cosmic::Action<Self::Message>>) {
        // Construct the app model with the runtime's core.
        let config_handler = cosmic_config::Config::new(Self::APP_ID, Config::VERSION).ok();
        
        let config = config_handler
            .as_ref()
            .map(|context| match Config::get_entry(context) {
                Ok(config) => config,
                Err((_errors, config)) => config,
            })
            .unwrap_or_default();

        let interval_input = format!("{}", config.update_interval_ms);
        
        // Check if widget should auto-start
        let widget_running = if config.widget_autostart {
            // Try to launch the widget
            log::info!("Auto-start enabled, launching widget");
            if let Ok(_) = std::process::Command::new("cosmic-monitor-widget").spawn() {
                log::info!("Widget auto-started successfully");
                true
            } else {
                log::error!("Failed to auto-start widget");
                false
            }
        } else {
            // Check if widget is already running even if autostart is disabled
            log::info!("Auto-start disabled, checking if widget is already running");
            Self::check_widget_running()
        };

        let app = AppModel {
            core,
            config,
            config_handler,
            interval_input,
            widget_running,
            ..Default::default()
        };

        (app, Task::none())
    }

    fn on_close_requested(&self, id: Id) -> Option<Message> {
        Some(Message::PopupClosed(id))
    }

    /// Describes the interface based on the current state of the application model.
    ///
    /// The applet's button in the panel will be drawn using the main view method.
    /// This view should emit messages to toggle the applet's popup window, which will
    /// be drawn using the `view_window` method.
    fn view(&self) -> Element<'_, Self::Message> {
        self.core
            .applet
            .icon_button("utilities-system-monitor-symbolic")
            .on_press(Message::TogglePopup)
            .into()
    }

    /// The applet's popup window will be drawn using this view method. If there are
    /// multiple poups, you may match the id parameter to determine which popup to
    /// create a view for.
    fn view_window(&self, _id: Id) -> Element<'_, Self::Message> {
        let widget_text = if self.widget_running {
            fl!("hide-widget")
        } else {
            fl!("show-widget")
        };

        let content_list = widget::list_column()
            .padding(5)
            .spacing(0)
            .add(widget::settings::item(
                widget_text,
                widget::button::icon(widget::icon::from_name("applications-system-symbolic"))
                    .on_press(Message::ToggleWidget)
            ))
            .add(widget::settings::item(
                fl!("configure"),
                widget::button::icon(widget::icon::from_name("preferences-system-symbolic"))
                    .on_press(Message::OpenSettings)
            ));

        self.core.applet.popup_container(content_list).into()
    }

    /// Register subscriptions for this application.
    ///
    /// Subscriptions are long-lived async tasks running in the background which
    /// emit messages to the application through a channel. They may be conditionally
    /// activated by selectively appending to the subscription batch, and will
    /// continue to execute for the duration that they remain in the batch.
    fn subscription(&self) -> Subscription<Self::Message> {
        struct MySubscription;

        Subscription::batch(vec![
            // Create a subscription which emits updates through a channel.
            Subscription::run_with_id(
                std::any::TypeId::of::<MySubscription>(),
                cosmic::iced::stream::channel(4, move |mut channel| async move {
                    _ = channel.send(Message::SubscriptionChannel).await;

                    futures_util::future::pending().await
                }),
            ),
            // Watch for application configuration changes.
            self.core()
                .watch_config::<Config>(Self::APP_ID)
                .map(|update| {
                    // for why in update.errors {
                    //     tracing::error!(?why, "app config error");
                    // }

                    Message::UpdateConfig(update.config)
                }),
        ])
    }

    /// Handles messages emitted by the application and its widgets.
    ///
    /// Tasks may be returned for asynchronous execution of code in the background
    /// on the application's async runtime. The application will not exit until all
    /// tasks are finished.
    fn update(&mut self, message: Self::Message) -> Task<cosmic::Action<Self::Message>> {
        match message {
            Message::SubscriptionChannel => {
                // For example purposes only.
            }
            Message::UpdateConfig(config) => {
                self.config = config;
            }
            Message::ToggleWidget => {
                // Toggle widget visibility
                if self.widget_running {
                    // Try to kill widget (TODO: track PID properly)
                    log::info!("Stopping widget via pkill");
                    let _ = std::process::Command::new("pkill")
                        .arg("-f")
                        .arg("cosmic-monitor-widget")
                        .spawn();
                    self.widget_running = false;
                    // Update config to not auto-start
                    self.config.widget_autostart = false;
                    self.save_config();
                } else {
                    // Launch the widget
                    log::info!("Launching widget");
                    if let Ok(_) = std::process::Command::new("cosmic-monitor-widget").spawn() {
                        self.widget_running = true;
                        // Update config to auto-start
                        self.config.widget_autostart = true;
                        self.save_config();
                        log::info!("Widget launched successfully");
                    } else {
                        log::error!("Failed to launch widget");
                    }
                }
            }
            Message::OpenSettings => {
                // Launch settings app
                let _ = std::process::Command::new("cosmic-monitor-settings").spawn();
            }
            Message::TogglePopup => {
                return if let Some(p) = self.popup.take() {
                    destroy_popup(p)
                } else {
                    // Check current widget status when opening popup
                    self.widget_running = Self::check_widget_running();
                    
                    let new_id = Id::unique();
                    self.popup.replace(new_id);
                    let mut popup_settings = self.core.applet.get_popup_settings(
                        self.core.main_window_id().unwrap(),
                        new_id,
                        None,
                        None,
                        None,
                    );
                    popup_settings.positioner.size_limits = Limits::NONE
                        .max_width(372.0)
                        .min_width(300.0)
                        .min_height(200.0)
                        .max_height(1080.0);
                    get_popup(popup_settings)
                }
            }
            Message::PopupClosed(id) => {
                if self.popup.as_ref() == Some(&id) {
                    self.popup = None;
                }
            }
        }
        Task::none()
    }

    fn style(&self) -> Option<cosmic::iced_runtime::Appearance> {
        Some(cosmic::applet::style())
    }
}
