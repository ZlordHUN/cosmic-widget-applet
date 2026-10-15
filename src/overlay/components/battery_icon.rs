// SPDX-License-Identifier: MPL-2.0

//! A shared battery glyph with a repeating empty-to-full charging fill.
//!
//! The sweep repeats while the actual reading is below 100%. The percentage
//! beside the icon remains the device's real charge; the moving fill indicates
//! charging activity. At 100% the icon stays full and stops requesting frames.
//! Static readings use the same outline and layout, filled to their actual level.
//! Static phases reuse cached geometry and wait for the next change.

use cosmic::iced::advanced::widget::tree::{self, Tree};
use cosmic::iced::advanced::{self, Clipboard, Layout, Shell, Widget, layout, renderer};
use cosmic::iced::time::Instant;
use cosmic::iced::widget::canvas::{self, Fill, Path, Style, fill, path};
use cosmic::iced::{Color, Event, Length, Point, Rectangle, Size, Vector, mouse, window};
use cosmic::{Element, Renderer, Theme};
use std::cell::Cell;
use std::f32::consts::{FRAC_PI_2, PI, TAU};
use std::sync::OnceLock;
use std::time::Duration;

/// Extend the COSMIC glyph horizontally without stretching its height or bolt.
const VIEWBOX_WIDTH: f32 = 28.0;
const VIEWBOX_HEIGHT: f32 = 16.0;
/// COSMIC draws the stored charge at this opacity inside a solid outline.
const CHARGE_OPACITY: f32 = 0.35;
/// The rounded interior of the outline, in icon units.
const CAVITY_LEFT: f32 = 1.0;
const CAVITY_TOP: f32 = 4.0;
const CAVITY_RIGHT: f32 = 25.0;
const CAVITY_BOTTOM: f32 = 12.0;
const CAVITY_RADIUS: f32 = 1.0;
const OUTLINE_WIDTH: f32 = 1.0;
const TERMINAL_RADIUS: f32 = 2.0;
const BOLT: [(f32, f32); 6] = [
    (15.0, 7.0),
    (13.0, 7.0),
    (13.0, 5.0),
    (11.0, 9.0),
    (13.0, 9.0),
    (13.0, 11.0),
];

const SWEEP: Duration = Duration::from_millis(1300);
const HOLD: Duration = Duration::from_millis(350);
const FADE: Duration = Duration::from_millis(550);
/// After the fade, the empty glyph rests before the next fill.
const CYCLE: Duration = Duration::from_millis(2600);
const FRAME_INTERVAL: Duration = Duration::from_millis(50);

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(in crate::overlay) enum Mode {
    Level,
    Charging,
    CachedCharging,
}

/// Draws every known battery reading with the same geometry and actual percentage.
pub(in crate::overlay) fn battery(
    level: u8,
    mode: Mode,
    size: u16,
    color: impl Fn(&Theme) -> Color + 'static,
) -> Element<'static, crate::overlay::Message> {
    BatteryIcon {
        level,
        mode,
        size: f32::from(size),
        color,
    }
    .into()
}

/// Reserve the same horizontal space for static and animated battery icons.
pub(in crate::overlay) fn width(height: u16) -> f32 {
    dimensions(f32::from(height)).width
}

fn dimensions(height: f32) -> Size<f32> {
    Size::new((height * VIEWBOX_WIDTH / VIEWBOX_HEIGHT).ceil(), height)
}

struct BatteryIcon<F> {
    level: u8,
    mode: Mode,
    size: f32,
    color: F,
}

#[derive(Debug, Clone, Copy, PartialEq)]
struct Sweep {
    /// How far the fill has moved from empty toward full.
    progress: f32,
    /// Fill opacity during the reset between sweeps.
    strength: f32,
}

impl Sweep {
    const REST: Self = Self {
        progress: 0.0,
        strength: 0.0,
    };
    const FULL: Self = Self {
        progress: 1.0,
        strength: 1.0,
    };
}

#[derive(Debug, Clone, Copy, PartialEq)]
struct Glyph {
    level: u8,
    mode: Mode,
    sweep: Sweep,
    color: Color,
}

struct State {
    joined: Option<Instant>,
    sweep: Sweep,
    cache: canvas::Cache<Renderer>,
    cached: Cell<Option<Glyph>>,
}

impl Default for State {
    fn default() -> Self {
        Self {
            joined: None,
            sweep: Sweep::REST,
            cache: canvas::Cache::default(),
            cached: Cell::new(None),
        }
    }
}

/// Every charging glyph counts cycles from the same moment so they fill together.
fn shared_epoch(now: Instant) -> Instant {
    static EPOCH: OnceLock<Instant> = OnceLock::new();
    *EPOCH.get_or_init(|| now)
}

/// Returns the sweep at `now` and when it next changes.
///
/// A glyph that `joined` after the current cycle began rests until the next
/// one, so every sweep it shows starts from empty.
fn charge_cycle(epoch: Instant, joined: Instant, now: Instant) -> (Sweep, Instant) {
    let elapsed = now.saturating_duration_since(epoch).as_nanos();
    let phase = Duration::from_nanos(u64::try_from(elapsed % CYCLE.as_nanos()).unwrap_or_default());
    let next_frame = now + FRAME_INTERVAL;
    // Static phases wait for their end, but never less than a frame, so a
    // redraw that lands just early cannot spin.
    let wait_until = |boundary: Duration| now + boundary.saturating_sub(phase).max(FRAME_INTERVAL);
    let eased = |offset: Duration, length: Duration| {
        smoothstep(offset.as_secs_f32() / length.as_secs_f32())
    };

    if now.saturating_duration_since(joined) < phase {
        (Sweep::REST, wait_until(CYCLE))
    } else if phase < SWEEP {
        let sweep = Sweep {
            progress: eased(phase, SWEEP),
            strength: 1.0,
        };
        (sweep, next_frame)
    } else if phase < SWEEP + HOLD {
        (Sweep::FULL, wait_until(SWEEP + HOLD))
    } else if phase < SWEEP + HOLD + FADE {
        let sweep = Sweep {
            progress: 1.0,
            strength: 1.0 - eased(phase.saturating_sub(SWEEP + HOLD), FADE),
        };
        (sweep, next_frame)
    } else {
        (Sweep::REST, wait_until(CYCLE))
    }
}

fn smoothstep(t: f32) -> f32 {
    let t = t.clamp(0.0, 1.0);
    t * t * (3.0 - 2.0 * t)
}

fn draw_glyph(frame: &mut canvas::Frame<Renderer>, glyph: Glyph) {
    let Glyph {
        level,
        mode,
        sweep,
        color,
    } = glyph;
    frame.scale(frame.height() / VIEWBOX_HEIGHT);

    let (progress, strength) = if mode == Mode::Charging && level < 100 {
        (sweep.progress, sweep.strength)
    } else {
        (f32::from(level.min(100)) / 100.0, 1.0)
    };
    let edge = CAVITY_LEFT + (CAVITY_RIGHT - CAVITY_LEFT) * progress;
    if edge > CAVITY_LEFT && strength > 0.0 {
        frame.fill(
            &Path::new(|path| cavity(path, edge)),
            Color {
                a: color.a * CHARGE_OPACITY * strength,
                ..color
            },
        );
    }

    frame.fill(
        &outline(),
        Fill {
            style: Style::Solid(color),
            rule: fill::Rule::EvenOdd,
        },
    );
    if mode != Mode::Level {
        frame.fill(&bolt(), color);
    }
}

fn bolt() -> Path {
    Path::new(|path| {
        let [(x, y), rest @ ..] = BOLT;
        path.move_to(Point::new(x, y));
        for (x, y) in rest {
            path.line_to(Point::new(x, y));
        }
        path.close();
    })
}

/// The outline with its terminal, cut out by the full cavity.
fn outline() -> Path {
    let left = CAVITY_LEFT - OUTLINE_WIDTH;
    let top = CAVITY_TOP - OUTLINE_WIDTH;
    let right = CAVITY_RIGHT + OUTLINE_WIDTH;
    let bottom = CAVITY_BOTTOM + OUTLINE_WIDTH;
    let radius = CAVITY_RADIUS + OUTLINE_WIDTH;
    let terminal = Point::new(right, f32::midpoint(top, bottom));

    Path::new(|path| {
        path.move_to(Point::new(left, top + radius));
        let top_left = Point::new(left + radius, top + radius);
        arc_to(path, top_left, radius, PI, 1.5 * PI);
        path.line_to(Point::new(right - radius, top));
        let top_right = Point::new(right - radius, top + radius);
        arc_to(path, top_right, radius, 1.5 * PI, TAU);
        path.line_to(Point::new(right, terminal.y - TERMINAL_RADIUS));
        arc_to(path, terminal, TERMINAL_RADIUS, -FRAC_PI_2, 0.0);
        arc_to(path, terminal, TERMINAL_RADIUS, 0.0, FRAC_PI_2);
        path.line_to(Point::new(right, bottom - radius));
        let bottom_right = Point::new(right - radius, bottom - radius);
        arc_to(path, bottom_right, radius, 0.0, FRAC_PI_2);
        path.line_to(Point::new(left + radius, bottom));
        let bottom_left = Point::new(left + radius, bottom - radius);
        arc_to(path, bottom_left, radius, FRAC_PI_2, PI);
        path.close();

        cavity(path, CAVITY_RIGHT);
    })
}

/// Adds the part of the cavity left of `edge` as a closed contour that keeps
/// to the cavity's rounded corners.
fn cavity(path: &mut path::Builder, edge: f32) {
    let edge = edge.clamp(CAVITY_LEFT, CAVITY_RIGHT);
    // Centers of the rounded corners.
    let (left, right) = (CAVITY_LEFT + CAVITY_RADIUS, CAVITY_RIGHT - CAVITY_RADIUS);
    let (top, bottom) = (CAVITY_TOP + CAVITY_RADIUS, CAVITY_BOTTOM - CAVITY_RADIUS);
    // Where a lower corner's arc crosses `edge`, as a clockwise angle. The
    // upper corner on the same side crosses it at the mirrored angle.
    let reach = |center_x: f32| ((edge - center_x) / CAVITY_RADIUS).acos();

    path.move_to(Point::new(CAVITY_LEFT, top));
    if edge < left {
        let angle = reach(left);
        arc_to(path, Point::new(left, top), CAVITY_RADIUS, PI, TAU - angle);
        path.line_to(Point::new(edge, bottom + CAVITY_RADIUS * angle.sin()));
        arc_to(path, Point::new(left, bottom), CAVITY_RADIUS, angle, PI);
    } else {
        arc_to(path, Point::new(left, top), CAVITY_RADIUS, PI, 1.5 * PI);
        if edge <= right {
            path.line_to(Point::new(edge, CAVITY_TOP));
            path.line_to(Point::new(edge, CAVITY_BOTTOM));
        } else {
            let angle = reach(right);
            path.line_to(Point::new(right, CAVITY_TOP));
            arc_to(
                path,
                Point::new(right, top),
                CAVITY_RADIUS,
                1.5 * PI,
                TAU - angle,
            );
            path.line_to(Point::new(edge, bottom + CAVITY_RADIUS * angle.sin()));
            arc_to(
                path,
                Point::new(right, bottom),
                CAVITY_RADIUS,
                angle,
                FRAC_PI_2,
            );
        }
        path.line_to(Point::new(left, CAVITY_BOTTOM));
        arc_to(path, Point::new(left, bottom), CAVITY_RADIUS, FRAC_PI_2, PI);
    }
    path.close();
}

/// Continues `path` along a clockwise arc of at most a quarter turn, where a
/// single cubic is indistinguishable from a circle. `path::Builder::arc`
/// would start a new subpath instead of extending the current contour.
fn arc_to(path: &mut path::Builder, center: Point, radius: f32, from: f32, to: f32) {
    let handle = 4.0 / 3.0 * ((to - from) / 4.0).tan() * radius;
    let (from_sin, from_cos) = from.sin_cos();
    let (to_sin, to_cos) = to.sin_cos();
    path.bezier_curve_to(
        Point::new(
            center.x + radius * from_cos - handle * from_sin,
            center.y + radius * from_sin + handle * from_cos,
        ),
        Point::new(
            center.x + radius * to_cos + handle * to_sin,
            center.y + radius * to_sin - handle * to_cos,
        ),
        Point::new(center.x + radius * to_cos, center.y + radius * to_sin),
    );
}

impl<Message, F> Widget<Message, Theme, Renderer> for BatteryIcon<F>
where
    F: Fn(&Theme) -> Color,
{
    fn tag(&self) -> tree::Tag {
        tree::Tag::of::<State>()
    }

    fn state(&self) -> tree::State {
        tree::State::new(State::default())
    }

    fn size(&self) -> Size<Length> {
        let size = dimensions(self.size);
        Size::new(Length::Fixed(size.width), Length::Fixed(size.height))
    }

    fn layout(
        &mut self,
        _tree: &mut Tree,
        _renderer: &Renderer,
        limits: &layout::Limits,
    ) -> layout::Node {
        let size = dimensions(self.size);
        layout::atomic(limits, size.width, size.height)
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
            if self.mode != Mode::Charging || self.level >= 100 {
                state.joined = None;
                state.sweep = Sweep::REST;
                return;
            }
            let joined = *state.joined.get_or_insert(*now);
            let (sweep, next_change) = charge_cycle(shared_epoch(*now), joined, *now);
            state.sweep = sweep;
            if layout.bounds().intersects(viewport) {
                shell.request_redraw_at(next_change);
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
        let state = tree.state.downcast_ref::<State>();
        let glyph = Glyph {
            level: self.level,
            mode: self.mode,
            sweep: state.sweep,
            color: (self.color)(theme),
        };
        if state.cached.replace(Some(glyph)) != Some(glyph) {
            state.cache.clear();
        }
        let geometry = state
            .cache
            .draw(renderer, bounds.size(), |frame| draw_glyph(frame, glyph));

        // Static icons are rasterized at whole pixels. Matching them keeps the
        // glyph from shifting or softening when a device starts charging.
        let origin = Vector::new(bounds.x.floor(), bounds.y.floor());
        renderer.with_translation(origin, |renderer| {
            use cosmic::iced::advanced::graphics::geometry::Renderer as _;

            renderer.draw_geometry(geometry);
        });
    }
}

impl<'a, Message, F> From<BatteryIcon<F>> for cosmic::iced::Element<'a, Message, Theme, Renderer>
where
    F: Fn(&Theme) -> Color + 'a,
{
    fn from(battery: BatteryIcon<F>) -> Self {
        Self::new(battery)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use cosmic::iced::advanced::clipboard;
    use cosmic::iced::advanced::renderer::Headless as _;
    use cosmic::iced::window::RedrawRequest;
    use cosmic::iced::{Font, Pixels};

    const COLOR: Color = Color::from_rgb(0.4, 0.85, 0.55);
    const CANVAS: u16 = 48;

    fn battery(level: u8) -> Element<'static, ()> {
        battery_in_mode(level, Mode::Charging)
    }

    fn battery_in_mode(level: u8, mode: Mode) -> Element<'static, ()> {
        BatteryIcon {
            level,
            mode,
            size: 18.0,
            color: |_: &Theme| COLOR,
        }
        .into()
    }

    async fn headless_renderer() -> Renderer {
        <Renderer as renderer::Headless>::new(Font::default(), Pixels(14.0), Some("tiny-skia"))
            .await
            .expect("the software renderer must be available for rendering tests")
    }

    /// Renders `icon` with its top-left corner at `position` on a black canvas.
    fn render(
        renderer: &mut Renderer,
        icon: &Element<'_, ()>,
        tree: &Tree,
        position: Point,
    ) -> Vec<u8> {
        use cosmic::iced::advanced::Renderer as _;

        let viewport = Rectangle::with_size(Size::new(f32::from(CANVAS), f32::from(CANVAS)));
        let layout = layout::Node::new(dimensions(18.0)).move_to(position);
        renderer.reset(viewport);
        icon.as_widget().draw(
            tree,
            renderer,
            &Theme::default(),
            &renderer::Style::default(),
            Layout::new(&layout),
            mouse::Cursor::Unavailable,
            &viewport,
        );
        let size = Size::new(u32::from(CANVAS), u32::from(CANVAS));
        renderer.screenshot(size, 1.0, Color::BLACK)
    }

    fn pixel(pixels: &[u8], (x, y): (usize, usize)) -> [u8; 4] {
        let index = (y * usize::from(CANVAS) + x) * 4;
        pixels[index..index + 4].try_into().unwrap()
    }

    fn assert_close(actual: f32, expected: f32) {
        assert!(
            (actual - expected).abs() < 1e-4,
            "{actual} is not close to {expected}"
        );
    }

    #[test]
    fn charging_icon_is_wider_without_increasing_row_height() {
        assert_eq!(dimensions(18.0), Size::new(32.0, 18.0));
        assert_eq!(width(18), 32.0);
    }

    #[test]
    fn each_cycle_sweeps_to_full_then_holds_fades_and_rests() {
        let start = Instant::now();
        let at = |ms: u64| start + Duration::from_millis(ms);
        let cycle = |ms| charge_cycle(start, start, at(ms));

        let starting = Sweep {
            progress: 0.0,
            strength: 1.0,
        };
        assert_eq!(cycle(0), (starting, at(50)));
        let (halfway, next) = cycle(650);
        assert_close(halfway.progress, 0.5);
        assert_close(halfway.strength, 1.0);
        assert_eq!(next, at(700));

        // Nothing moves while the charge holds at full or rests, so the next
        // frame waits for the following phase.
        assert_eq!(cycle(1400), (Sweep::FULL, at(1650)));
        let (fading, next) = cycle(1925);
        assert_close(fading.progress, 1.0);
        assert_close(fading.strength, 0.5);
        assert_eq!(next, at(1975));
        assert_eq!(cycle(2300), (Sweep::REST, at(2600)));

        // A redraw that lands just before a phase change still waits a frame.
        assert_eq!(cycle(1640).1, at(1690));
        assert_eq!(cycle(2600 + 650).0, halfway);
    }

    #[test]
    fn batteries_that_start_charging_mid_cycle_wait_for_the_next_sweep() {
        let epoch = Instant::now();
        let at = |ms: u64| epoch + Duration::from_millis(ms);
        let joined = at(3000);

        assert_eq!(charge_cycle(epoch, joined, joined), (Sweep::REST, at(5200)));
        // The first sweep starts from empty, in step with every
        // battery that was already charging.
        let (sweep, _) = charge_cycle(epoch, joined, at(5200));
        assert_close(sweep.progress, 0.0);
        let later = at(5200 + 650);
        assert_eq!(
            charge_cycle(epoch, joined, later),
            charge_cycle(epoch, epoch, later)
        );
    }

    #[tokio::test]
    async fn charging_sweeps_from_empty_to_full_even_at_99_percent() {
        let mut renderer = headless_renderer().await;
        for level in [12, 80, 96, 99] {
            let icon = battery(level);
            let mut tree = Tree::new(icon.as_widget());
            let mut frame = |sweep| {
                tree.state.downcast_mut::<State>().sweep = sweep;
                render(&mut renderer, &icon, &tree, Point::new(10.0, 10.0))
            };
            let rest = frame(Sweep::REST);
            let halfway = frame(Sweep {
                progress: 0.5,
                strength: 1.0,
            });
            let full = frame(Sweep::FULL);
            let returned = frame(Sweep::REST);
            // Sample each end of the cavity, clear of the centered bolt.
            let (left, right) = ((15, 19), (35, 19));
            assert_eq!(pixel(&rest, left), [0, 0, 0, 255]);
            assert_eq!(pixel(&rest, right), [0, 0, 0, 255]);
            assert_ne!(pixel(&halfway, left), pixel(&rest, left));
            assert_eq!(pixel(&halfway, right), pixel(&rest, right));
            assert_eq!(
                pixel(&halfway, left),
                pixel(&full, left),
                "the fill travels horizontally without pulsing its brightness"
            );
            assert_eq!(pixel(&full, right), pixel(&full, left));
            assert_eq!(returned, rest, "each sweep must reset to empty");
            let mut solid_pixels = 0;
            for (before, after) in rest.chunks_exact(4).zip(full.chunks_exact(4)) {
                if before[0] == 102 && before[1] >= 216 && before[2] >= 140 {
                    assert_eq!(before, after, "outline and bolt must stay fixed");
                    solid_pixels += 1;
                }
            }
            assert!(solid_pixels > 0);
            for pixels in [&rest, &halfway, &full] {
                for (index, pixel) in pixels.chunks_exact(4).enumerate() {
                    let (x, y) = (index % usize::from(CANVAS), index / usize::from(CANVAS));
                    if !(10..42).contains(&x) || !(10..28).contains(&y) {
                        assert_eq!(pixel, [0, 0, 0, 255], "glyph must fit inside 32×18");
                    }
                }
            }
        }
    }

    #[tokio::test]
    async fn actual_100_percent_stays_full_across_animation_phases() {
        let mut renderer = headless_renderer().await;
        let icon = battery(100);
        let mut tree = Tree::new(icon.as_widget());
        let full = render(&mut renderer, &icon, &tree, Point::new(10.0, 10.0));
        assert_ne!(pixel(&full, (35, 19)), [0, 0, 0, 255]);
        for sweep in [
            Sweep::FULL,
            Sweep {
                progress: 0.5,
                strength: 1.0,
            },
            Sweep::REST,
        ] {
            tree.state.downcast_mut::<State>().sweep = sweep;
            assert_eq!(
                render(&mut renderer, &icon, &tree, Point::new(10.0, 10.0)),
                full
            );
        }
    }

    #[tokio::test]
    async fn mode_changes_keep_the_same_geometry_and_refresh_the_cached_glyph() {
        let mut renderer = headless_renderer().await;
        let mut static_icon = battery_in_mode(50, Mode::Level);
        let mut charging_icon = battery_in_mode(50, Mode::Charging);
        let mut cached_icon = battery_in_mode(50, Mode::CachedCharging);
        let mut tree = Tree::new(static_icon.as_widget());
        let position = Point::new(10.4, 10.7);
        let resting = render(&mut renderer, &static_icon, &tree, position);
        assert_ne!(pixel(&resting, (15, 19)), [0, 0, 0, 255]);
        assert_eq!(pixel(&resting, (35, 19)), [0, 0, 0, 255]);

        tree.diff(charging_icon.as_widget_mut());
        tree.state.downcast_mut::<State>().sweep = Sweep {
            progress: 0.5,
            strength: 1.0,
        };
        let charging = render(&mut renderer, &charging_icon, &tree, position);
        assert_ne!(
            charging, resting,
            "the charging bolt must appear on the reused tree"
        );
        assert_eq!(
            static_icon.as_widget().size(),
            charging_icon.as_widget().size()
        );
        for (index, (normal, plugged_in)) in resting
            .chunks_exact(4)
            .zip(charging.chunks_exact(4))
            .enumerate()
        {
            let (x, y) = (index % usize::from(CANVAS), index / usize::from(CANVAS));
            // Only the centered lightning bolt changes at the same fill level.
            if !(21..28).contains(&x) || !(15..24).contains(&y) {
                assert_eq!(normal, plugged_in, "battery geometry shifted at {x},{y}");
            }
        }

        tree.diff(cached_icon.as_widget_mut());
        tree.state.downcast_mut::<State>().sweep = Sweep::FULL;
        assert_eq!(
            render(&mut renderer, &cached_icon, &tree, position),
            charging,
            "cached charging must show the actual level, independent of stale sweep state"
        );
        tree.diff(static_icon.as_widget_mut());
        assert_eq!(
            render(&mut renderer, &static_icon, &tree, position),
            resting,
            "unplugging must remove the bolt and restore the same static outline and level"
        );
    }

    #[tokio::test]
    async fn fractional_layouts_render_on_whole_pixels_like_static_icons() {
        let mut renderer = headless_renderer().await;
        let icon = battery(50);
        let tree = Tree::new(icon.as_widget());

        assert_eq!(
            render(&mut renderer, &icon, &tree, Point::new(10.4, 10.7)),
            render(&mut renderer, &icon, &tree, Point::new(10.0, 10.0)),
        );
    }

    #[tokio::test]
    async fn only_visible_charging_batteries_below_actual_100_request_frames() {
        let renderer = headless_renderer().await;
        let now = Instant::now();
        let layout = layout::Node::new(dimensions(18.0));
        let redraw_request = |level, mode, viewport: Rectangle| {
            let mut icon = battery_in_mode(level, mode);
            let mut tree = Tree::new(icon.as_widget());
            let mut messages = Vec::new();
            let mut shell = Shell::new(&mut messages);
            icon.as_widget_mut().update(
                &mut tree,
                &Event::Window(window::Event::RedrawRequested(now)),
                Layout::new(&layout),
                mouse::Cursor::Unavailable,
                &renderer,
                &mut clipboard::Null,
                &mut shell,
                &viewport,
            );
            shell.redraw_request()
        };
        let visible = Rectangle::with_size(Size::new(40.0, 40.0));
        let hidden = Rectangle::new(Point::new(100.0, 100.0), Size::new(40.0, 40.0));

        for mode in [Mode::Level, Mode::CachedCharging] {
            assert!(matches!(
                redraw_request(50, mode, visible),
                RedrawRequest::Wait
            ));
        }
        assert!(matches!(
            redraw_request(50, Mode::Charging, visible),
            RedrawRequest::At(next) if next > now
        ));
        for level in [96, 99] {
            assert!(
                matches!(redraw_request(level, Mode::Charging, visible), RedrawRequest::At(next) if next > now)
            );
        }
        assert!(matches!(
            redraw_request(100, Mode::Charging, visible),
            RedrawRequest::Wait
        ));
        assert!(matches!(
            redraw_request(50, Mode::Charging, hidden),
            RedrawRequest::Wait
        ));
        assert!(matches!(
            redraw_request(100, Mode::Charging, hidden),
            RedrawRequest::Wait
        ));
    }
}
