// SPDX-License-Identifier: MPL-2.0

mod gauge;
mod marquee;
mod slide;
mod stats;
mod translate;
mod view;

use crate::config::{Config, WidgetSection};
use crate::media::{MultiPlayerState, PlaybackStatus, PlayerId};
use chrono::{DateTime, Local};
use cosmic::cosmic_config::{self, CosmicConfigEntry};
use cosmic::iced::platform_specific::runtime::wayland::{
    self,
    CornerRadius,
    layer_surface::{IcedMargin, SctkLayerSurfaceSettings},
};
use cosmic::iced::platform_specific::shell::commands::layer_surface::{
    self, Anchor, KeyboardInteractivity, Layer,
};
use cosmic::iced::platform_specific::shell::commands::{blur, corner_radius};
use cosmic::iced::{self, Color, Point, Rectangle, Size, Subscription, Task, window};
use futures_util::SinkExt;
use stats::{StatsSampler, SystemSnapshot};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

const APP_ID: &str = "com.github.zoliviragh.CosmicWidget.Iced";
const SURFACE_WIDTH: u32 = 370;
const BASE_SURFACE_HEIGHT: u32 = 556;
const EMPTY_STORAGE_HEIGHT: u32 = 63;
const STORAGE_SECTION_HEIGHT: u32 = 38;
const STORAGE_ITEM_HEIGHT: u32 = 62;
const EMPTY_DEVICES_HEIGHT: u32 = 83;
const DEVICES_SECTION_HEIGHT: u32 = 54;
const DEVICE_ITEM_HEIGHT: u32 = 33;
const EMPTY_WEATHER_HEIGHT: u32 = 83;
const WEATHER_SECTION_HEIGHT: u32 = 154;
const EMPTY_NOTIFICATIONS_HEIGHT: u32 = 83;
const NOTIFICATIONS_SECTION_HEIGHT: u32 = 65;
const NOTIFICATION_ITEM_HEIGHT: u32 = 47;
const MAX_VISIBLE_NOTIFICATION_ROWS: u32 = 4;
const NOTIFICATION_LINE_HEIGHT: u32 = 17;
const NOTIFICATION_CHARS_PER_LINE: usize = 32;
const NOTIFICATION_EXPANSION_DURATION: Duration = Duration::from_millis(220);
const NOTIFICATION_GROUP_EXPANSION_DURATION: Duration = Duration::from_millis(320);
const EMPTY_MEDIA_HEIGHT: u32 = 83;
const MEDIA_SECTION_HEIGHT: u32 = 248;
const MEDIA_CONTROL_GRACE: Duration = Duration::from_secs(2);
const MIN_UI_TICK_INTERVAL: Duration = Duration::from_millis(250);
const MAX_UI_TICK_INTERVAL: Duration = Duration::from_secs(1);
const UI_TICK_SETTLE_DELAY: Duration = Duration::from_millis(5);

#[derive(Debug, Clone)]
struct PendingPlayback {
    player_id: PlayerId,
    status: PlaybackStatus,
    expires_at: Instant,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct NotificationKey {
    app_name: String,
    timestamp: u64,
}

#[derive(Debug, Clone)]
struct DismissingNotification {
    key: NotificationKey,
    source: String,
    animation: ExpansionAnimation,
}

impl DismissingNotification {
    fn matches(&self, notification: &crate::notifications::Notification) -> bool {
        self.key.matches(notification)
    }
}

impl NotificationKey {
    fn matches(&self, notification: &crate::notifications::Notification) -> bool {
        self.app_name == notification.app_name && self.timestamp == notification.timestamp
    }
}

#[derive(Debug, Clone)]
struct ExpansionAnimation {
    progress: f32,
    start: f32,
    target: f32,
    started_at: Option<Instant>,
    duration: Duration,
    active_duration: Duration,
    linear: bool,
}

impl Default for ExpansionAnimation {
    fn default() -> Self {
        Self {
            progress: 0.0,
            start: 0.0,
            target: 0.0,
            started_at: None,
            duration: NOTIFICATION_EXPANSION_DURATION,
            active_duration: NOTIFICATION_EXPANSION_DURATION,
            linear: false,
        }
    }
}

impl ExpansionAnimation {
    fn with_duration(duration: Duration) -> Self {
        Self {
            duration,
            active_duration: duration,
            linear: true,
            ..Self::default()
        }
    }

    fn transition_to(&mut self, target: f32, now: Instant) {
        self.advance(now);
        self.start = self.progress;
        self.target = target.clamp(0.0, 1.0);
        let distance = (self.start - self.target).abs();
        self.started_at = if distance > f32::EPSILON {
            self.active_duration = self
                .duration
                .mul_f32(distance)
                .max(Duration::from_millis(48));
            Some(now)
        } else {
            None
        };
    }

    fn advance(&mut self, now: Instant) -> bool {
        let Some(started_at) = self.started_at else {
            return false;
        };
        let linear = now
            .saturating_duration_since(started_at)
            .as_secs_f32()
            / self.active_duration.as_secs_f32();
        let t = linear.clamp(0.0, 1.0);
        let eased = if self.linear {
            t
        } else {
            t * t * (3.0 - 2.0 * t)
        };
        self.progress = self.start + (self.target - self.start) * eased;

        if linear >= 1.0 {
            self.progress = self.target;
            self.started_at = None;
        }

        true
    }

    fn reset(&mut self) {
        let duration = self.duration;
        let linear = self.linear;
        *self = Self {
            duration,
            active_duration: duration,
            linear,
            ..Self::default()
        };
    }

    fn is_animating(&self) -> bool {
        self.started_at.is_some()
    }

    fn is_collapsed(&self) -> bool {
        !self.is_animating() && self.target == 0.0
    }
}

#[derive(Debug, Clone, Default)]
struct ScrollAnimation {
    visual_offset: f32,
    start: f32,
    target: f32,
    started_at: Option<Instant>,
}

impl ScrollAnimation {
    fn transition_to(&mut self, target: f32, now: Instant) {
        self.advance(now);
        self.start = self.visual_offset;
        self.target = target.max(0.0);
        self.started_at = if (self.start - self.target).abs() > 0.5 {
            Some(now)
        } else {
            self.visual_offset = self.target;
            None
        };
    }

    fn snap_to(&mut self, offset: f32) {
        let offset = offset.max(0.0);
        self.visual_offset = offset;
        self.start = offset;
        self.target = offset;
        self.started_at = None;
    }

    fn advance(&mut self, now: Instant) -> bool {
        let Some(started_at) = self.started_at else {
            return false;
        };
        let linear = now
            .saturating_duration_since(started_at)
            .as_secs_f32()
            / NOTIFICATION_EXPANSION_DURATION.as_secs_f32();
        let t = linear.clamp(0.0, 1.0);
        let eased = t * t * (3.0 - 2.0 * t);
        self.visual_offset = self.start + (self.target - self.start) * eased;

        if linear >= 1.0 {
            self.visual_offset = self.target;
            self.started_at = None;
        }

        true
    }

    fn translation(&self) -> f32 {
        self.target - self.visual_offset
    }

    fn is_animating(&self) -> bool {
        self.started_at.is_some()
    }
}

pub fn run() -> iced::Result {
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
    expanded_notification_group: Option<String>,
    expanded_notification: Option<NotificationKey>,
    notification_group_expansion: ExpansionAnimation,
    notification_expansion: ExpansionAnimation,
    dismissing_notifications: Vec<DismissingNotification>,
    notification_scroll: ScrollAnimation,
    media_seek_preview: Option<f64>,
    media_timeline_hovered: bool,
    pending_playback: Option<PendingPlayback>,
}

#[derive(Debug, Clone)]
pub enum Message {
    Tick,
    AnimationTick,
    ClearNotifications,
    ToggleNotificationGroup { source: String },
    ToggleNotification { app_name: String, timestamp: u64 },
    DismissNotification { app_name: String, timestamp: u64 },
    PreviousMedia,
    PlayPauseMedia,
    NextMedia,
    SelectMediaPlayer(PlayerId),
    MediaTimelineHoverChanged(bool),
    MediaSeekChanged(f64),
    CommitMediaSeek,
    NotificationScrolled(f32),
}

impl App {
    fn new() -> (Self, Task<Message>) {
        let config_handler =
            cosmic_config::Config::new("com.github.zoliviragh.CosmicWidget", Config::VERSION).ok();
        let config = config_handler
            .as_ref()
            .and_then(|handler| Config::get_entry(handler).ok())
            .unwrap_or_default();
        let sampler = StatsSampler::spawn(
            config.update_interval_ms,
            config.show_weather,
            config.weather_location.clone(),
            config.max_notifications,
            config.cider_api_token.clone(),
        );
        let surface_id = window::Id::unique();
        let frosted = frosted_enabled();
        let snapshot = SystemSnapshot::default();
        let surface_height = desired_surface_height(&config, &snapshot);

        let create_surface = layer_surface::get_layer_surface(SctkLayerSurfaceSettings {
            id: surface_id,
            layer: Layer::Bottom,
            keyboard_interactivity: KeyboardInteractivity::OnDemand,
            anchor: Anchor::TOP.union(Anchor::BOTTOM).union(Anchor::LEFT),
            namespace: "cosmic-widget-iced".to_string(),
            margin: IcedMargin {
                top: config.widget_y,
                left: config.widget_x,
                ..IcedMargin::default()
            },
            // An unspecified size becomes 1x1 for a surface anchored to only
            // one horizontal and vertical edge in the pinned Iced backend.
            size: Some((Some(SURFACE_WIDTH), None)),
            input_zone: Some(vec![surface_region(surface_height)]),
            exclusive_zone: -1,
            ..SctkLayerSurfaceSettings::default()
        });

        let create_surface = if frosted {
            create_surface.chain(set_surface_blur(surface_id, true, surface_height))
        } else {
            create_surface
        };

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
                expanded_notification_group: None,
                expanded_notification: None,
                notification_group_expansion: ExpansionAnimation::with_duration(
                    NOTIFICATION_GROUP_EXPANSION_DURATION,
                ),
                notification_expansion: ExpansionAnimation::default(),
                dismissing_notifications: Vec::new(),
                notification_scroll: ScrollAnimation::default(),
                media_seek_preview: None,
                media_timeline_hovered: false,
                pending_playback: None,
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
        let mut tasks = Vec::new();

        match message {
            Message::Tick => {
                self.now = Local::now();
                let mut snapshot = self.sampler.snapshot();
                reconcile_media_state(
                    &mut snapshot.media,
                    &mut self.pending_playback,
                    Instant::now(),
                );
                self.snapshot = snapshot;
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
                if self
                    .expanded_notification_group
                    .as_ref()
                    .is_some_and(|source| {
                        notification_group_size(&self.snapshot, source) < 2
                    })
                {
                    self.expanded_notification_group = None;
                    self.notification_group_expansion.reset();
                }
                let corners = overlay_corners();
                if self.corners != Some(corners) {
                    self.corners = Some(corners);
                    tasks.push(set_surface_corners(self.surface_id, corners));
                }

                if let Some(handler) = &self.config_handler {
                    if let Ok(config) = Config::get_entry(handler) {
                        if config != self.config {
                            let position_changed = config.widget_x != self.config.widget_x
                                || config.widget_y != self.config.widget_y;
                            self.sampler.set_interval(config.update_interval_ms);
                            self.sampler.set_weather_config(
                                config.show_weather,
                                config.weather_location.clone(),
                            );
                            if config.cider_api_token != self.config.cider_api_token {
                                self.sampler.set_cider_token(config.cider_api_token.clone());
                            }
                            self.config = config;

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
                }

                let frosted = frosted_enabled();
                let surface_height = desired_surface_height_with_animation(
                    &self.config,
                    &self.snapshot,
                    self.expanded_notification.as_ref(),
                    self.expanded_notification_group.as_deref(),
                    self.notification_expansion.progress,
                    self.notification_group_expansion.progress,
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
                    tasks.push(set_surface_blur(
                        self.surface_id,
                        frosted,
                        surface_height,
                    ));
                }
            }
            Message::AnimationTick => {
                let now = Instant::now();
                let was_animating = self.animations_active();
                self.notification_expansion.advance(now);
                self.notification_group_expansion.advance(now);
                self.notification_scroll.advance(now);
                let mut completed_dismissals = Vec::new();
                self.dismissing_notifications.retain_mut(|dismissal| {
                    dismissal.animation.advance(now);
                    let completed = !dismissal.animation.is_animating()
                        && dismissal.animation.target == 1.0;
                    if completed {
                        completed_dismissals.push((
                            dismissal.key.clone(),
                            dismissal.source.clone(),
                        ));
                    }
                    !completed
                });

                for (key, source) in completed_dismissals {
                    self.sampler
                        .dismiss_notification(&key.app_name, key.timestamp);
                    self.snapshot.notifications.retain(|notification| {
                        !key.matches(notification)
                    });
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
                self.sampler.clear_notifications();
                self.snapshot.notifications.clear();
                self.expanded_notification_group = None;
                self.expanded_notification = None;
                self.notification_group_expansion.reset();
                self.notification_expansion.reset();
                self.dismissing_notifications.clear();
                self.notification_scroll = ScrollAnimation::default();
                let target = desired_surface_height(&self.config, &self.snapshot);
                if target != self.surface_height {
                    self.surface_height = target;
                    tasks.push(set_surface_regions(self.surface_id, target, self.frosted));
                }
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
            Message::ToggleNotification {
                app_name,
                timestamp,
            } => {
                let selected = NotificationKey {
                    app_name,
                    timestamp,
                };
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
            Message::DismissNotification {
                app_name,
                timestamp,
            } => {
                let key = NotificationKey {
                    app_name,
                    timestamp,
                };
                if self
                    .dismissing_notifications
                    .iter()
                    .any(|dismissal| dismissal.key == key)
                {
                    return Task::none();
                }
                let Some(source) = self
                    .snapshot
                    .notifications
                    .iter()
                    .find(|notification| key.matches(notification))
                    .map(notification_source)
                    .map(str::to_string)
                else {
                    return Task::none();
                };
                let mut animation = ExpansionAnimation::default();
                animation.transition_to(1.0, Instant::now());
                self.dismissing_notifications.push(DismissingNotification {
                    key,
                    source,
                    animation,
                });
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
                self.snapshot.media = self.sampler.media_state();
                reconcile_media_state(
                    &mut self.snapshot.media,
                    &mut self.pending_playback,
                    Instant::now(),
                );
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
                self.snapshot.media = self.sampler.media_state();
                reconcile_media_state(
                    &mut self.snapshot.media,
                    &mut self.pending_playback,
                    Instant::now(),
                );
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
                    self.snapshot.media = self.sampler.media_state();
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
            self.notification_group_expansion.progress,
            self.notification_group_expansion.target > 0.0,
            self.notification_expansion.progress,
            &self.dismissing_notifications,
            self.notification_scroll.translation(),
            self.surface_height,
            self.media_seek_preview,
            self.media_timeline_hovered,
        )
    }

    fn subscription(&self) -> Subscription<Message> {
        let stats = Subscription::run_with(self.ui_tick_interval(), aligned_tick_stream)
            .map(|_| Message::Tick);

        if self.animations_active() {
            Subscription::batch([
                stats,
                iced::window::frames().map(|_| Message::AnimationTick),
            ])
        } else {
            stats
        }
    }

    fn animations_active(&self) -> bool {
        self.notification_expansion.is_animating()
            || self.notification_group_expansion.is_animating()
            || self
                .dismissing_notifications
                .iter()
                .any(|dismissal| dismissal.animation.is_animating())
            || self.notification_scroll.is_animating()
    }

    fn ui_tick_interval(&self) -> Duration {
        ui_tick_interval(self.config.update_interval_ms)
    }

    fn target_surface_height(&self) -> u32 {
        desired_surface_height_with_animation(
            &self.config,
            &self.snapshot,
            self.expanded_notification.as_ref(),
            self.expanded_notification_group.as_deref(),
            self.notification_expansion.target,
            self.notification_group_expansion.target,
        )
    }
}

fn ui_tick_interval(configured_ms: u64) -> Duration {
    Duration::from_millis(configured_ms).clamp(MIN_UI_TICK_INTERVAL, MAX_UI_TICK_INTERVAL)
}

fn aligned_tick_stream(interval: &Duration) -> impl iced::futures::Stream<Item = ()> + use<> {
    let interval = *interval;

    iced::stream::channel(1, async move |mut output| {
        loop {
            let elapsed = SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap_or_default();
            tokio::time::sleep(delay_until_next_tick(elapsed, interval)).await;

            if output.send(()).await.is_err() {
                break;
            }
        }
    })
}

fn delay_until_next_tick(elapsed: Duration, interval: Duration) -> Duration {
    let interval_ns = interval.as_nanos().max(1);
    let remainder_ns = elapsed.as_nanos() % interval_ns;
    let until_boundary_ns = if remainder_ns == 0 {
        interval_ns
    } else {
        interval_ns - remainder_ns
    };
    let until_boundary = Duration::from_nanos(u64::try_from(until_boundary_ns).unwrap_or(u64::MAX));

    until_boundary.saturating_add(UI_TICK_SETTLE_DELAY)
}

fn reconcile_media_state(
    state: &mut MultiPlayerState,
    pending_playback: &mut Option<PendingPlayback>,
    now: Instant,
) {
    if pending_playback
        .as_ref()
        .is_some_and(|pending| pending.expires_at <= now)
    {
        *pending_playback = None;
    }

    if let Some(pending) = pending_playback.as_ref()
        && let Some((_, info)) = state
            .players
            .iter_mut()
            .find(|(id, _)| id == &pending.player_id)
    {
        info.status = pending.status.clone();
    }
}

fn system_theme() -> cosmic::Theme {
    let mut theme = cosmic::theme::system_preference();
    theme.transparent = theme.cosmic().frosted_system_interface;
    theme
}

fn frosted_enabled() -> bool {
    cosmic::theme::system_preference()
        .cosmic()
        .frosted_system_interface
}

fn overlay_corners() -> CornerRadius {
    let radius = cosmic::theme::system_preference()
        .cosmic()
        .corner_radii
        .radius_l;

    CornerRadius {
        top_left: radius[0].round() as u32,
        top_right: radius[1].round() as u32,
        bottom_right: radius[2].round() as u32,
        bottom_left: radius[3].round() as u32,
    }
}

fn set_surface_corners(id: window::Id, corners: CornerRadius) -> Task<Message> {
    corner_radius::corner_radius(id, Some(corners)).discard()
}

fn set_surface_blur(id: window::Id, enabled: bool, height: u32) -> Task<Message> {
    let region = enabled.then(|| vec![surface_region(height)]);

    blur::blur(id, region).discard()
}

fn set_surface_regions(id: window::Id, height: u32, frosted: bool) -> Task<Message> {
    Task::batch([
        set_surface_input_zone(id, height),
        set_surface_blur(id, frosted, height),
    ])
}

fn surface_region(height: u32) -> Rectangle {
    Rectangle::new(
        Point::ORIGIN,
        Size::new(SURFACE_WIDTH as f32, height as f32),
    )
}

fn set_surface_input_zone(id: window::Id, height: u32) -> Task<Message> {
    cosmic::iced::runtime::task::effect(
        cosmic::iced::runtime::Action::PlatformSpecific(
            cosmic::iced::runtime::platform_specific::Action::Wayland(
                wayland::Action::LayerSurface(wayland::layer_surface::Action::InputZone {
                    id,
                    zone: Some(vec![surface_region(height)]),
                }),
            ),
        ),
    )
}

fn desired_surface_height(config: &Config, snapshot: &SystemSnapshot) -> u32 {
    desired_surface_height_with_expansion(config, snapshot, None, None)
}

fn desired_surface_height_with_expansion(
    config: &Config,
    snapshot: &SystemSnapshot,
    expanded_notification: Option<&NotificationKey>,
    expanded_notification_group: Option<&str>,
) -> u32 {
    desired_surface_height_with_animation(
        config,
        snapshot,
        expanded_notification,
        expanded_notification_group,
        if expanded_notification.is_some() { 1.0 } else { 0.0 },
        if expanded_notification_group.is_some() { 1.0 } else { 0.0 },
    )
}

fn desired_surface_height_with_animation(
    config: &Config,
    snapshot: &SystemSnapshot,
    expanded_notification: Option<&NotificationKey>,
    expanded_notification_group: Option<&str>,
    notification_progress: f32,
    group_progress: f32,
) -> u32 {
    let mut height = BASE_SURFACE_HEIGHT as f32;
    let storage_visible = config.show_storage
        && config
            .section_order
            .iter()
            .any(|section| matches!(section, WidgetSection::Storage));

    if storage_visible {
        let storage_height = if snapshot.disks.is_empty() {
            EMPTY_STORAGE_HEIGHT
        } else {
            STORAGE_SECTION_HEIGHT + STORAGE_ITEM_HEIGHT.saturating_mul(snapshot.disks.len() as u32)
        };
        height += storage_height as f32;
    }

    let devices_visible = config.show_battery
        && config
            .section_order
            .iter()
            .any(|section| matches!(section, WidgetSection::Battery));

    if devices_visible {
        let devices_height = if snapshot.devices.is_empty() {
            EMPTY_DEVICES_HEIGHT
        } else {
            DEVICES_SECTION_HEIGHT
                + DEVICE_ITEM_HEIGHT.saturating_mul(snapshot.devices.len() as u32)
        };
        height += devices_height as f32;
    }

    let weather_visible = config.show_weather
        && config
            .section_order
            .iter()
            .any(|section| matches!(section, WidgetSection::Weather));

    if weather_visible {
        let weather_height = if snapshot.weather.is_some() {
            WEATHER_SECTION_HEIGHT
        } else {
            EMPTY_WEATHER_HEIGHT
        };
        height += weather_height as f32;
    }

    let notifications_visible = config.show_notifications
        && config
            .section_order
            .iter()
            .any(|section| matches!(section, WidgetSection::Notifications));

    if notifications_visible {
        let notifications_height = if snapshot.notifications.is_empty() {
            EMPTY_NOTIFICATIONS_HEIGHT
        } else {
            NOTIFICATIONS_SECTION_HEIGHT
                + notification_viewport_height_with_animation(
                    snapshot,
                    expanded_notification,
                    expanded_notification_group,
                    notification_progress,
                    group_progress,
                )
                .round() as u32
        };
        height += notifications_height as f32;
    }

    let media_visible = config.show_media
        && config
            .section_order
            .iter()
            .any(|section| matches!(section, WidgetSection::Media));

    if media_visible {
        let media_height = if snapshot
            .media
            .current_player()
            .is_some_and(|(_, info)| info.is_active())
        {
            MEDIA_SECTION_HEIGHT
        } else {
            EMPTY_MEDIA_HEIGHT
        };
        height += media_height as f32;
    }

    height.round() as u32
}

fn notification_source(notification: &crate::notifications::Notification) -> &str {
    if notification.app_name.trim().is_empty()
        || notification.app_name.eq_ignore_ascii_case("system")
    {
        notification.summary.trim()
    } else {
        notification.app_name.trim()
    }
}

fn notification_group_size(snapshot: &SystemSnapshot, source: &str) -> usize {
    snapshot
        .notifications
        .iter()
        .filter(|notification| notification_source(notification) == source)
        .count()
}

fn notification_display_rows(snapshot: &SystemSnapshot, expanded_group: Option<&str>) -> u32 {
    let mut groups: Vec<(&str, u32)> = Vec::new();

    for notification in &snapshot.notifications {
        let source = notification_source(notification);
        if let Some((_, count)) = groups
            .iter_mut()
            .find(|(group_source, _)| *group_source == source)
        {
            *count += 1;
        } else {
            groups.push((source, 1));
        }
    }

    groups
        .into_iter()
        .map(|(source, count)| {
            if count > 1 && expanded_group == Some(source) {
                count + 1
            } else {
                1
            }
        })
        .sum()
}

fn notification_viewport_height(
    snapshot: &SystemSnapshot,
    expanded_notification: Option<&NotificationKey>,
    expanded_group: Option<&str>,
) -> u32 {
    notification_viewport_height_with_animation(
        snapshot,
        expanded_notification,
        expanded_group,
        if expanded_notification.is_some() { 1.0 } else { 0.0 },
        if expanded_group.is_some() { 1.0 } else { 0.0 },
    )
    .round() as u32
}

fn notification_viewport_height_with_animation(
    snapshot: &SystemSnapshot,
    expanded_notification: Option<&NotificationKey>,
    expanded_group: Option<&str>,
    notification_progress: f32,
    group_progress: f32,
) -> f32 {
    let compact_rows = notification_display_rows(snapshot, None) as f32;
    let group_rows = expanded_group
        .map(|source| notification_group_size(snapshot, source) as f32)
        .unwrap_or(0.0)
        * group_progress.clamp(0.0, 1.0);
    let selected_group_progress = expanded_notification
        .and_then(|key| {
            snapshot
                .notifications
                .iter()
                .find(|notification| key.matches(notification))
        })
        .map(|notification| {
            let source = notification_source(notification);
            if notification_group_size(snapshot, source) > 1 {
                group_progress.clamp(0.0, 1.0)
            } else {
                1.0
            }
        })
        .unwrap_or(1.0);
    let expanded_height = expanded_notification_extra_height(snapshot, expanded_notification)
        as f32
        * notification_progress.clamp(0.0, 1.0)
        * selected_group_progress;
    let content_height = NOTIFICATION_ITEM_HEIGHT as f32 * (compact_rows + group_rows)
        + expanded_height;

    content_height.min((NOTIFICATION_ITEM_HEIGHT * MAX_VISIBLE_NOTIFICATION_ROWS) as f32)
}

fn expanded_notification_extra_height(
    snapshot: &SystemSnapshot,
    expanded: Option<&NotificationKey>,
) -> u32 {
    let Some(notification) = expanded.and_then(|key| {
        snapshot
            .notifications
            .iter()
            .find(|notification| key.matches(notification))
    }) else {
        return 0;
    };
    notification_extra_height(notification)
}

fn notification_extra_height(notification: &crate::notifications::Notification) -> u32 {
    let summary = if notification.summary.trim().is_empty() {
        &notification.app_name
    } else {
        &notification.summary
    };
    let body = if notification.body.trim().is_empty() {
        &notification.app_name
    } else {
        &notification.body
    };
    let lines = estimated_wrapped_lines(summary, NOTIFICATION_CHARS_PER_LINE)
        + estimated_wrapped_lines(body, NOTIFICATION_CHARS_PER_LINE);

    lines
        .saturating_sub(2)
        .saturating_mul(NOTIFICATION_LINE_HEIGHT)
}

fn estimated_wrapped_lines(text: &str, line_width: usize) -> u32 {
    text.lines()
        .map(|line| {
            let characters = line.trim().chars().count().max(1);
            characters.div_ceil(line_width) as u32
        })
        .sum::<u32>()
        .max(1)
}

#[cfg(test)]
mod tests {
    use super::{
        BASE_SURFACE_HEIGHT, ExpansionAnimation, NOTIFICATION_EXPANSION_DURATION,
        NotificationKey, PendingPlayback, ScrollAnimation, UI_TICK_SETTLE_DELAY,
        delay_until_next_tick, desired_surface_height, desired_surface_height_with_expansion,
        notification_viewport_height_with_animation, reconcile_media_state, ui_tick_interval,
    };
    use crate::battery::BatteryDevice;
    use crate::config::{Config, WidgetSection};
    use crate::media::{MediaInfo, PlaybackStatus, PlayerId};
    use crate::notifications::Notification;
    use crate::storage::DiskInfo;
    use crate::weather::WeatherData;
    use std::time::{Duration, Instant};

    #[test]
    fn ui_ticks_follow_the_configured_rate_without_exceeding_one_second() {
        assert_eq!(ui_tick_interval(100), Duration::from_millis(250));
        assert_eq!(ui_tick_interval(500), Duration::from_millis(500));
        assert_eq!(ui_tick_interval(5_000), Duration::from_secs(1));
    }

    #[test]
    fn ui_ticks_align_just_after_the_next_epoch_boundary() {
        let interval = Duration::from_secs(1);

        assert_eq!(
            delay_until_next_tick(Duration::from_millis(1_250), interval),
            Duration::from_millis(750) + UI_TICK_SETTLE_DELAY,
        );
        assert_eq!(
            delay_until_next_tick(Duration::from_secs(2), interval),
            interval + UI_TICK_SETTLE_DELAY,
        );
    }

    #[test]
    fn surface_height_tracks_visible_storage_rows() {
        let mut config = Config::default();
        config.show_storage = true;
        config.section_order = vec![WidgetSection::Storage];
        let mut snapshot = super::SystemSnapshot::default();

        let empty_height = desired_surface_height(&config, &snapshot);
        snapshot.disks = vec![disk(), disk(), disk()];

        assert!(empty_height > BASE_SURFACE_HEIGHT);
        assert_eq!(desired_surface_height(&config, &snapshot), 780);
    }

    #[test]
    fn surface_height_tracks_visible_device_rows() {
        let mut config = Config::default();
        config.show_storage = false;
        config.show_battery = true;
        config.section_order = vec![WidgetSection::Battery];
        let mut snapshot = super::SystemSnapshot::default();

        let empty_height = desired_surface_height(&config, &snapshot);
        snapshot.devices = vec![device(), device(), device()];

        assert!(empty_height > BASE_SURFACE_HEIGHT);
        assert_eq!(desired_surface_height(&config, &snapshot), 709);
    }

    #[test]
    fn surface_height_tracks_loaded_weather_content() {
        let mut config = Config::default();
        config.show_storage = false;
        config.show_weather = true;
        config.section_order = vec![WidgetSection::Weather];
        let mut snapshot = super::SystemSnapshot::default();

        let loading_height = desired_surface_height(&config, &snapshot);
        snapshot.weather = Some(weather());

        assert!(loading_height > BASE_SURFACE_HEIGHT);
        assert_eq!(desired_surface_height(&config, &snapshot), 710);
    }

    #[test]
    fn surface_height_groups_notifications_and_caps_the_viewport() {
        let mut config = Config::default();
        config.show_storage = false;
        config.show_notifications = true;
        config.section_order = vec![WidgetSection::Notifications];
        let mut snapshot = super::SystemSnapshot::default();

        let empty_height = desired_surface_height(&config, &snapshot);
        snapshot.notifications = vec![notification(), notification(), notification()];

        assert!(empty_height > BASE_SURFACE_HEIGHT);
        assert_eq!(desired_surface_height(&config, &snapshot), 668);
        assert_eq!(
            desired_surface_height_with_expansion(
                &config,
                &snapshot,
                None,
                Some("Package manager updated")
            ),
            809
        );

        snapshot.notifications = (0..5)
            .map(|index| {
                let mut item = notification();
                item.app_name = format!("App {index}");
                item
            })
            .collect();
        assert_eq!(desired_surface_height(&config, &snapshot), 809);
    }

    #[test]
    fn surface_height_grows_only_after_notification_selection() {
        let mut config = Config::default();
        config.show_storage = false;
        config.show_notifications = true;
        config.section_order = vec![WidgetSection::Notifications];
        let mut snapshot = super::SystemSnapshot::default();
        let mut item = notification();
        item.body = "A complete notification body that wraps across several lines so all of its content remains readable when expanded.".to_string();
        let selected = NotificationKey {
            app_name: item.app_name.clone(),
            timestamp: item.timestamp,
        };
        snapshot.notifications = vec![item];
        let compact_height = desired_surface_height(&config, &snapshot);

        assert!(
            desired_surface_height_with_expansion(&config, &snapshot, Some(&selected), None)
                > compact_height
        );
    }

    #[test]
    fn notification_expansion_uses_an_eased_reversible_transition() {
        let started = Instant::now();
        let mut animation = ExpansionAnimation::default();

        animation.transition_to(1.0, started);
        animation.advance(started + NOTIFICATION_EXPANSION_DURATION / 2);
        assert!((animation.progress - 0.5).abs() < 0.01);

        animation.transition_to(0.0, started + NOTIFICATION_EXPANSION_DURATION / 2);
        animation.advance(started + NOTIFICATION_EXPANSION_DURATION * 2);
        assert_eq!(animation.progress, 0.0);
        assert!(animation.is_collapsed());
    }

    #[test]
    fn notification_group_expansion_uses_a_slower_stable_transition() {
        let started = Instant::now();
        let mut animation = ExpansionAnimation::with_duration(
            super::NOTIFICATION_GROUP_EXPANSION_DURATION,
        );

        animation.transition_to(1.0, started);
        animation.advance(started + NOTIFICATION_EXPANSION_DURATION);
        assert!(animation.progress < 1.0);
        assert!(animation.is_animating());

        animation.advance(started + super::NOTIFICATION_GROUP_EXPANSION_DURATION);
        assert_eq!(animation.progress, 1.0);
        assert!(!animation.is_animating());
    }

    #[test]
    fn notification_group_expansion_reverses_from_its_current_progress() {
        let started = Instant::now();
        let duration = super::NOTIFICATION_GROUP_EXPANSION_DURATION;
        let mut animation = ExpansionAnimation::with_duration(duration);

        animation.transition_to(1.0, started);
        animation.advance(started + duration / 2);
        assert!((animation.progress - 0.5).abs() < 0.01);

        let reversed_at = started + duration / 2;
        animation.transition_to(0.0, reversed_at);
        animation.advance(reversed_at + Duration::from_millis(16));
        assert!(animation.progress <= 0.46);

        animation.advance(reversed_at + duration / 2);

        assert_eq!(animation.progress, 0.0);
        assert!(animation.is_collapsed());
    }

    #[test]
    fn notification_group_viewport_grows_with_animation_progress() {
        let mut snapshot = super::SystemSnapshot::default();
        snapshot.notifications = vec![notification(), notification(), notification()];

        let compact = notification_viewport_height_with_animation(
            &snapshot,
            None,
            Some("Package manager updated"),
            0.0,
            0.0,
        );
        let halfway = notification_viewport_height_with_animation(
            &snapshot,
            None,
            Some("Package manager updated"),
            0.0,
            0.5,
        );
        let expanded = notification_viewport_height_with_animation(
            &snapshot,
            None,
            Some("Package manager updated"),
            0.0,
            1.0,
        );

        assert!(compact < halfway);
        assert!(halfway < expanded);
    }

    #[test]
    fn notification_scroll_eases_to_the_new_offset() {
        let started = Instant::now();
        let mut animation = ScrollAnimation::default();

        animation.transition_to(60.0, started);
        assert_eq!(animation.translation(), 60.0);

        animation.advance(started + NOTIFICATION_EXPANSION_DURATION / 2);
        assert!((animation.translation() - 30.0).abs() < 0.5);

        animation.advance(started + NOTIFICATION_EXPANSION_DURATION);
        assert_eq!(animation.translation(), 0.0);
        assert!(!animation.is_animating());
    }

    #[test]
    fn notification_scroll_can_snap_during_group_layout_changes() {
        let started = Instant::now();
        let mut animation = ScrollAnimation::default();
        animation.transition_to(60.0, started);

        animation.snap_to(24.0);

        assert_eq!(animation.translation(), 0.0);
        assert!(!animation.is_animating());
    }

    #[test]
    fn surface_height_tracks_active_media_content() {
        let mut config = Config::default();
        config.show_storage = false;
        config.show_media = true;
        config.section_order = vec![WidgetSection::Media];
        let mut snapshot = super::SystemSnapshot::default();

        let empty_height = desired_surface_height(&config, &snapshot);
        snapshot.media.players.push((
            PlayerId::Mpris("org.mpris.MediaPlayer2.firefox".to_string()),
            media(),
        ));

        assert_eq!(empty_height, 639);
        assert_eq!(desired_surface_height(&config, &snapshot), 804);

        snapshot.media.players.push((
            PlayerId::Mpris("org.mpris.MediaPlayer2.cider".to_string()),
            media(),
        ));
        assert_eq!(desired_surface_height(&config, &snapshot), 804);
    }

    #[test]
    fn media_reconciliation_preserves_immediate_user_choices() {
        let firefox = PlayerId::Mpris("org.mpris.MediaPlayer2.firefox".to_string());
        let cider = PlayerId::Cider;
        let mut state = crate::media::MultiPlayerState {
            players: vec![(firefox, media()), (cider.clone(), media())],
            current_index: 0,
        };
        let mut pending = Some(PendingPlayback {
            player_id: cider,
            status: PlaybackStatus::Paused,
            expires_at: Instant::now() + Duration::from_secs(1),
        });

        state.current_index = 1;
        reconcile_media_state(&mut state, &mut pending, Instant::now());

        assert_eq!(state.current_index, 1);
        assert_eq!(
            state.current_player().unwrap().1.status,
            PlaybackStatus::Paused
        );
    }

    #[test]
    fn media_reconciliation_releases_expired_playback_state() {
        let now = Instant::now();
        let mut state = crate::media::MultiPlayerState {
            players: vec![(PlayerId::Cider, media())],
            current_index: 0,
        };
        let mut pending = Some(PendingPlayback {
            player_id: PlayerId::Cider,
            status: PlaybackStatus::Paused,
            expires_at: now,
        });

        reconcile_media_state(&mut state, &mut pending, now);

        assert!(pending.is_none());
        assert_eq!(
            state.current_player().unwrap().1.status,
            PlaybackStatus::Playing
        );
    }

    fn disk() -> DiskInfo {
        DiskInfo {
            name: "Disk".to_string(),
            mount_point: "/".to_string(),
            used_percentage: 50.0,
            total_space: 1_000,
            available_space: 500,
            is_loading: false,
        }
    }

    fn device() -> BatteryDevice {
        BatteryDevice {
            name: "Device".to_string(),
            level: Some(75),
            status: Some("discharging".to_string()),
            kind: Some("mouse".to_string()),
            codename: None,
            is_loading: false,
            is_connected: true,
        }
    }

    fn weather() -> WeatherData {
        WeatherData {
            temperature: 3.2,
            feels_like: 1.0,
            temp_min: 3.2,
            temp_max: 3.2,
            humidity: 80,
            description: "Overcast".to_string(),
            icon: "04d".to_string(),
            location: "Milwaukee".to_string(),
        }
    }

    fn notification() -> Notification {
        Notification {
            id: None,
            server_owner: None,
            app_name: "System".to_string(),
            summary: "Package manager updated".to_string(),
            body: "System is up to date.".to_string(),
            timestamp: 1_000,
        }
    }

    fn media() -> MediaInfo {
        MediaInfo {
            player_name: "Firefox".to_string(),
            title: "Making Minecraft fun again".to_string(),
            artist: "Call Me Kevin".to_string(),
            status: PlaybackStatus::Playing,
            position: 74_000,
            duration: 1_769_000,
            can_play: true,
            can_pause: true,
            can_go_next: true,
            can_go_previous: true,
            can_seek: true,
            ..Default::default()
        }
    }
}
