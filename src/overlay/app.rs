// SPDX-License-Identifier: MPL-2.0

//! Overlay event loop and coordination of monitoring, surfaces, and animation.

use super::animation::{
    ExpansionAnimation, NOTIFICATION_GROUP_EXPANSION_DURATION, ScrollAnimation,
};
use super::layout::{
    desired_surface_height, desired_surface_height_with_animation, notification_group_size,
};
use super::media_state::{
    DismissingMedia, MEDIA_CONTROL_GRACE, PendingPlayback, advance_media_dismissal,
    reconcile_media_dismissal, reconcile_media_state,
};
use super::notification_state::{
    DismissingNotification, NotificationKey, open_notification_folder,
    transition_clear_button_for_notification_change,
};
use super::stats::{StatsSampler, SystemSnapshot};
use super::surface::{
    create_overlay_surface, dragged_overlay_position, frosted_enabled, overlay_corners,
    set_surface_blur, set_surface_corners, set_surface_regions, system_theme,
};
use super::ticks::aligned_tick_stream;
use super::view;
use crate::config::{Config, UPDATE_INTERVAL_MS};
use crate::monitors::media::{MultiPlayerState, PlaybackStatus, PlayerId};
use chrono::{DateTime, Local};
use cosmic::cosmic_config::{self, CosmicConfigEntry};
use cosmic::iced::platform_specific::runtime::wayland::CornerRadius;
use cosmic::iced::platform_specific::shell::commands::layer_surface;
use cosmic::iced::{self, Color, Point, Subscription, Task, window};
use std::time::{Duration, Instant};

const APP_ID: &str = "com.github.zoliviragh.CosmicWidget.Iced";
const CORNER_RADIUS_STARTUP_DELAY: Duration = Duration::from_secs(1);
const SURFACE_RECOVERY_DELAY: Duration = Duration::from_millis(750);

pub(super) fn run() -> iced::Result {
    cosmic::icon_theme::set_default(cosmic::config::icon_theme());

    iced::daemon(App::new, App::update, App::view)
        .executor::<cosmic::executor::single::Executor>()
        .title(App::title)
        .subscription(App::subscription)
        .theme(App::theme)
        .style(App::style)
        .default_font(cosmic::font::default())
        .antialiasing(true)
        .settings(iced::Settings {
            id: Some(APP_ID.to_string()),
            is_daemon: true,
            ..iced::Settings::default()
        })
        .run()
}

struct App {
    config: Config,
    config_handler: Option<cosmic_config::Config>,
    now: DateTime<Local>,
    snapshot: SystemSnapshot,
    sampler: StatsSampler,
    surface_id: window::Id,
    surface_height: u32,
    frosted: bool,
    corners: Option<CornerRadius>,
    corners_ready_at: Instant,
    expanded_notification_group: Option<String>,
    expanded_notification: Option<NotificationKey>,
    hovered_notification: Option<NotificationKey>,
    notification_group_expansion: ExpansionAnimation,
    notification_expansion: ExpansionAnimation,
    dismissing_notifications: Vec<DismissingNotification>,
    clearing_notifications: bool,
    clear_button_animation: ExpansionAnimation,
    notification_scroll: ScrollAnimation,
    dismissing_media: Option<DismissingMedia>,
    media_seek_preview: Option<f64>,
    media_timeline_hovered: bool,
    pending_playback: Option<PendingPlayback>,
    overlay_cursor: Point,
    overlay_drag_cursor: Option<Point>,
    surface_recovery_generation: u64,
}

#[derive(Debug, Clone)]
pub(super) enum Message {
    Tick,
    AnimationTick,
    ClearNotifications,
    ToggleNotificationGroup { source: String },
    ToggleNotification { key: NotificationKey },
    DismissNotification { key: NotificationKey },
    NotificationHoverChanged { key: NotificationKey, hovered: bool },
    OpenNotificationFolder { key: NotificationKey },
    ActivateNotification { key: NotificationKey },
    NotificationFolderOpened(Result<(), String>),
    PreviousMedia,
    PlayPauseMedia,
    NextMedia,
    SelectMediaPlayer(PlayerId),
    MediaTimelineHoverChanged(bool),
    MediaSeekChanged(f64),
    CommitMediaSeek,
    NotificationScrolled(f32),
    OverlayPointerMoved(Point),
    BeginOverlayDrag,
    EndOverlayDrag,
    PinOverlay,
    OutputTopologyChanged,
    RecoverSurface(u64),
}

impl App {
    fn new() -> (Self, Task<Message>) {
        let config_handler =
            cosmic_config::Config::new("com.github.zoliviragh.CosmicWidget", Config::VERSION).ok();
        let mut config = config_handler
            .as_ref()
            .map(|handler| match Config::get_entry(handler) {
                Ok(config) => config,
                Err((_errors, config)) => config,
            })
            .unwrap_or_default();
        config.ensure_all_sections();
        let sampler = StatsSampler::spawn(
            config.show_weather,
            config.enable_solaar_integration,
            config.weather_location.clone(),
            config.max_notifications,
            config.cider_api_token.clone(),
            config.show_codex_usage,
        );
        let surface_id = window::Id::unique();
        let frosted = frosted_enabled();
        let snapshot = SystemSnapshot::default();
        let surface_height = desired_surface_height(&config, &snapshot);

        let create_surface = create_overlay_surface(surface_id, &config, surface_height, frosted);

        (
            Self {
                config,
                config_handler,
                now: Local::now(),
                snapshot,
                sampler,
                surface_id,
                surface_height,
                frosted,
                // The compositor validates radii against the committed buffer,
                // so wait until the 1x1 bootstrap surface has been replaced.
                corners: None,
                corners_ready_at: Instant::now() + CORNER_RADIUS_STARTUP_DELAY,
                expanded_notification_group: None,
                expanded_notification: None,
                hovered_notification: None,
                notification_group_expansion: ExpansionAnimation::with_duration(
                    NOTIFICATION_GROUP_EXPANSION_DURATION,
                ),
                notification_expansion: ExpansionAnimation::default(),
                dismissing_notifications: Vec::new(),
                clearing_notifications: false,
                clear_button_animation: ExpansionAnimation::default(),
                notification_scroll: ScrollAnimation::default(),
                dismissing_media: None,
                media_seek_preview: None,
                media_timeline_hovered: false,
                pending_playback: None,
                overlay_cursor: Point::ORIGIN,
                overlay_drag_cursor: None,
                surface_recovery_generation: 0,
            },
            create_surface,
        )
    }

    fn title(&self, _window: window::Id) -> String {
        "COSMIC Widget".to_string()
    }

    fn theme(&self, _window: window::Id) -> cosmic::Theme {
        system_theme()
    }

    fn style(&self, theme: &cosmic::Theme) -> iced::theme::Style {
        iced::theme::Style {
            background_color: Color::TRANSPARENT,
            text_color: theme.cosmic().on_bg_color().into(),
            icon_color: theme.cosmic().on_bg_color().into(),
        }
    }

    fn update(&mut self, message: Message) -> Task<Message> {
        // The outgoing card is visual only. Ignore any control event queued
        // just before its source disappeared rather than redirecting it.
        if self.dismissing_media.is_some()
            && matches!(
                &message,
                Message::PreviousMedia
                    | Message::PlayPauseMedia
                    | Message::NextMedia
                    | Message::SelectMediaPlayer(_)
                    | Message::MediaSeekChanged(_)
                    | Message::CommitMediaSeek
                    | Message::MediaTimelineHoverChanged(_)
            )
        {
            return Task::none();
        }
        let mut tasks = Vec::new();

        match message {
            Message::Tick => {
                self.now = Local::now();
                let had_notifications = !self.snapshot.notifications.is_empty();
                let mut snapshot = self.sampler.snapshot();
                snapshot.media = self.prepare_media_state(snapshot.media, Instant::now());
                self.snapshot = snapshot;
                let has_notifications = !self.snapshot.notifications.is_empty();
                transition_clear_button_for_notification_change(
                    &mut self.clear_button_animation,
                    had_notifications,
                    has_notifications,
                    self.clearing_notifications,
                    Instant::now(),
                );
                self.dismissing_notifications.retain(|dismissal| {
                    self.snapshot
                        .notifications
                        .iter()
                        .any(|notification| dismissal.matches(notification))
                });
                if self.expanded_notification.as_ref().is_some_and(|key| {
                    !self
                        .snapshot
                        .notifications
                        .iter()
                        .any(|notification| key.matches(notification))
                }) {
                    self.expanded_notification = None;
                    self.notification_expansion.reset();
                }
                if self.hovered_notification.as_ref().is_some_and(|key| {
                    !self
                        .snapshot
                        .notifications
                        .iter()
                        .any(|notification| key.matches(notification))
                }) {
                    self.hovered_notification = None;
                }
                if self
                    .expanded_notification_group
                    .as_ref()
                    .is_some_and(|source| notification_group_size(&self.snapshot, source) < 2)
                {
                    self.expanded_notification_group = None;
                    self.notification_group_expansion.reset();
                }
                if Instant::now() >= self.corners_ready_at {
                    let corners = overlay_corners();
                    if self.corners != Some(corners) {
                        self.corners = Some(corners);
                        tasks.push(set_surface_corners(self.surface_id, corners));
                    }
                }

                if let Some(handler) = &self.config_handler {
                    let mut config =
                        Config::get_entry(handler).unwrap_or_else(|(_errors, config)| config);
                    config.ensure_all_sections();
                    if self.config.widget_movable && config.widget_movable {
                        config.widget_x = self.config.widget_x;
                        config.widget_y = self.config.widget_y;
                    }
                    if config != self.config {
                        crate::runtime::logging::set_enabled(config.enable_logging);
                        let position_changed = config.widget_x != self.config.widget_x
                            || config.widget_y != self.config.widget_y;
                        self.sampler.set_weather_config(
                            config.show_weather,
                            config.weather_location.clone(),
                        );
                        self.sampler
                            .set_solaar_enabled(config.enable_solaar_integration);
                        self.sampler.set_ai_usage_enabled(config.show_codex_usage);
                        if config.cider_api_token != self.config.cider_api_token {
                            self.sampler.set_cider_token(config.cider_api_token.clone());
                        }
                        self.config = config;
                        if !self.config.widget_movable {
                            self.overlay_drag_cursor = None;
                        }

                        if position_changed {
                            tasks.push(layer_surface::set_margin(
                                self.surface_id,
                                self.config.widget_y,
                                0,
                                0,
                                self.config.widget_x,
                            ));
                        }
                    }
                }

                let frosted = frosted_enabled();
                let surface_height = desired_surface_height_with_animation(
                    &self.config,
                    &self.snapshot,
                    self.expanded_notification.as_ref(),
                    self.expanded_notification_group.as_deref(),
                    self.notification_expansion.progress,
                    self.notification_group_expansion.progress,
                    self.dismissing_media.is_some(),
                );
                let size_changed = surface_height != self.surface_height;
                let frosted_changed = frosted != self.frosted;
                let animations_active = self.animations_active();

                self.frosted = frosted;
                if size_changed {
                    self.surface_height = surface_height;
                    if !animations_active {
                        tasks.push(set_surface_regions(
                            self.surface_id,
                            surface_height,
                            frosted,
                        ));
                    }
                }
                if frosted_changed && !size_changed {
                    tasks.push(set_surface_blur(self.surface_id, frosted, surface_height));
                }
            }
            Message::AnimationTick => {
                let now = Instant::now();
                let was_animating = self.animations_active();
                self.notification_expansion.advance(now);
                self.notification_group_expansion.advance(now);
                self.clear_button_animation.advance(now);
                self.notification_scroll.advance(now);
                advance_media_dismissal(&mut self.dismissing_media, now);
                for dismissal in &mut self.dismissing_notifications {
                    dismissal.animation.advance(now);
                }

                if self.clearing_notifications {
                    let rows_completed = self.dismissing_notifications.iter().all(|dismissal| {
                        !dismissal.animation.is_animating() && dismissal.animation.target == 1.0
                    });
                    let clear_completed = rows_completed
                        && !self.clear_button_animation.is_animating()
                        && self.clear_button_animation.target == 0.0;
                    if clear_completed {
                        self.sampler.clear_notifications();
                        self.snapshot.notifications.clear();
                        self.expanded_notification_group = None;
                        self.expanded_notification = None;
                        self.hovered_notification = None;
                        self.notification_group_expansion.reset();
                        self.notification_expansion.reset();
                        self.dismissing_notifications.clear();
                        self.clearing_notifications = false;
                        self.notification_scroll = ScrollAnimation::default();
                    }
                } else {
                    let had_notifications = !self.snapshot.notifications.is_empty();
                    let mut completed_dismissals = Vec::new();
                    self.dismissing_notifications.retain(|dismissal| {
                        let completed = !dismissal.animation.is_animating()
                            && dismissal.animation.target == 1.0;
                        if completed {
                            completed_dismissals
                                .push((dismissal.key.clone(), dismissal.source.clone()));
                        }
                        !completed
                    });

                    for (key, source) in completed_dismissals {
                        self.sampler.dismiss_notification(&key);
                        self.snapshot
                            .notifications
                            .retain(|notification| !key.matches(notification));
                        if self.expanded_notification.as_ref() == Some(&key) {
                            self.expanded_notification = None;
                            self.notification_expansion.reset();
                        }
                        if self.expanded_notification_group.as_deref() == Some(&source)
                            && notification_group_size(&self.snapshot, &source) < 2
                        {
                            self.expanded_notification_group = None;
                            self.notification_group_expansion.reset();
                        }
                    }
                    transition_clear_button_for_notification_change(
                        &mut self.clear_button_animation,
                        had_notifications,
                        !self.snapshot.notifications.is_empty(),
                        false,
                        now,
                    );
                }

                if self.notification_expansion.is_collapsed() {
                    self.expanded_notification = None;
                }
                if self.notification_group_expansion.is_collapsed() {
                    self.expanded_notification_group = None;
                }

                self.surface_height = desired_surface_height_with_animation(
                    &self.config,
                    &self.snapshot,
                    self.expanded_notification.as_ref(),
                    self.expanded_notification_group.as_deref(),
                    self.notification_expansion.progress,
                    self.notification_group_expansion.progress,
                    self.dismissing_media.is_some(),
                );

                if was_animating && !self.animations_active() {
                    tasks.push(set_surface_regions(
                        self.surface_id,
                        self.surface_height,
                        self.frosted,
                    ));
                }
            }
            Message::ClearNotifications => {
                if self.snapshot.notifications.is_empty() || self.clearing_notifications {
                    return Task::none();
                }

                let now = Instant::now();
                self.dismissing_notifications = self
                    .snapshot
                    .notifications
                    .iter()
                    .map(|notification| DismissingNotification::new(notification, now))
                    .collect();
                self.clearing_notifications = true;
                self.clear_button_animation.transition_to(0.0, now);
                self.hovered_notification = None;
            }
            Message::ToggleNotificationGroup { source } => {
                let now = Instant::now();
                let current_scroll_offset = self.notification_scroll.target;
                self.notification_scroll.snap_to(current_scroll_offset);
                if self.expanded_notification_group.as_deref() == Some(&source) {
                    let target = if self.notification_group_expansion.target > 0.0 {
                        0.0
                    } else {
                        1.0
                    };
                    self.notification_group_expansion.transition_to(target, now);
                } else {
                    self.expanded_notification_group = Some(source);
                    self.notification_group_expansion.reset();
                    self.notification_group_expansion.transition_to(1.0, now);
                }
                self.expanded_notification = None;
                self.notification_expansion.reset();
                tasks.push(set_surface_regions(
                    self.surface_id,
                    self.surface_height.max(self.target_surface_height()),
                    self.frosted,
                ));
            }
            Message::ToggleNotification { key: selected } => {
                let now = Instant::now();
                if self.expanded_notification.as_ref() == Some(&selected) {
                    let target = if self.notification_expansion.target > 0.0 {
                        0.0
                    } else {
                        1.0
                    };
                    self.notification_expansion.transition_to(target, now);
                } else {
                    self.expanded_notification = Some(selected);
                    self.notification_expansion.reset();
                    self.notification_expansion.transition_to(1.0, now);
                }
                tasks.push(set_surface_regions(
                    self.surface_id,
                    self.surface_height.max(self.target_surface_height()),
                    self.frosted,
                ));
            }
            Message::DismissNotification { key } => {
                if self.clearing_notifications {
                    return Task::none();
                }
                if self
                    .dismissing_notifications
                    .iter()
                    .any(|dismissal| dismissal.key == key)
                {
                    return Task::none();
                }
                let Some(notification) = self
                    .snapshot
                    .notifications
                    .iter()
                    .find(|notification| key.matches(notification))
                else {
                    return Task::none();
                };
                self.dismissing_notifications
                    .push(DismissingNotification::new(notification, Instant::now()));
            }
            Message::NotificationHoverChanged { key, hovered } => {
                if hovered {
                    self.hovered_notification = Some(key);
                } else if self.hovered_notification.as_ref() == Some(&key) {
                    self.hovered_notification = None;
                }
            }
            Message::OpenNotificationFolder { key } => {
                if let Some(folder) = self
                    .snapshot
                    .notifications
                    .iter()
                    .find(|notification| key.matches(notification))
                    .and_then(|notification| notification.open_folder.clone())
                {
                    tasks.push(Task::perform(
                        open_notification_folder(folder),
                        Message::NotificationFolderOpened,
                    ));
                }
            }
            Message::ActivateNotification { key } => {
                if !self.sampler.activate_notification(&key) {
                    log::warn!("Notification no longer has an actionable target");
                }
            }
            Message::NotificationFolderOpened(result) => {
                if let Err(error) = result {
                    log::warn!("Failed to open notification folder: {error}");
                }
            }
            Message::NotificationScrolled(offset) => {
                if self.notification_group_expansion.is_animating() {
                    self.notification_scroll.snap_to(offset);
                } else {
                    self.notification_scroll
                        .transition_to(offset, Instant::now());
                }
            }
            Message::PreviousMedia => {
                self.media_seek_preview = None;
                self.pending_playback = None;
                self.sampler.previous_media();
            }
            Message::PlayPauseMedia => {
                self.pending_playback = self.snapshot.media.current_player().map(|(id, info)| {
                    let status = match info.status {
                        PlaybackStatus::Playing => PlaybackStatus::Paused,
                        PlaybackStatus::Paused | PlaybackStatus::Stopped => PlaybackStatus::Playing,
                    };

                    PendingPlayback {
                        player_id: id.clone(),
                        status,
                        expires_at: Instant::now() + MEDIA_CONTROL_GRACE,
                    }
                });
                self.sampler.play_pause_media();
                self.snapshot.media =
                    self.prepare_media_state(self.sampler.media_state(), Instant::now());
            }
            Message::NextMedia => {
                self.media_seek_preview = None;
                self.pending_playback = None;
                self.sampler.next_media();
            }
            Message::SelectMediaPlayer(player_id) => {
                self.media_seek_preview = None;
                self.media_timeline_hovered = false;
                self.pending_playback = None;
                self.sampler.select_media_player(&player_id);
                self.snapshot.media =
                    self.prepare_media_state(self.sampler.media_state(), Instant::now());
            }
            Message::MediaTimelineHoverChanged(hovered) => {
                self.media_timeline_hovered = hovered;
            }
            Message::MediaSeekChanged(progress) => {
                self.media_seek_preview = Some(progress.clamp(0.0, 1.0));
            }
            Message::CommitMediaSeek => {
                if let Some(progress) = self.media_seek_preview.take() {
                    self.sampler.seek_media(progress);
                    self.snapshot.media =
                        self.prepare_media_state(self.sampler.media_state(), Instant::now());
                }
            }
            Message::OverlayPointerMoved(position) => {
                self.overlay_cursor = position;
                if self.config.widget_movable
                    && let Some(drag_origin) = self.overlay_drag_cursor
                {
                    let (x, y) = dragged_overlay_position(
                        self.config.widget_x,
                        self.config.widget_y,
                        drag_origin,
                        position,
                    );
                    if x != self.config.widget_x || y != self.config.widget_y {
                        self.config.widget_x = x;
                        self.config.widget_y = y;
                        tasks.push(layer_surface::set_margin(self.surface_id, y, 0, 0, x));
                    }
                }
            }
            Message::BeginOverlayDrag => {
                if self.config.widget_movable {
                    self.overlay_drag_cursor = Some(self.overlay_cursor);
                }
            }
            Message::EndOverlayDrag => {
                self.overlay_drag_cursor = None;
            }
            Message::PinOverlay => {
                self.overlay_drag_cursor = None;
                self.config.widget_movable = false;
                if let Some(handler) = &self.config_handler
                    && let Err(error) = self.config.write_entry(handler)
                {
                    log::error!("Failed to save the pinned overlay position: {error}");
                }
            }
            Message::OutputTopologyChanged => {
                self.surface_recovery_generation = self.surface_recovery_generation.wrapping_add(1);
                let generation = self.surface_recovery_generation;
                tasks.push(Task::perform(
                    async move {
                        tokio::time::sleep(SURFACE_RECOVERY_DELAY).await;
                        generation
                    },
                    Message::RecoverSurface,
                ));
            }
            Message::RecoverSurface(generation) => {
                if generation == self.surface_recovery_generation {
                    tasks.push(self.recreate_surface());
                }
            }
        }

        Task::batch(tasks)
    }

    fn view(&self, _window: window::Id) -> cosmic::Element<'_, Message> {
        view::widget_view(
            &self.config,
            self.now,
            &self.snapshot,
            self.expanded_notification_group.as_deref(),
            self.expanded_notification.as_ref(),
            self.hovered_notification.as_ref(),
            self.notification_group_expansion.progress,
            self.notification_group_expansion.target > 0.0,
            self.notification_expansion.progress,
            &self.dismissing_notifications,
            self.clearing_notifications,
            self.clear_button_animation.progress,
            self.notification_scroll.translation(),
            self.surface_height,
            self.dismissing_media.as_ref(),
            self.media_seek_preview,
            self.media_timeline_hovered,
        )
    }

    fn subscription(&self) -> Subscription<Message> {
        let stats = Subscription::run_with(self.ui_tick_interval(), aligned_tick_stream)
            .map(|_| Message::Tick);
        let output_changes = iced::event::listen_with(|event, _status, _window| {
            matches!(
                event,
                iced::Event::PlatformSpecific(iced::event::PlatformSpecific::Wayland(
                    iced::event::wayland::Event::Output(_, _)
                ))
            )
            .then_some(Message::OutputTopologyChanged)
        });

        if self.animations_active() {
            Subscription::batch([
                stats,
                output_changes,
                iced::window::frames().map(|_| Message::AnimationTick),
            ])
        } else {
            Subscription::batch([stats, output_changes])
        }
    }

    fn recreate_surface(&mut self) -> Task<Message> {
        let previous_id = self.surface_id;
        let surface_id = window::Id::unique();
        self.surface_id = surface_id;
        self.corners = None;
        self.corners_ready_at = Instant::now() + CORNER_RADIUS_STARTUP_DELAY;

        layer_surface::destroy_layer_surface(previous_id).chain(create_overlay_surface(
            surface_id,
            &self.config,
            self.surface_height,
            self.frosted,
        ))
    }

    fn animations_active(&self) -> bool {
        self.notification_expansion.is_animating()
            || self.notification_group_expansion.is_animating()
            || self
                .dismissing_notifications
                .iter()
                .any(|dismissal| dismissal.animation.is_animating())
            || self.clear_button_animation.is_animating()
            || self.notification_scroll.is_animating()
            || self
                .dismissing_media
                .as_ref()
                .is_some_and(|media| media.animation.is_animating())
    }

    fn prepare_media_state(
        &mut self,
        mut current: MultiPlayerState,
        now: Instant,
    ) -> MultiPlayerState {
        reconcile_media_state(&mut current, &mut self.pending_playback, now);
        reconcile_media_dismissal(
            &mut self.dismissing_media,
            &self.snapshot.media,
            &current,
            now,
        );
        if self.dismissing_media.is_some() {
            self.media_seek_preview = None;
            self.media_timeline_hovered = false;
            self.pending_playback = None;
        }
        current
    }

    fn ui_tick_interval(&self) -> Duration {
        Duration::from_millis(UPDATE_INTERVAL_MS)
    }

    fn target_surface_height(&self) -> u32 {
        desired_surface_height_with_animation(
            &self.config,
            &self.snapshot,
            self.expanded_notification.as_ref(),
            self.expanded_notification_group.as_deref(),
            self.notification_expansion.target,
            self.notification_group_expansion.target,
            self.dismissing_media.is_some(),
        )
    }
}
