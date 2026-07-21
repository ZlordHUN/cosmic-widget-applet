// SPDX-License-Identifier: MPL-2.0

use crate::config::TemperatureGaugeStyle;
use cosmic::iced::advanced::widget::tree::{self, Tree};
use cosmic::iced::advanced::{self, Clipboard, Layout, Shell, Widget, layout, renderer};
use cosmic::iced::widget::{Stack, canvas};
use cosmic::iced::{
    Border, Color, Event, Length, Point, Radians, Rectangle, Size, Vector, mouse, window,
};
use cosmic::{Element, Renderer, Theme, widget};
use std::f32::consts::PI;
use std::time::Duration;

const GAUGE_SIZE: f32 = 112.0;
const TRACK_WIDTH: f32 = 8.0;
const BAR_HEIGHT: f32 = 8.0;
const START_ANGLE: f32 = 3.0 * PI / 4.0;
const SWEEP_ANGLE: f32 = 3.0 * PI / 2.0;
const ANIMATION_LAG: Duration = Duration::from_millis(280);
const ANIMATION_FRAME_INTERVAL: Duration = Duration::from_millis(20);
const MAX_ANIMATION_FRAME_DELTA: Duration = Duration::from_millis(100);
const SNAP_THRESHOLD: f32 = 0.0005;

fn gauge_angles(style: TemperatureGaugeStyle) -> (f32, f32) {
    match style {
        TemperatureGaugeStyle::Arc => (START_ANGLE, SWEEP_ANGLE),
        TemperatureGaugeStyle::Circular => (-PI / 2.0, 2.0 * PI),
        TemperatureGaugeStyle::Text => (START_ANGLE, SWEEP_ANGLE),
    }
}

pub fn temperature_gauge(
    value: f32,
    style: TemperatureGaugeStyle,
) -> Element<'static, super::Message> {
    let value = value.clamp(0.0, 100.0);
    let ring: Element<'static, super::Message> = TemperatureGauge::new(value / 100.0, style).into();
    let value: Element<'static, super::Message> =
        widget::container(widget::text::title4(format_temperature(value)))
            .center_x(Length::Fixed(GAUGE_SIZE))
            .center_y(Length::Fixed(GAUGE_SIZE))
            .into();

    Stack::<super::Message, Theme, Renderer>::with_children(vec![ring, value]).into()
}

pub fn indicator_bar(value: f32) -> Element<'static, super::Message> {
    UtilizationBar::new(value / 100.0).into()
}

fn format_temperature(value: f32) -> String {
    format!("{value:.0}\u{b0}C")
}

struct TemperatureGauge {
    target: f32,
    style: TemperatureGaugeStyle,
}

impl TemperatureGauge {
    fn new(target: f32, style: TemperatureGaugeStyle) -> Self {
        Self {
            target: target.clamp(0.0, 1.0),
            style,
        }
    }
}

#[derive(Default)]
struct State {
    current: f32,
    last_frame: Option<cosmic::iced::time::Instant>,
}

#[derive(Default)]
struct TemperatureState {
    progress: State,
    cache: canvas::Cache<Renderer>,
    style: TemperatureGaugeStyle,
}

impl State {
    fn update(&mut self, target: f32, now: cosmic::iced::time::Instant) -> bool {
        let Some(last_frame) = self.last_frame.replace(now) else {
            self.current = target;
            return false;
        };

        let difference = target - self.current;
        if difference.abs() <= SNAP_THRESHOLD {
            self.current = target;
            return false;
        }

        let elapsed = (now - last_frame).min(MAX_ANIMATION_FRAME_DELTA);
        let lag = ANIMATION_LAG.as_secs_f32();
        self.current += difference * (1.0 - (-elapsed.as_secs_f32() / lag).exp());
        true
    }
}

struct UtilizationBar {
    target: f32,
}

impl UtilizationBar {
    fn new(target: f32) -> Self {
        Self {
            target: target.clamp(0.0, 1.0),
        }
    }
}

impl<Message> Widget<Message, Theme, Renderer> for UtilizationBar
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
        Size::new(Length::Fill, Length::Fixed(BAR_HEIGHT))
    }

    fn layout(
        &mut self,
        _tree: &mut Tree,
        _renderer: &Renderer,
        limits: &layout::Limits,
    ) -> layout::Node {
        layout::atomic(limits, Length::Fill, Length::Fixed(BAR_HEIGHT))
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
        if let Event::Window(window::Event::RedrawRequested(now)) = event
            && tree.state.downcast_mut::<State>().update(self.target, *now)
        {
            shell.request_redraw_at(*now + ANIMATION_FRAME_INTERVAL);
        }
    }

    fn draw(
        &self,
        tree: &Tree,
        renderer: &mut Renderer,
        theme: &Theme,
        _style: &renderer::Style,
        layout: Layout<'_>,
        _cursor: mouse::Cursor,
        _viewport: &Rectangle,
    ) {
        use advanced::Renderer as _;

        let bounds = layout.bounds();
        let current = tree.state.downcast_ref::<State>().current;
        let meter_style = meter_style(theme);
        let active = indicator_color(theme, self.target * 100.0, meter_style.bar_color);
        let border = Border {
            width: meter_style.border_color.map_or(0.0, |_| 1.0),
            color: meter_style.border_color.unwrap_or(meter_style.bar_color),
            radius: meter_style.border_radius.into(),
        };
        let draw_quad = |renderer: &mut Renderer, color| {
            renderer.fill_quad(
                renderer::Quad {
                    bounds,
                    border,
                    snap: true,
                    ..renderer::Quad::default()
                },
                color,
            );
        };

        draw_quad(renderer, meter_style.track_color);
        if current > SNAP_THRESHOLD {
            let fill = Rectangle {
                width: bounds.width * current,
                ..bounds
            };
            renderer.with_layer(fill, |renderer| draw_quad(renderer, active));
        }
    }
}

impl<'a, Message> From<UtilizationBar> for cosmic::iced::Element<'a, Message, Theme, Renderer>
where
    Message: Clone + 'a,
{
    fn from(bar: UtilizationBar) -> Self {
        Self::new(bar)
    }
}

impl<Message> Widget<Message, Theme, Renderer> for TemperatureGauge
where
    Message: Clone,
{
    fn tag(&self) -> tree::Tag {
        tree::Tag::of::<TemperatureState>()
    }

    fn state(&self) -> tree::State {
        tree::State::new(TemperatureState::default())
    }

    fn size(&self) -> Size<Length> {
        Size::new(Length::Fixed(GAUGE_SIZE), Length::Fixed(GAUGE_SIZE))
    }

    fn layout(
        &mut self,
        _tree: &mut Tree,
        _renderer: &Renderer,
        limits: &layout::Limits,
    ) -> layout::Node {
        layout::atomic(limits, GAUGE_SIZE, GAUGE_SIZE)
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
        if let Event::Window(window::Event::RedrawRequested(now)) = event {
            let state = tree.state.downcast_mut::<TemperatureState>();
            let style_changed = state.style != self.style;
            if style_changed {
                state.style = self.style;
                state.cache.clear();
            }
            if state.progress.update(self.target, *now) {
                state.cache.clear();
                shell.request_redraw_at(*now + ANIMATION_FRAME_INTERVAL);
            }
        }
    }

    fn draw(
        &self,
        tree: &Tree,
        renderer: &mut Renderer,
        theme: &Theme,
        _style: &renderer::Style,
        layout: Layout<'_>,
        _cursor: mouse::Cursor,
        _viewport: &Rectangle,
    ) {
        use advanced::Renderer as _;

        let bounds = layout.bounds();
        let state = tree.state.downcast_ref::<TemperatureState>();
        let current = state.progress.current;
        let meter_style = meter_style(theme);
        let track = meter_style.track_color;
        let active = indicator_color(theme, current * 100.0, meter_style.bar_color);
        let geometry = state.cache.draw(renderer, bounds.size(), |frame| {
            let radius = (GAUGE_SIZE - TRACK_WIDTH) / 2.0;

            match self.style {
                TemperatureGaugeStyle::Arc => {
                    stroke_arc(
                        frame,
                        radius,
                        START_ANGLE,
                        START_ANGLE + SWEEP_ANGLE,
                        track,
                        canvas::LineCap::Butt,
                    );
                    fill_arc_cap(frame, radius, START_ANGLE + SWEEP_ANGLE, track);
                }
                TemperatureGaugeStyle::Circular => {
                    frame.stroke(
                        &canvas::Path::circle(frame.center(), radius),
                        canvas::Stroke::default()
                            .with_color(track)
                            .with_width(TRACK_WIDTH),
                    );
                }
                TemperatureGaugeStyle::Text => {
                    stroke_arc(
                        frame,
                        radius,
                        START_ANGLE,
                        START_ANGLE + SWEEP_ANGLE,
                        track,
                        canvas::LineCap::Butt,
                    );
                    fill_arc_cap(frame, radius, START_ANGLE + SWEEP_ANGLE, track);
                }
            }

            if current > SNAP_THRESHOLD {
                let (start, sweep) = gauge_angles(self.style);
                if self.style == TemperatureGaugeStyle::Circular && current >= 1.0 - SNAP_THRESHOLD
                {
                    frame.stroke(
                        &canvas::Path::circle(frame.center(), radius),
                        canvas::Stroke::default()
                            .with_color(active)
                            .with_width(TRACK_WIDTH),
                    );
                } else {
                    stroke_arc(
                        frame,
                        radius,
                        start,
                        start + sweep * current,
                        active,
                        canvas::LineCap::Round,
                    );
                }
            } else if self.style == TemperatureGaugeStyle::Arc {
                fill_arc_cap(frame, radius, START_ANGLE, track);
            }
        });

        renderer.with_translation(Vector::new(bounds.x, bounds.y), |renderer| {
            use cosmic::iced::advanced::graphics::geometry::Renderer as _;

            renderer.draw_geometry(geometry);
        });
    }
}

impl<'a, Message> From<TemperatureGauge> for cosmic::iced::Element<'a, Message, Theme, Renderer>
where
    Message: Clone + 'a,
{
    fn from(gauge: TemperatureGauge) -> Self {
        Self::new(gauge)
    }
}

fn stroke_arc(
    frame: &mut canvas::Frame<Renderer>,
    radius: f32,
    start_angle: f32,
    end_angle: f32,
    color: Color,
    line_cap: canvas::LineCap,
) {
    let mut builder = canvas::path::Builder::new();
    builder.arc(canvas::path::Arc {
        center: frame.center(),
        radius,
        start_angle: Radians(start_angle),
        end_angle: Radians(end_angle),
    });
    frame.stroke(
        &builder.build(),
        canvas::Stroke::default()
            .with_color(color)
            .with_width(TRACK_WIDTH)
            .with_line_cap(line_cap),
    );
}

fn fill_arc_cap(frame: &mut canvas::Frame<Renderer>, radius: f32, angle: f32, color: Color) {
    let center = frame.center();
    let cap_center = Point::new(
        center.x + radius * angle.cos(),
        center.y + radius * angle.sin(),
    );
    frame.fill(&canvas::Path::circle(cap_center, TRACK_WIDTH / 2.0), color);
}

fn meter_style(theme: &Theme) -> cosmic::widget::progress_bar::style::Appearance {
    <Theme as cosmic::widget::progress_bar::style::StyleSheet>::appearance(theme, &(), true, false)
}

fn indicator_color(theme: &Theme, value: f32, normal: Color) -> Color {
    match indicator_band(value) {
        IndicatorBand::Normal => normal,
        IndicatorBand::Warning => theme.cosmic().warning.base.into(),
        IndicatorBand::Danger => theme.cosmic().destructive.base.into(),
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum IndicatorBand {
    Normal,
    Warning,
    Danger,
}

fn indicator_band(value: f32) -> IndicatorBand {
    if value < 50.0 {
        IndicatorBand::Normal
    } else if value < 80.0 {
        IndicatorBand::Warning
    } else {
        IndicatorBand::Danger
    }
}

#[cfg(test)]
mod tests {
    use super::{
        ANIMATION_FRAME_INTERVAL, IndicatorBand, State, format_temperature, gauge_angles,
        indicator_band,
    };
    use crate::config::TemperatureGaugeStyle;
    use cosmic::iced::time::Instant;
    use std::time::Duration;

    #[test]
    fn progress_snaps_initially_then_interpolates_new_targets() {
        let start = Instant::now();
        let mut state = State::default();

        assert!(!state.update(0.25, start));
        assert_eq!(state.current, 0.25);

        assert!(state.update(0.75, start + Duration::from_millis(16)));
        assert!(state.current > 0.25);
        assert!(state.current < 0.75);
    }

    #[test]
    fn progress_does_not_front_load_target_changes() {
        let start = Instant::now();
        let mut state = State::default();
        state.update(0.0, start);

        state.update(1.0, start + ANIMATION_FRAME_INTERVAL);

        assert!(state.current > 0.0);
        assert!(state.current < 0.1);
    }

    #[test]
    fn temperature_label_identifies_celsius() {
        assert_eq!(format_temperature(63.4), "63\u{b0}C");
    }

    #[test]
    fn indicators_share_value_bands() {
        assert_eq!(indicator_band(49.9), IndicatorBand::Normal);
        assert_eq!(indicator_band(50.0), IndicatorBand::Warning);
        assert_eq!(indicator_band(79.9), IndicatorBand::Warning);
        assert_eq!(indicator_band(80.0), IndicatorBand::Danger);
    }

    #[test]
    fn circular_style_uses_a_complete_ring() {
        let (_, arc_sweep) = gauge_angles(TemperatureGaugeStyle::Arc);
        let (_, circular_sweep) = gauge_angles(TemperatureGaugeStyle::Circular);

        assert!((arc_sweep - 3.0 * std::f32::consts::PI / 2.0).abs() < f32::EPSILON);
        assert!((circular_sweep - 2.0 * std::f32::consts::PI).abs() < f32::EPSILON);
    }
}
