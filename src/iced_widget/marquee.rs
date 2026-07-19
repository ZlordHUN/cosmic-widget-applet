// SPDX-License-Identifier: MPL-2.0

use cosmic::iced::advanced::text::{self, Paragraph as _, Renderer as _};
use cosmic::iced::advanced::widget::tree::{self, Tree};
use cosmic::iced::advanced::{Clipboard, Layout, Renderer as _, Shell, Widget, layout, renderer};
use cosmic::iced::{Event, Length, Point, Rectangle, Size, alignment, mouse, window};
use cosmic::{Element, Renderer, Theme};
use std::time::Duration;

const TITLE_HEIGHT: f32 = 22.0;
const TITLE_TEXT_SIZE: f32 = 16.0;
const SUBTITLE_HEIGHT: f32 = 20.0;
const SUBTITLE_TEXT_SIZE: f32 = 14.0;
const GAP: f32 = 32.0;
const SPEED: f32 = 28.0;
const FRAME_INTERVAL: Duration = Duration::from_millis(33);

pub fn media_title(title: &str) -> Element<'static, super::Message> {
    Marquee::new(title, MarqueeKind::Title).into()
}

pub fn media_subtitle(subtitle: &str) -> Element<'static, super::Message> {
    Marquee::new(subtitle, MarqueeKind::Subtitle).into()
}

#[derive(Debug, Clone, Copy)]
enum MarqueeKind {
    Title,
    Subtitle,
}

impl MarqueeKind {
    fn height(self) -> f32 {
        match self {
            Self::Title => TITLE_HEIGHT,
            Self::Subtitle => SUBTITLE_HEIGHT,
        }
    }

    fn text_size(self) -> f32 {
        match self {
            Self::Title => TITLE_TEXT_SIZE,
            Self::Subtitle => SUBTITLE_TEXT_SIZE,
        }
    }

    fn font(self) -> cosmic::iced::Font {
        match self {
            Self::Title => cosmic::font::semibold(),
            Self::Subtitle => cosmic::font::default(),
        }
    }
}

struct Marquee {
    title: String,
    kind: MarqueeKind,
}

impl Marquee {
    fn new(title: &str, kind: MarqueeKind) -> Self {
        Self {
            title: title.trim().to_string(),
            kind,
        }
    }

    fn paragraph(&self) -> <Renderer as text::Renderer>::Paragraph {
        <Renderer as text::Renderer>::Paragraph::with_text(text::Text {
            content: &self.title,
            bounds: Size::new(10_000.0, self.kind.height()),
            size: self.kind.text_size().into(),
            line_height: text::LineHeight::Absolute(self.kind.height().into()),
            font: self.kind.font(),
            align_x: text::Alignment::Default,
            align_y: alignment::Vertical::Top,
            shaping: text::Shaping::Advanced,
            wrapping: text::Wrapping::None,
            ellipsize: text::Ellipsize::None,
        })
    }
}

struct State {
    title: String,
    text_width: f32,
    viewport_width: f32,
    offset: f32,
    last_frame: Option<cosmic::iced::time::Instant>,
}

impl Default for State {
    fn default() -> Self {
        Self {
            title: String::new(),
            text_width: 0.0,
            viewport_width: 0.0,
            offset: 0.0,
            last_frame: None,
        }
    }
}

impl State {
    fn reset(&mut self, title: &str) {
        self.title.clear();
        self.title.push_str(title);
        self.offset = 0.0;
        self.last_frame = None;
    }

    fn overflowing(&self) -> bool {
        self.text_width > self.viewport_width + 0.5
    }

    fn advance(&mut self, now: cosmic::iced::time::Instant) {
        let Some(last_frame) = self.last_frame.replace(now) else {
            return;
        };
        let elapsed = (now - last_frame).min(Duration::from_millis(50));

        self.offset += SPEED * elapsed.as_secs_f32();
        let cycle = self.text_width + GAP;
        if self.offset >= cycle {
            self.offset = 0.0;
        }
    }
}

impl<Message> Widget<Message, Theme, Renderer> for Marquee
where
    Message: Clone,
{
    fn tag(&self) -> tree::Tag {
        tree::Tag::of::<State>()
    }

    fn state(&self) -> tree::State {
        tree::State::new(State::default())
    }

    fn size(&self) -> Size<Length> {
        Size::new(Length::Fill, Length::Fixed(self.kind.height()))
    }

    fn layout(
        &mut self,
        tree: &mut Tree,
        _renderer: &Renderer,
        limits: &layout::Limits,
    ) -> layout::Node {
        let node = layout::atomic(limits, Length::Fill, Length::Fixed(self.kind.height()));
        let paragraph = self.paragraph();
        let state = tree.state.downcast_mut::<State>();

        if state.title != self.title {
            state.reset(&self.title);
        }
        state.text_width = paragraph.min_width();
        state.viewport_width = node.size().width;
        if !state.overflowing() {
            state.offset = 0.0;
            state.last_frame = None;
        }

        node
    }

    fn update(
        &mut self,
        tree: &mut Tree,
        event: &Event,
        _layout: Layout<'_>,
        _cursor: mouse::Cursor,
        _renderer: &Renderer,
        _clipboard: &mut dyn Clipboard,
        shell: &mut Shell<'_, Message>,
        _viewport: &Rectangle,
    ) {
        let state = tree.state.downcast_mut::<State>();
        if let Event::Window(window::Event::RedrawRequested(now)) = event
            && state.overflowing()
        {
            state.advance(*now);
            shell.request_redraw_at(*now + FRAME_INTERVAL);
        }
    }

    fn draw(
        &self,
        tree: &Tree,
        renderer: &mut Renderer,
        _theme: &Theme,
        style: &renderer::Style,
        layout: Layout<'_>,
        _cursor: mouse::Cursor,
        _viewport: &Rectangle,
    ) {
        let bounds = layout.bounds();
        let state = tree.state.downcast_ref::<State>();
        let height = self.kind.height();
        let text_size = self.kind.text_size();
        let font = self.kind.font();
        let draw_title = |renderer: &mut Renderer, content: &str, x: f32| {
            renderer.fill_text(
                text::Text {
                    content: content.to_string(),
                    bounds: Size::new(state.text_width.max(bounds.width), height),
                    size: text_size.into(),
                    line_height: text::LineHeight::Absolute(height.into()),
                    font,
                    align_x: text::Alignment::Default,
                    align_y: alignment::Vertical::Top,
                    shaping: text::Shaping::Advanced,
                    wrapping: text::Wrapping::None,
                    ellipsize: text::Ellipsize::None,
                },
                Point::new(x, bounds.y),
                style.text_color,
                bounds,
            );
        };

        renderer.with_layer(bounds, |renderer| {
            draw_title(renderer, &self.title, bounds.x - state.offset);
            if state.overflowing() {
                draw_title(
                    renderer,
                    &self.title,
                    bounds.x - state.offset + state.text_width + GAP,
                );
            }
        });
    }
}

impl<'a, Message> From<Marquee> for cosmic::iced::Element<'a, Message, Theme, Renderer>
where
    Message: Clone + 'a,
{
    fn from(marquee: Marquee) -> Self {
        Self::new(marquee)
    }
}

#[cfg(test)]
mod tests {
    use super::{GAP, SPEED, State};
    use std::time::Duration;

    #[test]
    fn marquee_scrolls_continuously_without_leaving_a_blank_cycle() {
        let start = cosmic::iced::time::Instant::now();
        let mut state = State {
            text_width: 100.0,
            viewport_width: 50.0,
            ..State::default()
        };

        state.advance(start);
        state.advance(start + Duration::from_millis(50));
        assert!(state.offset > 0.0);
        state.offset = state.text_width + GAP - SPEED / 100.0;
        state.advance(start + Duration::from_millis(100));
        assert!(state.offset < state.text_width + GAP);
    }
}
