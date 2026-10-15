// SPDX-License-Identifier: MPL-2.0

//! Plays bundled Meteocons frames at a fixed position with native clipping.
//!
//! The upstream SVG animations are rendered into atlases ahead of time. Only the
//! visible condition is decoded, once, into reusable RGBA frame handles. Keeping
//! each frame at the same bounds avoids atlas sampling jitter and limits redraw
//! damage to the icon. Frame changes never decode PNGs or rebuild the widget tree.

use cosmic::iced::advanced::image::{self, Renderer as _};
use cosmic::iced::advanced::widget::tree::{self, Tree};
use cosmic::iced::advanced::{Clipboard, Layout, Shell, Widget, layout, renderer};
use cosmic::iced::time::Instant;
use cosmic::iced::{Event, Length, Rectangle, Size, mouse, window};
use cosmic::{Element, Renderer, Theme};
use serde::Deserialize;
use std::cell::RefCell;
use std::collections::BTreeMap;
use std::sync::OnceLock;
use std::time::Duration;

const ICON_SIZE: f32 = 80.0;

pub(in crate::overlay) fn weather_icon(code: &str) -> Element<'static, crate::overlay::Message> {
    WeatherIcon::new(condition_name(code)).into()
}

fn condition_name(code: &str) -> &'static str {
    match code {
        "01d" => "clear-day",
        "01n" => "clear-night",
        "02d" | "03d" => "partly-cloudy-day",
        "02n" | "03n" => "partly-cloudy-night",
        "04d" | "04n" => "overcast",
        "09d" | "09n" => "rain",
        "10d" => "partly-cloudy-day-rain",
        "10n" => "partly-cloudy-night-rain",
        "11d" | "11n" => "thunderstorms",
        "13d" | "13n" => "snow",
        "50d" | "50n" => "mist",
        _ => "not-available",
    }
}

#[derive(Deserialize)]
struct Manifest {
    frame_size: u32,
    frame_ms: u32,
    icons: BTreeMap<String, Atlas>,
}

#[derive(Clone, Copy, Deserialize)]
struct Atlas {
    columns: u32,
    rows: u32,
    frames: u32,
    frame_ms: Option<u32>,
}

impl Atlas {
    fn frame_duration(self) -> Duration {
        Duration::from_millis(u64::from(self.frame_ms.unwrap_or(manifest().frame_ms)))
    }

    fn frame_handles(self, pixels: &::image::RgbaImage, frame_size: u32) -> Vec<image::Handle> {
        assert_eq!(
            pixels.dimensions(),
            (self.columns * frame_size, self.rows * frame_size),
            "bundled Meteocons dimensions must match their manifest"
        );
        (0..self.frames)
            .map(|frame| {
                let pixels = ::image::imageops::crop_imm(
                    pixels,
                    frame % self.columns * frame_size,
                    frame / self.columns * frame_size,
                    frame_size,
                    frame_size,
                )
                .to_image();
                image::Handle::from_rgba(frame_size, frame_size, pixels.into_raw())
            })
            .collect()
    }
}

fn manifest() -> &'static Manifest {
    static MANIFEST: OnceLock<Manifest> = OnceLock::new();
    MANIFEST.get_or_init(|| {
        let manifest: Manifest = serde_json::from_slice(include_bytes!(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/assets/weather/meteocons/manifest.json"
        )))
        .expect("bundled Meteocons manifest must be valid");
        assert!(manifest.frame_size > 0 && manifest.frame_ms > 0);
        for atlas in manifest.icons.values() {
            assert!(atlas.columns > 0 && atlas.rows > 0);
            assert!(atlas.frames > 0 && atlas.frames <= atlas.columns * atlas.rows);
            assert!(atlas.frame_ms.unwrap_or(manifest.frame_ms) > 0);
        }
        manifest
    })
}

fn atlas_bytes(name: &str) -> &'static [u8] {
    macro_rules! bundled_atlases {
        ($($name:literal),+ $(,)?) => {
            match name {
                $($name => include_bytes!(concat!(
                    env!("CARGO_MANIFEST_DIR"), "/assets/weather/meteocons/", $name, ".png"
                )),)+
                _ => unreachable!("every weather condition has a bundled atlas"),
            }
        };
    }

    bundled_atlases!(
        "clear-day",
        "clear-night",
        "partly-cloudy-day",
        "partly-cloudy-night",
        "overcast",
        "rain",
        "partly-cloudy-day-rain",
        "partly-cloudy-night-rain",
        "thunderstorms",
        "snow",
        "mist",
        "not-available",
    )
}

struct WeatherIcon {
    name: &'static str,
    atlas: Atlas,
}

impl WeatherIcon {
    fn new(name: &'static str) -> Self {
        Self {
            name,
            atlas: manifest().icons[name],
        }
    }
}

struct State {
    name: &'static str,
    frames: Vec<image::Handle>,
    allocation: RefCell<Option<(u32, image::Allocation)>>,
    clock: FrameClock,
}

impl State {
    fn new(name: &'static str) -> Self {
        let pixels = ::image::load_from_memory(atlas_bytes(name))
            .expect("bundled Meteocons atlas must decode")
            .to_rgba8();
        Self {
            name,
            frames: manifest().icons[name].frame_handles(&pixels, manifest().frame_size),
            allocation: RefCell::new(None),
            clock: FrameClock::default(),
        }
    }

    fn current_handle(&self) -> &image::Handle {
        &self.frames[self.clock.frame as usize]
    }

    fn prepare_frame(&self, renderer: &Renderer) {
        let mut allocation = self.allocation.borrow_mut();
        if allocation
            .as_ref()
            .is_none_or(|(frame, _)| *frame != self.clock.frame)
        {
            // Pin only the current frame. Explicit loading also prevents a GPU
            // backend from leaving a blank frame while uploading it asynchronously.
            *allocation = renderer
                .load_image(self.current_handle())
                .ok()
                .map(|image| (self.clock.frame, image));
        }
    }

    fn sync(&mut self, name: &'static str) {
        if self.name != name {
            *self = Self::new(name);
        }
    }
}

#[derive(Default)]
struct FrameClock {
    previous: Option<Instant>,
    elapsed_nanos: u128,
    frame: u32,
}

impl FrameClock {
    fn advance(
        &mut self,
        now: Instant,
        visible: bool,
        frames: u32,
        frame_duration: Duration,
    ) -> Option<Instant> {
        if !visible || frames <= 1 {
            self.previous = None;
            return None;
        }

        let frame_nanos = frame_duration.as_nanos();
        let cycle_nanos = frame_nanos * u128::from(frames);
        if let Some(previous) = self.previous.replace(now) {
            self.elapsed_nanos = (self.elapsed_nanos
                + now.saturating_duration_since(previous).as_nanos())
                % cycle_nanos;
        }
        self.frame = (self.elapsed_nanos / frame_nanos) as u32;
        let until_next_frame = frame_nanos - self.elapsed_nanos % frame_nanos;
        Some(now + Duration::from_nanos(until_next_frame as u64))
    }
}

impl<Message> Widget<Message, Theme, Renderer> for WeatherIcon {
    fn tag(&self) -> tree::Tag {
        tree::Tag::of::<State>()
    }

    fn state(&self) -> tree::State {
        tree::State::new(State::new(self.name))
    }

    fn diff(&mut self, tree: &mut Tree) {
        tree.state.downcast_mut::<State>().sync(self.name);
    }

    fn size(&self) -> Size<Length> {
        Size::new(Length::Fixed(ICON_SIZE), Length::Fixed(ICON_SIZE))
    }

    fn layout(
        &mut self,
        _tree: &mut Tree,
        _renderer: &Renderer,
        limits: &layout::Limits,
    ) -> layout::Node {
        layout::atomic(limits, ICON_SIZE, ICON_SIZE)
    }

    fn update(
        &mut self,
        tree: &mut Tree,
        event: &Event,
        layout: Layout<'_>,
        _cursor: mouse::Cursor,
        _renderer: &Renderer,
        _clipboard: &mut dyn Clipboard,
        shell: &mut Shell<'_, Message>,
        viewport: &Rectangle,
    ) {
        if let Event::Window(window::Event::RedrawRequested(now)) = event {
            let state = tree.state.downcast_mut::<State>();
            if let Some(next_frame) = state.clock.advance(
                *now,
                layout.bounds().intersects(viewport),
                self.atlas.frames,
                self.atlas.frame_duration(),
            ) {
                shell.request_redraw_at(next_frame);
            }
        }
    }

    fn draw(
        &self,
        tree: &Tree,
        renderer: &mut Renderer,
        _theme: &Theme,
        _style: &renderer::Style,
        layout: Layout<'_>,
        _cursor: mouse::Cursor,
        viewport: &Rectangle,
    ) {
        use cosmic::iced::advanced::Renderer as _;

        let bounds = layout.bounds();
        let Some(clip) = bounds.intersection(viewport) else {
            return;
        };
        let state = tree.state.downcast_ref::<State>();
        state.prepare_frame(renderer);
        // The software backend ignores per-image clip bounds, so keep a layer
        // for viewport clipping even though the image itself is just one frame.
        renderer.with_layer(clip, |renderer| {
            renderer.draw_image(
                image::Image::new(state.current_handle().clone()),
                bounds,
                clip,
            );
        });
    }
}

impl<'a, Message: 'a> From<WeatherIcon> for cosmic::iced::Element<'a, Message, Theme, Renderer> {
    fn from(icon: WeatherIcon) -> Self {
        Self::new(icon)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn rendered_animation_frames_stay_inside_the_icon_bounds() {
        use cosmic::iced::advanced::Renderer as _;
        use cosmic::iced::advanced::renderer::Headless as _;
        use cosmic::iced::{Color, Font, Pixels, Point};

        let mut renderer =
            <Renderer as renderer::Headless>::new(Font::default(), Pixels(14.0), Some("tiny-skia"))
                .await
                .expect("the software renderer must be available for this regression test");
        let icon: Element<'_, ()> = WeatherIcon::new("clear-night").into();
        let mut tree = Tree::new(icon.as_widget());
        let layout =
            layout::Node::new(Size::new(ICON_SIZE, ICON_SIZE)).move_to(Point::new(120.0, 80.0));
        let viewport = Rectangle::with_size(Size::new(320.0, 240.0));
        let size = Size::new(320, 240);
        let theme = Theme::default();
        let style = renderer::Style::default();
        let frame_ids: Vec<_> = tree
            .state
            .downcast_ref::<State>()
            .frames
            .iter()
            .map(image::Handle::id)
            .collect();
        let mut previous = None;

        // Both frames originate from interior atlas cells. Only the selected
        // frame should be visible, at the same position throughout playback.
        for frame in [67, 117] {
            tree.state.downcast_mut::<State>().clock.frame = frame;
            renderer.reset(viewport);
            icon.as_widget().draw(
                &tree,
                &mut renderer,
                &theme,
                &style,
                Layout::new(&layout),
                mouse::Cursor::Unavailable,
                &viewport,
            );
            let pixels = renderer.screenshot(size, 1.0, Color::BLACK);
            assert_eq!(pixels.len(), 320 * 240 * 4);
            let mut colored_inside = 0;
            for (index, pixel) in pixels.chunks_exact(4).enumerate() {
                let x = index % 320;
                let y = index / 320;
                if (120..200).contains(&x) && (80..160).contains(&y) {
                    colored_inside += usize::from(pixel != [0, 0, 0, 255]);
                } else {
                    assert_eq!(
                        pixel,
                        [0, 0, 0, 255],
                        "atlas frame {frame} escaped the icon at ({x}, {y})"
                    );
                }
            }
            assert!(colored_inside > 200, "frame {frame} must visibly render");
            if let Some(previous) = &previous {
                assert_ne!(&pixels, previous, "the rendered icon must animate");
            }
            previous = Some(pixels);
        }
        let state = tree.state.downcast_ref::<State>();
        assert_eq!(
            state
                .frames
                .iter()
                .map(image::Handle::id)
                .collect::<Vec<_>>(),
            frame_ids,
            "drawing must reuse the decoded frames"
        );
        assert_eq!(state.allocation.borrow().as_ref().unwrap().0, 117);
    }

    #[test]
    fn clock_tracks_elapsed_time_and_wraps_without_accumulating_frame_delay() {
        let mut clock = FrameClock::default();
        let start = Instant::now();
        let interval = Duration::from_millis(50);

        assert_eq!(
            clock.advance(start, true, 80, interval),
            Some(start + interval)
        );
        assert_eq!(clock.frame, 0);
        let late = start + Duration::from_millis(127);
        assert_eq!(
            clock.advance(late, true, 80, interval),
            Some(start + Duration::from_millis(150))
        );
        assert_eq!(clock.frame, 2);
        clock.advance(start + Duration::from_millis(4050), true, 80, interval);
        assert_eq!(clock.frame, 1);
    }

    #[test]
    fn hidden_and_static_icons_do_not_schedule_frames() {
        let start = Instant::now();
        let interval = Duration::from_millis(50);
        let mut clock = FrameClock::default();
        clock.advance(start, true, 80, interval);
        clock.advance(start + interval, true, 80, interval);
        assert_eq!(clock.frame, 1);
        assert_eq!(clock.advance(start + interval, false, 80, interval), None);
        let resumed = start + Duration::from_secs(300);
        assert_eq!(
            clock.advance(resumed, true, 80, interval),
            Some(resumed + interval)
        );
        assert_eq!(clock.frame, 1);
        assert_eq!(clock.advance(resumed, true, 1, interval), None);
    }

    #[test]
    fn condition_changes_replace_the_texture_and_reset_animation() {
        let mut state = State::new("clear-day");
        let original_handle = state.frames[0].id();
        state.clock.frame = 7;
        state.sync("clear-day");
        assert_eq!(state.frames[0].id(), original_handle);
        assert_eq!(state.clock.frame, 7);

        state.sync("clear-night");
        assert_ne!(state.frames[0].id(), original_handle);
        assert_eq!(state.name, "clear-night");
        assert_eq!(state.clock.frame, 0);
        assert!(state.clock.previous.is_none());
    }

    #[test]
    fn every_weather_code_maps_to_a_bundled_condition() {
        for condition in ["01", "02", "03", "04", "09", "10", "11", "13", "50"] {
            for period in ['d', 'n'] {
                let name = condition_name(&format!("{condition}{period}"));
                assert!(manifest().icons.contains_key(name));
                assert!(!atlas_bytes(name).is_empty());
            }
        }
        assert_eq!(condition_name(""), "not-available");
        assert_eq!(condition_name("unexpected"), "not-available");
        assert_ne!(condition_name("01d"), condition_name("01n"));
        assert_ne!(condition_name("10d"), condition_name("10n"));
    }

    #[test]
    fn bundled_atlases_match_metadata_and_contain_actual_animation() {
        let manifest = manifest();
        assert!(manifest.frame_size > 0);
        assert!(manifest.frame_ms > 0);
        for (name, atlas) in &manifest.icons {
            assert!(atlas.columns > 0 && atlas.rows > 0, "{name}");
            assert!(
                atlas.frames > 0 && atlas.frames <= atlas.columns * atlas.rows,
                "{name}"
            );
            assert!(!atlas.frame_duration().is_zero(), "{name}");
            let pixels = ::image::load_from_memory(atlas_bytes(name))
                .unwrap_or_else(|error| panic!("{name}: {error}"))
                .to_rgba8();
            assert_eq!(
                pixels.dimensions(),
                (
                    atlas.columns * manifest.frame_size,
                    atlas.rows * manifest.frame_size
                ),
                "{name}"
            );
            let frame = |index: u32| {
                ::image::imageops::crop_imm(
                    &pixels,
                    index % atlas.columns * manifest.frame_size,
                    index / atlas.columns * manifest.frame_size,
                    manifest.frame_size,
                    manifest.frame_size,
                )
                .to_image()
            };
            let first = frame(0);
            assert!(first.pixels().any(|pixel| pixel[3] > 0), "{name} is blank");
            if atlas.frames > 1 {
                assert!(
                    (1..atlas.frames).any(|index| frame(index) != first),
                    "{name} has no changing animation frames"
                );
            }
        }
    }

    #[tokio::test]
    async fn identical_frames_do_not_shift_when_crossing_atlas_rows_or_columns() {
        use cosmic::iced::advanced::Renderer as _;
        use cosmic::iced::advanced::renderer::Headless as _;
        use cosmic::iced::{Color, Font, Pixels, Point};

        let atlas = Atlas {
            columns: 3,
            rows: 3,
            frames: 9,
            frame_ms: None,
        };
        let frame_size = 128;
        let frame = ::image::RgbaImage::from_fn(frame_size, frame_size, |x, y| {
            if (21..93).contains(&x) && (29..101).contains(&y) {
                ::image::Rgba([80, 180, 240, 255])
            } else {
                ::image::Rgba([0, 0, 0, 0])
            }
        });
        let mut pixels = ::image::RgbaImage::new(3 * frame_size, 3 * frame_size);
        for row in 0..3 {
            for column in 0..3 {
                ::image::imageops::replace(
                    &mut pixels,
                    &frame,
                    i64::from(column * frame_size),
                    i64::from(row * frame_size),
                );
            }
        }
        let state = State {
            name: "identical-test-frames",
            frames: atlas.frame_handles(&pixels, frame_size),
            allocation: RefCell::new(None),
            clock: FrameClock::default(),
        };
        assert!(state.frames.iter().all(|handle| {
            matches!(
                handle,
                image::Handle::Rgba {
                    width: 128,
                    height: 128,
                    ..
                }
            )
        }));
        let mut tree = Tree::empty();
        tree.state = tree::State::new(state);
        let icon: Element<'_, ()> = WeatherIcon {
            name: "identical-test-frames",
            atlas,
        }
        .into();
        let mut renderer =
            <Renderer as renderer::Headless>::new(Font::default(), Pixels(14.0), Some("tiny-skia"))
                .await
                .unwrap();
        let viewport = Rectangle::with_size(Size::new(160.0, 180.0));
        // x=24 exposes the old truncation toward zero when a moved atlas origin
        // changes sign. Fractional positions/scales must also stay consistent.
        for (position, scale) in [
            (Point::new(24.0, 40.0), 1.0),
            (Point::new(24.3, 40.7), 1.25),
        ] {
            let layout = layout::Node::new(Size::new(ICON_SIZE, ICON_SIZE)).move_to(position);
            let size = Size::new((160.0 * scale) as u32, (180.0 * scale) as u32);
            let mut first = None;
            for frame in 0..atlas.frames {
                tree.state.downcast_mut::<State>().clock.frame = frame;
                renderer.reset(viewport);
                icon.as_widget().draw(
                    &tree,
                    &mut renderer,
                    &Theme::default(),
                    &renderer::Style::default(),
                    Layout::new(&layout),
                    mouse::Cursor::Unavailable,
                    &viewport,
                );
                let screenshot = renderer.screenshot(size, scale, Color::BLACK);
                assert!(
                    screenshot
                        .chunks_exact(4)
                        .any(|pixel| pixel != [0, 0, 0, 255])
                );
                if let Some(first) = &first {
                    assert!(
                        screenshot == *first,
                        "identical frame {frame} shifted at scale {scale}"
                    );
                } else {
                    first = Some(screenshot);
                }
            }
        }
    }
}
