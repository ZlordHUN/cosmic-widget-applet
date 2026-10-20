// SPDX-License-Identifier: MPL-2.0

use crate::monitors::media::{AlbumArt, MediaInfo, PlaybackStatus, PlayerId, SubtitleState};
use crate::overlay::Message;
use crate::overlay::components::{
    button_style, marquee, media_artwork as rounded_artwork, media_slider,
    section::{compact_single_line, section},
    slide,
};
use crate::overlay::media_state::DismissingMedia;
use crate::overlay::stats::SystemSnapshot;
use cosmic::iced::{Alignment, Background, Border, Color, ContentFit, Length};
use cosmic::{Element, theme, widget};
use std::collections::HashMap;
use std::sync::{Mutex, OnceLock};

mod subtitle_labels;
mod subtitle_theme;

const MEDIA_ACTIVE_SECTION_HEIGHT: f32 = 232.0;
const MEDIA_CONTENT_HEIGHT: f32 = 204.0;
pub(in crate::overlay) const MEDIA_EMPTY_CONTENT_HEIGHT: f32 = 76.0;
const MEDIA_EMPTY_ARTWORK_SIZE: f32 = 56.0;
const MEDIA_SOURCE_SELECTOR_HEIGHT: f32 = 34.0;
const MEDIA_SOURCE_BUTTON_SIZE: f32 = 34.0;
const MEDIA_SOURCE_DOT_SIZE: f32 = 12.0;
const MEDIA_ARTWORK_SIZE: f32 = 96.0;
const MEDIA_VIDEO_ARTWORK_WIDTH: f32 = 128.0;
const MEDIA_VIDEO_ARTWORK_HEIGHT: f32 = 72.0;
const MEDIA_CONTENT_SPACING: u16 = 12;
const MEDIA_CONTROL_ICON_SIZE: u16 = 20;
const MEDIA_SEEK_ICON_SIZE: u16 = 24;
const MEDIA_CONTROL_PADDING: u16 = 6;
const MEDIA_CONTROL_SPACING: u16 = 8;
const MEDIA_SEEK_INTERVAL_MS: i64 = 10_000;
const MEDIA_SUBTITLE_CONTROL_SIZE: f32 = 28.0;
const MEDIA_SUBTITLE_DROPDOWN_WIDTH: f32 = 112.0;
// Reserve both horizontal insets, the 12px arrow, and a 4px text/arrow gap.
const MEDIA_SUBTITLE_TEXT_WIDTH: f32 = MEDIA_SUBTITLE_DROPDOWN_WIDTH - 12.0 - 12.0 - 4.0;
const MEDIA_TIMELINE_FOOTER_GAP: f32 = 4.0;
const MEDIA_VOLUME_ICON_SIZE: u16 = 18;
const MEDIA_VOLUME_SLIDER_WIDTH: f32 = 100.0;

pub(in crate::overlay) fn media_view<'a>(
    stats: &'a SystemSnapshot,
    dismissing: Option<&'a DismissingMedia>,
    seek_preview: Option<f64>,
    volume_preview: Option<f64>,
    section_spacing: u16,
    content_spacing: u16,
    detail_spacing: u16,
    timeline_hovered: bool,
) -> Element<'a, Message> {
    let state = dismissing.map_or(&stats.media, |outgoing| &outgoing.state);
    if let Some((player_id, info)) = state.current_player().filter(|(_, info)| info.is_active()) {
        let content = media_content(
            info,
            state,
            seek_preview,
            content_spacing,
            detail_spacing,
            timeline_hovered,
        );
        let content = match dismissing {
            Some(outgoing) => slide::left(content, outgoing.animation.progress),
            None => content,
        };
        let volume = info.volume.filter(|_| dismissing.is_none()).map(|volume| {
            volume_preview
                .filter(|value| value.is_finite())
                .unwrap_or(volume)
                .clamp(0.0, 1.0)
        });
        return widget::column::with_capacity(2)
            .spacing(section_spacing)
            .height(Length::Fixed(MEDIA_ACTIVE_SECTION_HEIGHT))
            .push(media_heading(player_id, volume))
            .push(content)
            .into();
    }

    section("emblem-music-symbolic", "Now Playing", section_spacing)
        .push(empty_media_card())
        .into()
}

fn media_heading(player_id: &PlayerId, volume: Option<f64>) -> Element<'static, Message> {
    let mut heading = widget::row::with_capacity(4)
        .width(Length::Fill)
        .align_y(Alignment::Center)
        .spacing(8)
        .push(widget::icon::from_name("emblem-music-symbolic").size(18))
        .push(widget::text::heading("Now Playing"));
    if let Some(volume) = volume {
        heading = heading
            .push(widget::space::horizontal())
            .push(media_volume_control(player_id, volume));
    }
    heading.into()
}

fn media_volume_control(player_id: &PlayerId, volume: f64) -> Element<'static, Message> {
    use std::hash::{Hash, Hasher};

    let changed_player = player_id.clone();
    let slider = media_slider::volume(
        volume,
        move |value| Message::MediaVolumeChanged(changed_player.clone(), value),
        Message::CommitMediaVolume(player_id.clone()),
    );
    let volume_control = widget::row::with_capacity(2)
        .align_y(Alignment::Center)
        .spacing(6)
        .push(widget::icon::from_name(volume_icon(volume)).size(MEDIA_VOLUME_ICON_SIZE))
        .push(widget::container(slider).width(Length::Fixed(MEDIA_VOLUME_SLIDER_WIDTH)));
    let control = widget::tooltip(
        volume_control,
        widget::text::caption(format!("Volume {}%", (volume * 100.0).round() as u32)),
        widget::tooltip::Position::Top,
    )
    .into();
    let mut hasher = std::collections::hash_map::DefaultHasher::new();
    player_id.hash(&mut hasher);
    // Preserve a drag through normal volume polls; a source switch must cancel
    // it before the native slider receives callbacks for the newly shown player.
    media_control_scope::view(hasher.finish(), control)
}

fn volume_icon(volume: f64) -> &'static str {
    if volume <= 0.0 {
        "audio-volume-muted-symbolic"
    } else if volume < 0.34 {
        "audio-volume-low-symbolic"
    } else if volume < 0.67 {
        "audio-volume-medium-symbolic"
    } else {
        "audio-volume-high-symbolic"
    }
}

fn empty_media_card() -> Element<'static, Message> {
    let note = widget::icon::from_name("emblem-music-symbolic")
        .size(32)
        .icon()
        .class(theme::Svg::custom(|theme| widget::svg::Style {
            color: Some(theme.current_container().on.into()),
        }))
        .opacity(0.38);
    let artwork = widget::container(note)
        .center_x(Length::Fixed(MEDIA_EMPTY_ARTWORK_SIZE))
        .center_y(Length::Fixed(MEDIA_EMPTY_ARTWORK_SIZE))
        .class(theme::Container::custom(|theme| {
            let foreground: Color = theme.current_container().on.into();
            cosmic::iced::widget::container::Style {
                background: Some(Background::Color(Color {
                    a: 0.09,
                    ..foreground
                })),
                border: Border {
                    radius: 10.0.into(),
                    ..Default::default()
                },
                ..Default::default()
            }
        }));
    let hint = widget::container(widget::text::caption("Play something to see it here.")).class(
        theme::Container::custom(|theme| {
            let foreground: Color = theme.current_container().on.into();
            cosmic::iced::widget::container::Style {
                text_color: Some(Color {
                    a: 0.55,
                    ..foreground
                }),
                ..Default::default()
            }
        }),
    );
    let caption = widget::column::with_capacity(2)
        .width(Length::Fill)
        .spacing(4)
        .push(widget::text::body("No media playing"))
        .push(hint);
    let content = widget::row::with_capacity(2)
        .width(Length::Fill)
        .align_y(Alignment::Center)
        .spacing(12)
        .push(artwork)
        .push(caption);

    widget::container(content)
        .width(Length::Fill)
        .height(Length::Fixed(MEDIA_EMPTY_CONTENT_HEIGHT))
        .padding(10)
        .class(theme::Container::custom(|theme| {
            let foreground: Color = theme.current_container().on.into();
            cosmic::iced::widget::container::Style {
                text_color: Some(Color {
                    a: 0.85,
                    ..foreground
                }),
                background: Some(Background::Color(Color {
                    a: if theme.transparent { 0.035 } else { 0.055 },
                    ..foreground
                })),
                border: Border {
                    color: Color {
                        a: 0.08,
                        ..foreground
                    },
                    width: 1.0,
                    radius: 12.0.into(),
                },
                ..Default::default()
            }
        }))
        .into()
}

fn media_content<'a>(
    info: &'a MediaInfo,
    player_state: &'a crate::monitors::media::MultiPlayerState,
    seek_preview: Option<f64>,
    _content_spacing: u16,
    detail_spacing: u16,
    timeline_hovered: bool,
) -> Element<'a, Message> {
    let progress = seek_preview
        .unwrap_or_else(|| info.progress())
        .clamp(0.0, 1.0);
    let position = if seek_preview.is_some() && info.duration > 0 {
        (info.duration as f64 * progress) as u64
    } else {
        info.position
    };
    let subtitles = emby_subtitles(info);
    let control_icon_size = if subtitles.is_some() {
        18
    } else {
        MEDIA_CONTROL_ICON_SIZE
    };
    let seek_icon_size = if subtitles.is_some() {
        20
    } else {
        MEDIA_SEEK_ICON_SIZE
    };
    let control_spacing = if subtitles.is_some() {
        4
    } else {
        MEDIA_CONTROL_SPACING
    };
    let previous = widget::button::icon(
        widget::icon::from_name("media-skip-backward-symbolic").size(control_icon_size),
    )
    .icon_size(control_icon_size)
    .class(button_style::accent_hover(|| theme::Button::Icon))
    .tooltip("Previous track")
    .padding(MEDIA_CONTROL_PADDING)
    .on_press_maybe(info.can_go_previous.then_some(Message::PreviousMedia));
    let play_pause_icon = match info.status {
        PlaybackStatus::Playing => "media-playback-pause-symbolic",
        PlaybackStatus::Paused | PlaybackStatus::Stopped => "media-playback-start-symbolic",
    };
    let play_pause =
        widget::button::icon(widget::icon::from_name(play_pause_icon).size(control_icon_size))
            .icon_size(control_icon_size)
            .class(button_style::accent_hover(|| theme::Button::Icon))
            .tooltip(match info.status {
                PlaybackStatus::Playing => "Pause",
                PlaybackStatus::Paused | PlaybackStatus::Stopped => "Play",
            })
            .padding(MEDIA_CONTROL_PADDING)
            .on_press_maybe((info.can_play || info.can_pause).then_some(Message::PlayPauseMedia));
    let next = widget::button::icon(
        widget::icon::from_name("media-skip-forward-symbolic").size(control_icon_size),
    )
    .icon_size(control_icon_size)
    .class(button_style::accent_hover(|| theme::Button::Icon))
    .tooltip("Next track")
    .padding(MEDIA_CONTROL_PADDING)
    .on_press_maybe(info.can_go_next.then_some(Message::NextMedia));
    let back = widget::button::icon(
        widget::icon::from_svg_bytes(include_bytes!(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/assets/icons/material-design/replay_10.svg"
        )))
        .symbolic(true),
    )
    .icon_size(seek_icon_size)
    .class(button_style::accent_hover(|| theme::Button::Icon))
    .tooltip("Back 10 seconds")
    .padding(MEDIA_CONTROL_PADDING)
    .on_press_maybe(
        info.can_seek
            .then_some(Message::SeekMediaRelative(-MEDIA_SEEK_INTERVAL_MS)),
    );
    let forward = widget::button::icon(
        widget::icon::from_svg_bytes(include_bytes!(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/assets/icons/material-design/forward_10.svg"
        )))
        .symbolic(true),
    )
    .icon_size(seek_icon_size)
    .class(button_style::accent_hover(|| theme::Button::Icon))
    .tooltip("Forward 10 seconds")
    .padding(MEDIA_CONTROL_PADDING)
    .on_press_maybe(
        info.can_seek
            .then_some(Message::SeekMediaRelative(MEDIA_SEEK_INTERVAL_MS)),
    );
    let mut controls = widget::row::with_capacity(7)
        .align_y(Alignment::Center)
        .spacing(control_spacing)
        .push(previous)
        .push(back)
        .push(play_pause)
        .push(forward)
        .push(next);
    if let Some(subtitles) = subtitles {
        controls = controls
            .push(subtitle_toggle(subtitles))
            .push(subtitle_dropdown(subtitles));
    }
    let subtitle = media_subtitle(info);
    let mut metadata = widget::column::with_capacity(3)
        .width(Length::Fill)
        .spacing(detail_spacing)
        .push(marquee::media_title(&info.title));
    if !subtitle.is_empty() {
        metadata = metadata.push(marquee::media_subtitle(&subtitle));
    }
    let metadata = metadata.push(media_player_badge(&info.player_name));
    let details = widget::row::with_capacity(2)
        .align_y(Alignment::Center)
        .spacing(MEDIA_CONTENT_SPACING)
        .push(media_artwork(info.album_art.as_ref()))
        .push(metadata);
    let progress_control: Element<'a, Message> = if info.can_seek && info.duration > 0 {
        let slider = media_slider::view(
            progress,
            timeline_hovered,
            Message::MediaSeekChanged,
            Message::CommitMediaSeek,
        );

        widget::mouse_area(slider)
            .on_enter(Message::MediaTimelineHoverChanged(true))
            .on_exit(Message::MediaTimelineHoverChanged(false))
            .into()
    } else {
        crate::overlay::components::gauge::progress_bar(progress as f32 * 100.0)
    };
    let current_time =
        widget::container(widget::text::body(format_media_time(position))).width(Length::Fill);
    let duration = widget::container(widget::text::body(format_media_time(info.duration)))
        .width(Length::Fill)
        .align_x(cosmic::iced::alignment::Horizontal::Right);
    let mut footer = widget::row::with_capacity(3)
        .width(Length::Fill)
        .height(Length::Fixed(MEDIA_SOURCE_SELECTOR_HEIGHT))
        .align_y(Alignment::Center)
        .push(current_time);
    if player_state.player_count() > 1 {
        footer = footer.push(media_source_selector(player_state));
    }
    footer = footer.push(duration);

    widget::column::with_capacity(7)
        .height(Length::Fixed(MEDIA_CONTENT_HEIGHT))
        .spacing(0)
        .push(details)
        .push(widget::space::vertical())
        .push(widget::container(controls).center_x(Length::Fill))
        .push(widget::space().height(Length::Fixed(MEDIA_TIMELINE_FOOTER_GAP)))
        .push(progress_control)
        .push(widget::space().height(Length::Fixed(MEDIA_TIMELINE_FOOTER_GAP)))
        .push(footer)
        .into()
}

fn emby_subtitles(info: &MediaInfo) -> Option<&SubtitleState> {
    info.subtitles.as_ref().filter(|subtitles| {
        info.player_name.eq_ignore_ascii_case("emby") && !subtitles.tracks.is_empty()
    })
}

fn subtitle_toggle(subtitles: &SubtitleState) -> Element<'static, Message> {
    let enabled = subtitles.selected_index.is_some_and(|index| index >= 0);
    let label = widget::container(widget::text::body("CC").size(12).font(cosmic::font::bold()))
        .center_x(Length::Fixed(MEDIA_SUBTITLE_CONTROL_SIZE))
        .center_y(Length::Fixed(MEDIA_SUBTITLE_CONTROL_SIZE));
    let button = widget::button::custom(label)
        .padding(0)
        .class(if enabled {
            button_style::accent_hover(|| theme::Button::Suggested)
        } else {
            button_style::accent_hover(|| theme::Button::Icon)
        })
        .on_press_maybe(
            subtitles
                .selected_index
                .map(|_| Message::ToggleMediaSubtitles(subtitles.target.clone())),
        );
    widget::tooltip(
        button,
        widget::text::caption(match subtitles.selected_index {
            None => "Choose a subtitle track",
            Some(_) if enabled => "Turn subtitles off",
            Some(_) => "Turn subtitles on",
        }),
        widget::tooltip::Position::Top,
    )
    .into()
}

fn subtitle_options(subtitles: &SubtitleState) -> (Vec<String>, Vec<i32>, Option<usize>) {
    let mut labels = vec!["Off".to_string()];
    let mut indices = vec![-1];
    for track in &subtitles.tracks {
        labels.push(subtitle_labels::compact(
            &track.label,
            MEDIA_SUBTITLE_TEXT_WIDTH,
        ));
        indices.push(track.index);
    }
    // Preserve distinct choices when track names differ only past the compact
    // caption (for example multiple English subtitle formats).
    let original_labels = labels.clone();
    for (position, label) in labels.iter_mut().enumerate().skip(1) {
        if original_labels
            .iter()
            .filter(|other| other.as_str() == label.as_str())
            .count()
            > 1
        {
            *label = subtitle_labels::fit_with_suffix(
                label,
                &format!(" {}", indices[position]),
                MEDIA_SUBTITLE_TEXT_WIDTH,
            );
        }
    }
    let selected = subtitles
        .selected_index
        .and_then(|index| indices.iter().position(|candidate| *candidate == index));
    (labels, indices, selected)
}

fn subtitle_dropdown_key(subtitles: &SubtitleState) -> u64 {
    use std::hash::{Hash, Hasher};

    let mut hasher = std::collections::hash_map::DefaultHasher::new();
    (
        &subtitles.target.session_id,
        &subtitles.target.item_id,
        &subtitles.target.media_source_id,
    )
        .hash(&mut hasher);
    subtitles.tracks.len().hash(&mut hasher);
    for track in &subtitles.tracks {
        (&track.index, &track.label).hash(&mut hasher);
    }
    hasher.finish()
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct SubtitleChoice {
    index: i32,
    label: String,
}

impl std::fmt::Display for SubtitleChoice {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str(&self.label)
    }
}

fn subtitle_dropdown(subtitles: &SubtitleState) -> Element<'static, Message> {
    let (labels, indices, selected) = subtitle_options(subtitles);
    let selected_label = match subtitles.selected_index {
        Some(-1) => "Off",
        Some(index) => subtitles
            .tracks
            .iter()
            .find(|track| track.index == index)
            .map_or("Select subtitle track", |track| track.label.as_str()),
        None => "Select subtitle track",
    };
    let tooltip = format!(
        "Subtitles: {}",
        compact_single_line(selected_label, usize::MAX)
    );
    let target = subtitles.target.clone();
    let choices: Vec<_> = labels
        .into_iter()
        .zip(indices)
        .map(|(label, index)| SubtitleChoice { index, label })
        .collect();
    let selected = selected.and_then(|position| choices.get(position)).cloned();
    // Style the menu separately from the selector so both use the same glass.
    let dropdown: cosmic::iced::Element<
        'static,
        Message,
        subtitle_theme::SubtitleTheme,
        cosmic::Renderer,
    > = cosmic::iced::widget::pick_list(choices, selected, move |choice: SubtitleChoice| {
        Message::SelectMediaSubtitle(target.clone(), choice.index)
    })
    .width(Length::Fixed(MEDIA_SUBTITLE_DROPDOWN_WIDTH))
    .padding([6, 6])
    .text_size(12.0)
    .font(cosmic::font::default())
    .text_wrap(cosmic::iced::widget::text::Wrapping::None)
    .handle(cosmic::iced::widget::pick_list::Handle::Arrow {
        size: Some(12.0.into()),
    })
    .text_line_height(cosmic::iced::widget::text::LineHeight::Relative(1.2))
    .placeholder("Select")
    .into();
    let dropdown = cosmic::iced::widget::themer(
        Some(subtitle_theme::SubtitleTheme(
            crate::overlay::surface::system_theme(),
        )),
        dropdown,
    );
    let selector_card =
        widget::container(dropdown).class(theme::Container::custom(subtitle_theme::card_style));
    let dropdown: Element<'static, Message> = widget::tooltip(
        selector_card,
        widget::text::caption(tooltip),
        widget::tooltip::Position::Top,
    )
    .into();
    // Polls preserve an open menu, but a new item/session or changed stream
    // mapping must discard it instead of retargeting an already open choice.
    media_control_scope::view(subtitle_dropdown_key(subtitles), dropdown)
}

// This Iced version's keyed column preserves equal-length children even when
// their keys differ. Explicitly reset native control subtrees on identity
// changes; ordinary polling still preserves their open/hover/drag state.
mod media_control_scope {
    use super::Message;
    use cosmic::iced::advanced::widget::Operation;
    use cosmic::iced::advanced::widget::tree::{self, Tree};
    use cosmic::iced::advanced::{Clipboard, Layout, Shell, Widget, layout, overlay, renderer};
    use cosmic::iced::{Event, Length, Rectangle, Size, Vector, mouse};
    use cosmic::{Element, Renderer, Theme};

    pub(super) fn view(key: u64, content: Element<'static, Message>) -> Element<'static, Message> {
        Element::new(ScopedMediaControl { key, content })
    }

    struct ScopedMediaControl {
        key: u64,
        content: Element<'static, Message>,
    }

    struct ScopeState(u64);

    impl Widget<Message, Theme, Renderer> for ScopedMediaControl {
        fn tag(&self) -> tree::Tag {
            tree::Tag::of::<ScopeState>()
        }
        fn state(&self) -> tree::State {
            tree::State::new(ScopeState(self.key))
        }
        fn children(&self) -> Vec<Tree> {
            vec![Tree::new(self.content.as_widget())]
        }

        fn diff(&mut self, tree: &mut Tree) {
            let state = tree.state.downcast_mut::<ScopeState>();
            if state.0 != self.key {
                state.0 = self.key;
                tree.children = self.children();
            }
            tree.children[0].diff(self.content.as_widget_mut());
        }

        fn size(&self) -> Size<Length> {
            self.content.as_widget().size()
        }
        fn size_hint(&self) -> Size<Length> {
            self.content.as_widget().size_hint()
        }

        fn layout(
            &mut self,
            tree: &mut Tree,
            renderer: &Renderer,
            limits: &layout::Limits,
        ) -> layout::Node {
            self.content
                .as_widget_mut()
                .layout(&mut tree.children[0], renderer, limits)
        }

        fn operate(
            &mut self,
            tree: &mut Tree,
            layout: Layout<'_>,
            renderer: &Renderer,
            operation: &mut dyn Operation,
        ) {
            self.content.as_widget_mut().operate(
                &mut tree.children[0],
                layout,
                renderer,
                operation,
            );
        }

        fn update(
            &mut self,
            tree: &mut Tree,
            event: &Event,
            layout: Layout<'_>,
            cursor: mouse::Cursor,
            renderer: &Renderer,
            clipboard: &mut dyn Clipboard,
            shell: &mut Shell<'_, Message>,
            viewport: &Rectangle,
        ) {
            self.content.as_widget_mut().update(
                &mut tree.children[0],
                event,
                layout,
                cursor,
                renderer,
                clipboard,
                shell,
                viewport,
            );
        }

        fn mouse_interaction(
            &self,
            tree: &Tree,
            layout: Layout<'_>,
            cursor: mouse::Cursor,
            viewport: &Rectangle,
            renderer: &Renderer,
        ) -> mouse::Interaction {
            self.content.as_widget().mouse_interaction(
                &tree.children[0],
                layout,
                cursor,
                viewport,
                renderer,
            )
        }

        fn draw(
            &self,
            tree: &Tree,
            renderer: &mut Renderer,
            theme: &Theme,
            style: &renderer::Style,
            layout: Layout<'_>,
            cursor: mouse::Cursor,
            viewport: &Rectangle,
        ) {
            self.content.as_widget().draw(
                &tree.children[0],
                renderer,
                theme,
                style,
                layout,
                cursor,
                viewport,
            );
        }

        fn overlay<'a>(
            &'a mut self,
            tree: &'a mut Tree,
            layout: Layout<'a>,
            renderer: &Renderer,
            viewport: &Rectangle,
            translation: Vector,
        ) -> Option<overlay::Element<'a, Message, Theme, Renderer>> {
            self.content.as_widget_mut().overlay(
                &mut tree.children[0],
                layout,
                renderer,
                viewport,
                translation,
            )
        }
        fn a11y_nodes(
            &self,
            layout: Layout<'_>,
            state: &Tree,
            cursor: mouse::Cursor,
        ) -> iced_accessibility::A11yTree {
            self.content
                .as_widget()
                .a11y_nodes(layout, &state.children[0], cursor)
        }

        // IDs belong to the native child tree. Keep this structural wrapper
        // anonymous to avoid duplicating them in the accessibility tree.
        fn drag_destinations(
            &self,
            state: &Tree,
            layout: Layout<'_>,
            renderer: &Renderer,
            destinations: &mut cosmic::iced::advanced::clipboard::DndDestinationRectangles,
        ) {
            self.content.as_widget().drag_destinations(
                &state.children[0],
                layout,
                renderer,
                destinations,
            );
        }
    }
}

fn media_source_selector<'a>(
    player_state: &'a crate::monitors::media::MultiPlayerState,
) -> Element<'a, Message> {
    let mut dots = widget::row::with_capacity(player_state.player_count())
        .height(Length::Fixed(MEDIA_SOURCE_SELECTOR_HEIGHT))
        .align_y(Alignment::Center)
        .spacing(2);

    for (index, (player_id, info)) in player_state.players.iter().enumerate() {
        let dot = media_source_dot(index == player_state.current_index);
        let button = widget::button::custom(dot)
            .width(Length::Fixed(MEDIA_SOURCE_BUTTON_SIZE))
            .height(Length::Fixed(MEDIA_SOURCE_BUTTON_SIZE))
            .padding((MEDIA_SOURCE_BUTTON_SIZE - MEDIA_SOURCE_DOT_SIZE) / 2.0)
            .class(button_style::accent_hover(|| theme::Button::Text))
            .on_press(Message::SelectMediaPlayer(player_id.clone()));
        dots = dots.push(widget::tooltip(
            button,
            widget::text::caption(format!("Switch to {}", info.player_name)),
            widget::tooltip::Position::Top,
        ));
    }

    dots.into()
}

fn media_source_dot(active: bool) -> Element<'static, Message> {
    widget::container(widget::space())
        .width(Length::Fixed(MEDIA_SOURCE_DOT_SIZE))
        .height(Length::Fixed(MEDIA_SOURCE_DOT_SIZE))
        .class(theme::Container::custom(move |theme| {
            let cosmic = theme.cosmic();
            let accent: Color = cosmic.accent_color().into();
            let neutral: Color = cosmic.on_bg_color().into();

            cosmic::iced::widget::container::Style {
                background: active.then_some(Background::Color(accent)),
                border: Border {
                    color: Color { a: 0.55, ..neutral },
                    width: 1.0,
                    radius: [MEDIA_SOURCE_DOT_SIZE / 2.0; 4].into(),
                },
                ..Default::default()
            }
        }))
        .into()
}

fn media_artwork(art: Option<&AlbumArt>) -> Element<'static, Message> {
    if let Some(art) = art {
        let is_video_art = u64::from(art.source_width) * 4 >= u64::from(art.source_height) * 5;
        let (width, height) = if is_video_art {
            (MEDIA_VIDEO_ARTWORK_WIDTH, MEDIA_VIDEO_ARTWORK_HEIGHT)
        } else {
            (MEDIA_ARTWORK_SIZE, MEDIA_ARTWORK_SIZE)
        };
        return rounded_artwork::view(art, width, height);
    }

    widget::container(widget::icon::from_name("audio-x-generic-symbolic").size(48))
        .center_x(Length::Fixed(MEDIA_ARTWORK_SIZE))
        .center_y(Length::Fixed(MEDIA_ARTWORK_SIZE))
        .class(theme::Container::List)
        .into()
}

fn media_player_badge(player_name: &str) -> Element<'static, Message> {
    let badge = widget::row::with_capacity(2)
        .align_y(Alignment::Center)
        .spacing(6)
        .push(
            widget::icon::icon(media_player_icon(player_name))
                .size(16)
                .content_fit(ContentFit::Contain),
        )
        .push(widget::text::body(compact_single_line(player_name, 18)));
    widget::container(badge)
        .padding([3, 8])
        .class(theme::Container::Secondary)
        .into()
}

fn media_player_icon(player_name: &str) -> widget::icon::Handle {
    static CACHE: OnceLock<Mutex<HashMap<String, widget::icon::Handle>>> = OnceLock::new();
    let name = player_name.to_ascii_lowercase();
    let mut cache = CACHE.get_or_init(Mutex::default).lock().unwrap();
    if let Some(handle) = cache.get(&name) {
        return handle.clone();
    }
    let icons: &[&str] = match name.as_str() {
        "zen" | "zen browser" => &["app.zen_browser.zen", "zen", "zen-browser"],
        "firefox" | "mozilla firefox" => &["firefox", "org.mozilla.firefox"],
        "chromium" => &["chromium", "chromium-browser", "org.chromium.Chromium"],
        "chrome" | "google chrome" => &["google-chrome", "com.google.Chrome"],
        "brave" | "brave web browser" => &["brave-browser", "com.brave.Browser", "brave"],
        "cider" => &["cider", "sh.cider.Cider"],
        "cosmic player" => &["com.system76.CosmicPlayer", "cosmic-player"],
        "spotify" => &["spotify", "com.spotify.Client"],
        "emby" => &["emby-theater", "tv.emby.EmbyTheater", "emby"],
        "vlc" | "vlc media player" => &["vlc", "org.videolan.VLC"],
        _ => &[],
    };
    let handle = icons
        .iter()
        .find_map(|name| {
            widget::icon::from_name(*name)
                .size(16)
                .fallback(None)
                .path()
                .map(widget::icon::from_path)
        })
        .unwrap_or_else(|| widget::icon::from_name("emblem-music-symbolic").handle());
    if cache.len() >= 64 {
        cache.clear();
    }
    cache.insert(name, handle.clone());
    handle
}

fn media_subtitle(info: &MediaInfo) -> String {
    let subtitle = match (info.artist.trim(), info.album.trim()) {
        ("", "") => String::new(),
        (artist, "") => artist.to_string(),
        ("", album) => album.to_string(),
        (artist, album) => format!("{artist} - {album}"),
    };
    compact_single_line(&subtitle, usize::MAX)
}

fn format_media_time(milliseconds: u64) -> String {
    let seconds = milliseconds / 1_000;
    format!("{}:{:02}", seconds / 60, seconds % 60)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn volume_fits_existing_header_and_source_switch_cancels_drag() {
        use cosmic::iced::advanced::renderer::Headless as _;
        use cosmic::iced::advanced::{
            Layout, Renderer as _, Shell, layout, renderer, widget::Tree,
        };
        use cosmic::iced::{Event, Font, Pixels, Point, Rectangle, Size, mouse};

        let mut renderer = <cosmic::Renderer as renderer::Headless>::new(
            Font::default(),
            Pixels(14.0),
            Some("tiny-skia"),
        )
        .await
        .expect("software renderer for media volume validation");
        let make_stats = |volume| SystemSnapshot {
            media: crate::monitors::media::MultiPlayerState {
                players: vec![(
                    PlayerId::Cider,
                    MediaInfo {
                        player_name: "Cider".into(),
                        title: "Bear".into(),
                        album: "Original Motion Picture Soundtrack".into(),
                        status: PlaybackStatus::Playing,
                        duration: 115_000,
                        position: 73_000,
                        volume,
                        can_play: true,
                        can_pause: true,
                        can_seek: true,
                        ..Default::default()
                    },
                )],
                current_index: 0,
            },
            ..Default::default()
        };
        let card = Rectangle::with_size(Size::new(322.0, MEDIA_ACTIVE_SECTION_HEIGHT));
        let theme = cosmic::Theme::dark();
        let preview = std::env::var_os("COSMIC_MEDIA_VOLUME_PREVIEW");
        let mut gallery = preview
            .as_ref()
            .map(|_| image::RgbaImage::new(322 * 4, 2 * MEDIA_ACTIVE_SECTION_HEIGHT as u32));
        let mut baseline_header_height = None;
        for (index, volume) in [None, Some(0.0), Some(0.65), Some(1.0)]
            .into_iter()
            .enumerate()
        {
            let stats = make_stats(volume);
            let mut content = media_view(&stats, None, None, None, 12, 12, 4, false);
            let mut tree = Tree::new(content.as_widget());
            let node = content.as_widget_mut().layout(
                &mut tree,
                &renderer,
                &layout::Limits::new(Size::ZERO, card.size()),
            );
            let header = Layout::new(&node).children().next().unwrap();
            let header_height = header.bounds().height;
            assert_eq!(
                header_height,
                *baseline_header_height.get_or_insert(header_height)
            );
            assert_eq!(
                node.size(),
                card.size(),
                "volume must not grow the media card"
            );
            fn assert_contained(node: Layout<'_>, bounds: Rectangle) {
                let child = node.bounds();
                assert!(
                    child.x >= bounds.x - 0.1
                        && child.y >= bounds.y - 0.1
                        && child.x + child.width <= bounds.x + bounds.width + 0.1
                        && child.y + child.height <= bounds.y + bounds.height + 0.1,
                    "volume header child {child:?} exceeds {bounds:?}"
                );
                for child in node.children() {
                    assert_contained(child, bounds);
                }
            }
            assert_contained(header, header.bounds());
            if let Some(gallery) = &mut gallery {
                for (row, hovered) in [false, true].into_iter().enumerate() {
                    renderer.reset(card);
                    let cursor = if hovered && volume.is_some() {
                        mouse::Cursor::Available(Point::new(
                            card.width - 50.0,
                            header.bounds().center_y(),
                        ))
                    } else {
                        mouse::Cursor::Unavailable
                    };
                    content.as_widget().draw(
                        &tree,
                        &mut renderer,
                        &theme,
                        &renderer::Style {
                            text_color: Color::WHITE,
                            icon_color: Color::WHITE,
                            scale_factor: 1.0,
                        },
                        Layout::new(&node),
                        cursor,
                        &card,
                    );
                    let pixels = renderer.screenshot(
                        Size::new(322, MEDIA_ACTIVE_SECTION_HEIGHT as u32),
                        1.0,
                        theme.cosmic().background(false).base.into(),
                    );
                    let frame =
                        image::RgbaImage::from_raw(322, MEDIA_ACTIVE_SECTION_HEIGHT as u32, pixels)
                            .unwrap();
                    image::imageops::replace(
                        gallery,
                        &frame,
                        index as i64 * 322,
                        row as i64 * MEDIA_ACTIVE_SECTION_HEIGHT as i64,
                    );
                }
            }
            if volume.is_some() {
                let control = header.children().last().unwrap().bounds();
                assert_eq!(control.x + control.width, card.width);
                assert_eq!(control.width, 124.0);
                let mut messages = Vec::new();
                let cursor =
                    mouse::Cursor::Available(Point::new(control.x + 99.0, control.center_y()));
                for event in [
                    Event::Mouse(mouse::Event::ButtonPressed(mouse::Button::Left)),
                    Event::Mouse(mouse::Event::ButtonReleased(mouse::Button::Left)),
                ] {
                    content.as_widget_mut().update(
                        &mut tree,
                        &event,
                        Layout::new(&node),
                        cursor,
                        &renderer,
                        &mut cosmic::iced::advanced::clipboard::Null,
                        &mut Shell::new(&mut messages),
                        &card,
                    );
                }
                assert!(matches!(messages.as_slice(), [
                    Message::MediaVolumeChanged(PlayerId::Cider, value),
                    Message::CommitMediaVolume(PlayerId::Cider)
                ] if (*value - 0.75).abs() < 0.001));
            }
        }
        if let Some((gallery, path)) = gallery.zip(preview) {
            gallery.save(path).unwrap();
        }

        // Rebuild a captured drag as normal polling would, then as a different
        // source. Only the same source may keep receiving the drag/release.
        for same_player in [true, false] {
            let mut control = media_volume_control(&PlayerId::Cider, 0.5);
            let mut tree = Tree::new(control.as_widget());
            let bounds = Rectangle::with_size(Size::new(124.0, 20.0));
            let node = control.as_widget_mut().layout(
                &mut tree,
                &renderer,
                &layout::Limits::new(Size::ZERO, bounds.size()),
            );
            let mut messages = Vec::new();
            control.as_widget_mut().update(
                &mut tree,
                &Event::Mouse(mouse::Event::ButtonPressed(mouse::Button::Left)),
                Layout::new(&node),
                mouse::Cursor::Available(Point::new(74.0, 10.0)),
                &renderer,
                &mut cosmic::iced::advanced::clipboard::Null,
                &mut Shell::new(&mut messages),
                &bounds,
            );
            messages.clear();
            let player = if same_player {
                PlayerId::Cider
            } else {
                PlayerId::Emby("new-session".into())
            };
            let mut rebuilt = media_volume_control(&player, 0.5);
            tree.diff(rebuilt.as_widget_mut());
            let node = rebuilt.as_widget_mut().layout(
                &mut tree,
                &renderer,
                &layout::Limits::new(Size::ZERO, bounds.size()),
            );
            let pointer = Point::new(99.0, 10.0);
            for event in [
                Event::Mouse(mouse::Event::CursorMoved { position: pointer }),
                Event::Mouse(mouse::Event::ButtonReleased(mouse::Button::Left)),
            ] {
                rebuilt.as_widget_mut().update(
                    &mut tree,
                    &event,
                    Layout::new(&node),
                    mouse::Cursor::Available(pointer),
                    &renderer,
                    &mut cosmic::iced::advanced::clipboard::Null,
                    &mut Shell::new(&mut messages),
                    &bounds,
                );
            }
            assert_eq!(
                messages.len(),
                if same_player { 2 } else { 0 },
                "source switch must cancel dragging"
            );
        }
    }

    #[tokio::test]
    async fn transport_controls_fit_the_compact_media_card() {
        use cosmic::iced::advanced::renderer::Headless as _;
        use cosmic::iced::advanced::{Layout, Renderer as _, layout, renderer, widget::Tree};
        use cosmic::iced::{Font, Pixels, Rectangle, Size, mouse};

        fn assert_contained(layout: Layout<'_>, card: Rectangle) {
            let bounds = layout.bounds();
            assert!(
                bounds.x >= card.x - 0.1
                    && bounds.y >= card.y - 0.1
                    && bounds.x + bounds.width <= card.x + card.width + 0.1
                    && bounds.y + bounds.height <= card.y + card.height + 0.1,
                "media child {bounds:?} overflows compact card {card:?}"
            );
            for child in layout.children() {
                assert_contained(child, card);
            }
        }

        let mut renderer = <cosmic::Renderer as renderer::Headless>::new(
            Font::default(),
            Pixels(14.0),
            Some("tiny-skia"),
        )
        .await
        .expect("the software renderer must be available for media layout validation");
        let info = MediaInfo {
            player_name: "COSMIC Player".to_string(),
            title: "A very long episode title that needs to scroll within the card".to_string(),
            artist: "An artist with a long name".to_string(),
            album: "An album with a long name".to_string(),
            duration: 1_440_000,
            can_seek: true,
            ..Default::default()
        };
        let mut examples = vec![info.clone()];
        for selected in [Some(2), Some(-1), None, Some(9), Some(12)] {
            let mut emby = info.clone();
            emby.player_name = "Emby".into();
            emby.status = PlaybackStatus::Playing;
            emby.can_play = true;
            emby.can_pause = true;
            emby.can_go_previous = true;
            emby.can_go_next = true;
            emby.subtitles = Some(sample_subtitles(selected));
            examples.push(emby);
        }
        let players = crate::monitors::media::MultiPlayerState::default();
        let card = Rectangle::with_size(Size::new(322.0, MEDIA_CONTENT_HEIGHT));
        let preview = std::env::var_os("COSMIC_MEDIA_SUBTITLE_PREVIEW");
        let mut gallery = preview.as_ref().map(|_| {
            image::RgbaImage::new(322 * examples.len() as u32, MEDIA_CONTENT_HEIGHT as u32)
        });
        let theme = cosmic::Theme::dark();
        for (index, info) in examples.iter().enumerate() {
            let mut content = media_content(info, &players, None, 12, 4, false);
            let mut tree = Tree::new(content.as_widget());
            let node = content.as_widget_mut().layout(
                &mut tree,
                &renderer,
                &layout::Limits::new(Size::ZERO, card.size()),
            );
            assert_contained(Layout::new(&node), card);
            if let Some(gallery) = &mut gallery {
                renderer.reset(card);
                content.as_widget().draw(
                    &tree,
                    &mut renderer,
                    &theme,
                    &renderer::Style {
                        text_color: Color::WHITE,
                        icon_color: Color::WHITE,
                        scale_factor: 1.0,
                    },
                    Layout::new(&node),
                    mouse::Cursor::Unavailable,
                    &card,
                );
                let pixels = renderer.screenshot(
                    Size::new(322, MEDIA_CONTENT_HEIGHT as u32),
                    1.0,
                    theme.cosmic().background(false).base.into(),
                );
                let frame =
                    image::RgbaImage::from_raw(322, MEDIA_CONTENT_HEIGHT as u32, pixels).unwrap();
                image::imageops::replace(gallery, &frame, index as i64 * 322, 0);
            }
        }
        if let Some((gallery, path)) = gallery.zip(preview) {
            gallery.save(path).unwrap();
        }
    }

    fn sample_subtitles(selected_index: Option<i32>) -> SubtitleState {
        use crate::monitors::media::{SubtitleTarget, SubtitleTrack};
        SubtitleState {
            target: SubtitleTarget {
                session_id: "test-session".into(),
                item_id: "test-item".into(),
                media_source_id: Some("test-source".into()),
            },
            tracks: vec![
                SubtitleTrack {
                    index: 2,
                    label: "English".into(),
                },
                SubtitleTrack {
                    index: 5,
                    label: "English · Forced".into(),
                },
                SubtitleTrack {
                    index: 9,
                    label: "English · SDH · An exceptionally long subtitle track name".into(),
                },
                SubtitleTrack {
                    index: 12,
                    label: "English - SRT".into(),
                },
            ],
            selected_index,
        }
    }

    #[tokio::test]
    async fn subtitle_menu_survives_polls_but_closes_when_its_target_or_tracks_change() {
        use cosmic::iced::advanced::renderer::Headless as _;
        use cosmic::iced::advanced::{
            Layout, Renderer as _, Shell, layout, renderer, widget::Tree,
        };
        use cosmic::iced::{Event, Font, Pixels, Rectangle, Size, Vector, mouse};

        let mut renderer = <cosmic::Renderer as renderer::Headless>::new(
            Font::default(),
            Pixels(14.0),
            Some("tiny-skia"),
        )
        .await
        .expect("software renderer for subtitle menu lifecycle validation");
        let baseline = sample_subtitles(Some(2));
        let viewport = Rectangle::with_size(Size::new(322.0, 250.0));
        let mut updates = vec![(baseline.clone(), true)];
        let mut selected = baseline.clone();
        selected.selected_index = Some(-1);
        updates.push((selected, true));
        let mut session = baseline.clone();
        session.target.session_id = "new-session".into();
        updates.push((session, false));
        let mut item = baseline.clone();
        item.target.item_id = "new-item".into();
        updates.push((item, false));
        let mut source = baseline.clone();
        source.target.media_source_id = Some("new-source".into());
        updates.push((source, false));
        let mut stream = baseline.clone();
        stream.tracks[0].index = 42;
        updates.push((stream, false));
        let mut label = baseline.clone();
        label.tracks[0].label = "Japanese".into();
        updates.push((label, false));

        for (updated, remains_open) in updates {
            let mut dropdown = subtitle_dropdown(&baseline);
            let mut tree = Tree::new(dropdown.as_widget());
            let node = dropdown.as_widget_mut().layout(
                &mut tree,
                &renderer,
                &layout::Limits::new(Size::ZERO, viewport.size()),
            );
            let mut messages = Vec::new();
            dropdown.as_widget_mut().update(
                &mut tree,
                &Event::Mouse(mouse::Event::ButtonPressed(mouse::Button::Left)),
                Layout::new(&node),
                mouse::Cursor::Available(node.bounds().center()),
                &renderer,
                &mut cosmic::iced::advanced::clipboard::Null,
                &mut Shell::new(&mut messages),
                &viewport,
            );
            // Remove the tooltip before checking whether the actual dropdown
            // menu survives the view rebuild. Leaving does not close the menu.
            dropdown.as_widget_mut().update(
                &mut tree,
                &Event::Mouse(mouse::Event::CursorLeft),
                Layout::new(&node),
                mouse::Cursor::Unavailable,
                &renderer,
                &mut cosmic::iced::advanced::clipboard::Null,
                &mut Shell::new(&mut messages),
                &viewport,
            );
            assert!(
                dropdown
                    .as_widget_mut()
                    .overlay(
                        &mut tree,
                        Layout::new(&node),
                        &renderer,
                        &viewport,
                        Vector::ZERO,
                    )
                    .is_some()
            );
            assert!(messages.is_empty());
            if updated == baseline
                && let Some(path) = std::env::var_os("COSMIC_SUBTITLE_MENU_PREVIEW")
            {
                let mut theme = cosmic::Theme::dark();
                theme.transparent = true;
                renderer.reset(viewport);
                let backdrop = cosmic::iced::gradient::Linear::new(cosmic::iced::Radians(0.0))
                    .add_stop(0.0, Color::from_rgb(0.16, 0.22, 0.36))
                    .add_stop(1.0, Color::from_rgb(0.36, 0.21, 0.30));
                renderer.fill_quad(
                    renderer::Quad {
                        bounds: viewport,
                        ..Default::default()
                    },
                    Background::Gradient(backdrop.into()),
                );
                let style = renderer::Style {
                    text_color: Color::WHITE,
                    icon_color: Color::WHITE,
                    scale_factor: 1.0,
                };
                dropdown.as_widget().draw(
                    &tree,
                    &mut renderer,
                    &theme,
                    &style,
                    Layout::new(&node),
                    mouse::Cursor::Unavailable,
                    &viewport,
                );
                let mut menu = dropdown
                    .as_widget_mut()
                    .overlay(
                        &mut tree,
                        Layout::new(&node),
                        &renderer,
                        &viewport,
                        Vector::ZERO,
                    )
                    .unwrap();
                let menu_node = menu.as_overlay_mut().layout(&renderer, viewport.size());
                menu.as_overlay().draw(
                    &mut renderer,
                    &theme,
                    &style,
                    Layout::new(&menu_node),
                    mouse::Cursor::Unavailable,
                );
                let pixels = renderer.screenshot(Size::new(322, 250), 1.0, Color::TRANSPARENT);
                image::RgbaImage::from_raw(322, 250, pixels)
                    .unwrap()
                    .save(path)
                    .unwrap();
            }

            let mut rebuilt = subtitle_dropdown(&updated);
            tree.diff(rebuilt.as_widget_mut());
            let node = rebuilt.as_widget_mut().layout(
                &mut tree,
                &renderer,
                &layout::Limits::new(Size::ZERO, viewport.size()),
            );
            assert_eq!(
                rebuilt
                    .as_widget_mut()
                    .overlay(
                        &mut tree,
                        Layout::new(&node),
                        &renderer,
                        &viewport,
                        Vector::ZERO,
                    )
                    .is_some(),
                remains_open,
                "updated state: {updated:?}"
            );
        }
    }

    #[test]
    fn subtitle_choices_keep_stream_indices_and_unknown_state_distinct_from_off() {
        let (labels, indices, selected) = subtitle_options(&sample_subtitles(Some(9)));
        assert_eq!(indices, [-1, 2, 5, 9, 12]);
        assert_eq!(selected, Some(3));
        assert_eq!(labels[0], "Off");
        assert_eq!(labels[1], "English");
        assert!(labels[2].contains("Forced"));
        assert!(labels[3].contains("SDH"));
        assert_eq!(subtitle_options(&sample_subtitles(Some(-1))).2, Some(0));
        assert_eq!(subtitle_options(&sample_subtitles(None)).2, None);
        assert_eq!(subtitle_options(&sample_subtitles(Some(99))).2, None);

        let mut duplicates = sample_subtitles(Some(2));
        duplicates.tracks[0].label = "English alternative one".into();
        duplicates.tracks[3].label = "English alternative two".into();
        let (labels, _, _) = subtitle_options(&duplicates);
        assert_ne!(labels[1], labels[4]);
    }

    #[test]
    fn subtitle_controls_only_appear_for_emby_with_actual_tracks() {
        let mut info = MediaInfo {
            player_name: "COSMIC Player".into(),
            subtitles: Some(sample_subtitles(Some(2))),
            ..Default::default()
        };
        assert!(emby_subtitles(&info).is_none());
        info.player_name = "Emby".into();
        assert!(emby_subtitles(&info).is_some());
        info.subtitles.as_mut().unwrap().tracks.clear();
        assert!(emby_subtitles(&info).is_none());
    }

    #[test]
    fn media_metadata_is_prepared_for_iced() {
        let info = MediaInfo {
            player_name: "Firefox".to_string(),
            artist: "Deftones".to_string(),
            album: "Saturday Night Wrist".to_string(),
            ..Default::default()
        };

        assert_eq!(media_subtitle(&info), "Deftones - Saturday Night Wrist");
        assert_eq!(format_media_time(302_000), "5:02");

        let long_album = MediaInfo {
            album: "Final Straw (20th Anniversary Edition)".to_string(),
            ..Default::default()
        };
        assert_eq!(
            media_subtitle(&long_album),
            "Final Straw (20th Anniversary Edition)"
        );
    }
}
