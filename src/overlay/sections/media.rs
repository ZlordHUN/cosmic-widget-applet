// SPDX-License-Identifier: MPL-2.0

use crate::monitors::media::{AlbumArt, MediaInfo, PlaybackStatus};
use crate::overlay::Message;
use crate::overlay::components::{
    marquee,
    section::{compact_single_line, section},
    slide,
};
use crate::overlay::media_state::DismissingMedia;
use crate::overlay::stats::SystemSnapshot;
use cosmic::iced::core::image::FilterMethod;
use cosmic::iced::{Alignment, Background, Border, Color, ContentFit, Length};
use cosmic::{Element, theme, widget};
use std::rc::Rc;

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
const MEDIA_TIMELINE_FOOTER_GAP: f32 = 4.0;

pub(in crate::overlay) fn media_view<'a>(
    stats: &'a SystemSnapshot,
    dismissing: Option<&'a DismissingMedia>,
    seek_preview: Option<f64>,
    section_spacing: u16,
    content_spacing: u16,
    detail_spacing: u16,
    timeline_hovered: bool,
) -> Element<'a, Message> {
    let state = dismissing.map_or(&stats.media, |outgoing| &outgoing.state);
    if let Some((_, info)) = state.current_player().filter(|(_, info)| info.is_active()) {
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
        return section("emblem-music-symbolic", "Now Playing", section_spacing)
            .height(Length::Fixed(MEDIA_ACTIVE_SECTION_HEIGHT))
            .push(content)
            .into();
    }

    section("emblem-music-symbolic", "Now Playing", section_spacing)
        .push(empty_media_card())
        .into()
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
    let previous = widget::button::icon(
        widget::icon::from_name("media-skip-backward-symbolic").size(MEDIA_CONTROL_ICON_SIZE),
    )
    .tooltip("Previous track")
    .padding(MEDIA_CONTROL_PADDING)
    .on_press_maybe(info.can_go_previous.then_some(Message::PreviousMedia));
    let play_pause_icon = match info.status {
        PlaybackStatus::Playing => "media-playback-pause-symbolic",
        PlaybackStatus::Paused | PlaybackStatus::Stopped => "media-playback-start-symbolic",
    };
    let play_pause = widget::button::icon(
        widget::icon::from_name(play_pause_icon).size(MEDIA_CONTROL_ICON_SIZE),
    )
    .tooltip(match info.status {
        PlaybackStatus::Playing => "Pause",
        PlaybackStatus::Paused | PlaybackStatus::Stopped => "Play",
    })
    .padding(MEDIA_CONTROL_PADDING)
    .on_press_maybe((info.can_play || info.can_pause).then_some(Message::PlayPauseMedia));
    let next = widget::button::icon(
        widget::icon::from_name("media-skip-forward-symbolic").size(MEDIA_CONTROL_ICON_SIZE),
    )
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
    .icon_size(MEDIA_SEEK_ICON_SIZE)
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
    .icon_size(MEDIA_SEEK_ICON_SIZE)
    .tooltip("Forward 10 seconds")
    .padding(MEDIA_CONTROL_PADDING)
    .on_press_maybe(
        info.can_seek
            .then_some(Message::SeekMediaRelative(MEDIA_SEEK_INTERVAL_MS)),
    );
    let controls = widget::row::with_capacity(5)
        .align_y(Alignment::Center)
        .spacing(MEDIA_CONTROL_SPACING)
        .push(previous)
        .push(back)
        .push(play_pause)
        .push(forward)
        .push(next);
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
        let slider = widget::slider(0.0..=1.0, progress, Message::MediaSeekChanged)
            .step(0.001)
            .height(20)
            .class(theme::style::iced::Slider::Custom {
                active: Rc::new(move |theme| {
                    media_seek_style(
                        theme,
                        cosmic::iced::widget::slider::Status::Active,
                        timeline_hovered,
                    )
                }),
                hovered: Rc::new(|theme| {
                    media_seek_style(theme, cosmic::iced::widget::slider::Status::Hovered, true)
                }),
                dragging: Rc::new(|theme| {
                    media_seek_style(theme, cosmic::iced::widget::slider::Status::Dragged, true)
                }),
            })
            .on_release(Message::CommitMediaSeek);

        widget::mouse_area(slider)
            .on_enter(Message::MediaTimelineHoverChanged(true))
            .on_exit(Message::MediaTimelineHoverChanged(false))
            .into()
    } else {
        widget::progress_bar::linear::Linear::new()
            .girth(6)
            .progress(progress as f32)
            .width(Length::Fill)
            .into()
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

fn media_seek_style(
    theme: &cosmic::Theme,
    status: cosmic::iced::widget::slider::Status,
    show_handle: bool,
) -> cosmic::iced::widget::slider::Style {
    use cosmic::iced::widget::slider::{Catalog as _, HandleShape};

    let mut style = theme.style(&theme::style::iced::Slider::Standard, status);
    if !show_handle {
        style.handle.shape = HandleShape::Circle { radius: 0.0 };
        style.handle.background = Background::Color(Color::TRANSPARENT);
        style.handle.border_width = 0.0;
    }
    style
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
            .class(theme::Button::Text)
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
                    width: if active { 0.0 } else { 1.0 },
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
        return widget::image(art.iced_handle.clone())
            .width(Length::Fixed(width))
            .height(Length::Fixed(height))
            .content_fit(ContentFit::Cover)
            .filter_method(FilterMethod::Linear)
            .border_radius([4.0; 4])
            .into();
    }

    widget::container(widget::icon::from_name("audio-x-generic-symbolic").size(48))
        .center_x(Length::Fixed(MEDIA_ARTWORK_SIZE))
        .center_y(Length::Fixed(MEDIA_ARTWORK_SIZE))
        .class(theme::Container::List)
        .into()
}

fn media_player_badge(player_name: &str) -> Element<'static, Message> {
    widget::container(widget::text::body(compact_single_line(player_name, 18)))
        .padding([3, 8])
        .class(theme::Container::Secondary)
        .into()
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
    async fn transport_controls_fit_the_compact_media_card() {
        use cosmic::iced::advanced::{Layout, layout, renderer, widget::Tree};
        use cosmic::iced::{Font, Pixels, Rectangle, Size};

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

        let renderer = <cosmic::Renderer as renderer::Headless>::new(
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
        let players = crate::monitors::media::MultiPlayerState::default();
        let card = Rectangle::with_size(Size::new(322.0, MEDIA_CONTENT_HEIGHT));
        let mut content = media_content(&info, &players, None, 12, 4, false);
        let mut tree = Tree::new(content.as_widget());
        let node = content.as_widget_mut().layout(
            &mut tree,
            &renderer,
            &layout::Limits::new(Size::ZERO, card.size()),
        );
        assert_contained(Layout::new(&node), card);
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
