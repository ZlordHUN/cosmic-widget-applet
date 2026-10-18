// SPDX-License-Identifier: MPL-2.0

//! Native slider interaction with the same pixel-snapped rail as other meters.

use std::cell::Cell;
use std::rc::Rc;

use cosmic::iced::advanced::widget::Operation;
use cosmic::iced::advanced::widget::tree::{self, Tree};
use cosmic::iced::advanced::{self, Clipboard, Layout, Shell, Widget, layout, overlay, renderer};
use cosmic::iced::widget::slider::{Catalog as _, HandleShape, Status};
use cosmic::iced::{Border, Event, Length, Rectangle, Size, Vector, mouse};
use cosmic::{Element, Renderer, Theme};

use super::{gauge, track};

const HIT_HEIGHT: f32 = 20.0;
const ACTIVE_TRACK_HEIGHT: f32 = 8.0;

pub(in crate::overlay) fn view<'a, Message: Clone + 'a>(
    progress: f64,
    hovered: bool,
    on_change: impl Fn(f64) -> Message + 'a,
    on_release: Message,
) -> Element<'a, Message> {
    Element::new(build::<Message, Renderer>(
        progress, hovered, on_change, on_release,
    ))
}

fn build<'a, Message: Clone + 'a, R: advanced::Renderer + 'a>(
    progress: f64,
    hovered: bool,
    on_change: impl Fn(f64) -> Message + 'a,
    on_release: Message,
) -> MediaSlider<'a, Message, R> {
    let progress = if progress.is_finite() {
        progress.clamp(0.0, 1.0)
    } else {
        0.0
    };
    let current = Rc::new(Cell::new(progress as f32));
    let changed = Rc::clone(&current);
    let content = cosmic::iced::widget::Slider::new(0.0..=1.0, progress, move |value| {
        // Follow a drag immediately, including before the application rebuilds
        // the view after receiving the normal native slider message.
        changed.set(value as f32);
        on_change(value)
    })
    .step(0.001)
    .height(HIT_HEIGHT)
    .on_release(on_release)
    .into();
    MediaSlider {
        content,
        current,
        hovered,
    }
}

struct MediaSlider<'a, Message, R> {
    content: cosmic::iced::Element<'a, Message, Theme, R>,
    current: Rc<Cell<f32>>,
    hovered: bool,
}

#[derive(Default)]
struct VisualState {
    pointer_left: bool,
}

impl<Message, R: advanced::Renderer> Widget<Message, Theme, R> for MediaSlider<'_, Message, R> {
    fn tag(&self) -> tree::Tag {
        tree::Tag::of::<VisualState>()
    }

    fn state(&self) -> tree::State {
        tree::State::new(VisualState::default())
    }

    fn children(&self) -> Vec<Tree> {
        vec![Tree::new(self.content.as_widget())]
    }

    fn diff(&mut self, tree: &mut Tree) {
        tree.diff_children(std::slice::from_mut(&mut self.content));
    }

    fn size(&self) -> Size<Length> {
        self.content.as_widget().size()
    }

    fn size_hint(&self) -> Size<Length> {
        self.content.as_widget().size_hint()
    }

    fn layout(&mut self, tree: &mut Tree, renderer: &R, limits: &layout::Limits) -> layout::Node {
        self.content
            .as_widget_mut()
            .layout(&mut tree.children[0], renderer, limits)
    }

    fn operate(
        &mut self,
        tree: &mut Tree,
        layout: Layout<'_>,
        renderer: &R,
        operation: &mut dyn Operation,
    ) {
        self.content
            .as_widget_mut()
            .operate(&mut tree.children[0], layout, renderer, operation);
    }

    fn update(
        &mut self,
        tree: &mut Tree,
        event: &Event,
        layout: Layout<'_>,
        cursor: mouse::Cursor,
        renderer: &R,
        clipboard: &mut dyn Clipboard,
        shell: &mut Shell<'_, Message>,
        viewport: &Rectangle,
    ) {
        let visual = tree.state.downcast_mut::<VisualState>();
        if matches!(event, Event::Mouse(mouse::Event::CursorLeft)) {
            visual.pointer_left = true;
        } else if cursor.position().is_some()
            || matches!(event, Event::Mouse(mouse::Event::CursorEntered))
        {
            visual.pointer_left = false;
        }
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
        renderer: &R,
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
        renderer: &mut R,
        theme: &Theme,
        _style: &renderer::Style,
        layout: Layout<'_>,
        cursor: mouse::Cursor,
        viewport: &Rectangle,
    ) {
        let bounds = layout.bounds();
        // COSMIC runs on Linux, where the native slider exposes Grabbing while
        // dragging. This remains true after the pointer leaves the hit area.
        let dragging = self.mouse_interaction(tree, layout, cursor, viewport, renderer)
            == mouse::Interaction::Grabbing;
        // A captured drag can prevent the surrounding mouse area from updating
        // its hover hint. Real cursor coordinates take precedence; a surface
        // leave also invalidates that hint until the pointer returns.
        let hovered = if cursor.position().is_some() {
            cursor.is_over(bounds)
        } else {
            self.hovered && !tree.state.downcast_ref::<VisualState>().pointer_left
        };
        let active = dragging || hovered;
        let height = if active {
            ACTIVE_TRACK_HEIGHT
        } else {
            track::HEIGHT
        };
        let rail = Rectangle {
            y: bounds.center_y() - height / 2.0,
            height,
            ..bounds
        };
        track::draw(
            renderer,
            rail,
            self.current.get(),
            gauge::progress_bar_style(theme),
        );
        if active {
            let status = if dragging {
                Status::Dragged
            } else {
                Status::Hovered
            };
            let handle = theme
                .style(&cosmic::theme::style::iced::Slider::Standard, status)
                .handle;
            draw_handle(renderer, bounds, self.current.get(), handle);
        }
    }

    fn overlay<'a>(
        &'a mut self,
        tree: &'a mut Tree,
        layout: Layout<'a>,
        renderer: &R,
        viewport: &Rectangle,
        translation: Vector,
    ) -> Option<overlay::Element<'a, Message, Theme, R>> {
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

    // The native ID belongs to the child tree. Keeping this structural wrapper
    // anonymous avoids duplicate IDs while retaining native accessibility nodes.

    fn drag_destinations(
        &self,
        state: &Tree,
        layout: Layout<'_>,
        renderer: &R,
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

fn draw_handle(
    renderer: &mut impl advanced::Renderer,
    bounds: Rectangle,
    progress: f32,
    handle: cosmic::iced::widget::slider::Handle,
) {
    let (width, height, radius) = match handle.shape {
        HandleShape::Circle { radius } => (radius * 2.0, radius * 2.0, radius.into()),
        HandleShape::Rectangle {
            width,
            height,
            border_radius,
        } => (f32::from(width), f32::from(height), border_radius),
    };
    let width = width.min(bounds.width).max(0.0);
    if width <= 0.0 || height <= 0.0 {
        return;
    }
    // The native input maps the full rail width to 0..1. Keep the thumb on that
    // same coordinate, clamping only at the ends to keep it inside the card.
    let center = (bounds.x + bounds.width * progress).clamp(
        bounds.x + width / 2.0,
        bounds.x + bounds.width - width / 2.0,
    );
    renderer.fill_quad(
        renderer::Quad {
            bounds: Rectangle {
                x: center - width / 2.0,
                y: bounds.center_y() - height / 2.0,
                width,
                height,
            },
            border: Border {
                radius,
                width: handle.border_width,
                color: handle.border_color,
            },
            snap: true,
            ..Default::default()
        },
        handle.background,
    );
}

#[cfg(test)]
mod tests {
    use super::*;
    use cosmic::iced::{Background, Point, Transformation};

    #[derive(Default)]
    struct RecordingRenderer {
        quads: Vec<renderer::Quad>,
    }

    impl advanced::Renderer for RecordingRenderer {
        fn start_layer(&mut self, _: Rectangle) {}
        fn end_layer(&mut self) {}
        fn start_transformation(&mut self, _: Transformation) {}
        fn end_transformation(&mut self) {}
        fn fill_quad(&mut self, quad: renderer::Quad, _: impl Into<Background>) {
            self.quads.push(quad);
        }
        fn reset(&mut self, _: Rectangle) {}
        fn allocate_image(
            &mut self,
            _: &cosmic::iced::advanced::image::Handle,
            _: impl FnOnce(
                Result<
                    cosmic::iced::advanced::image::Allocation,
                    cosmic::iced::advanced::image::Error,
                >,
            ) + Send
            + 'static,
        ) {
        }
    }

    #[derive(Debug, Clone, PartialEq)]
    enum InputMessage {
        Changed(f64),
        Released,
    }

    #[test]
    fn native_click_drag_outside_and_release_are_preserved() {
        let mut slider =
            build::<_, RecordingRenderer>(0.5, true, InputMessage::Changed, InputMessage::Released);
        let mut tree = Tree::new(&slider as &dyn Widget<InputMessage, Theme, RecordingRenderer>);
        let mut renderer = RecordingRenderer::default();
        let theme = Theme::dark();
        let node = slider
            .layout(
                &mut tree,
                &renderer,
                &layout::Limits::new(Size::ZERO, Size::new(200.0, HIT_HEIGHT)),
            )
            .move_to(Point::new(10.0, 20.0));
        let viewport = Rectangle::with_size(Size::new(300.0, 100.0));
        let mut messages = Vec::new();
        for (event, point, expected, dragging) in [
            (
                Event::Mouse(mouse::Event::ButtonPressed(mouse::Button::Left)),
                Point::new(60.0, 30.0),
                0.25,
                true,
            ),
            (
                Event::Mouse(mouse::Event::CursorMoved {
                    position: Point::new(260.0, 30.0),
                }),
                Point::new(260.0, 30.0),
                1.0,
                true,
            ),
            (
                Event::Mouse(mouse::Event::ButtonReleased(mouse::Button::Left)),
                Point::new(260.0, 30.0),
                1.0,
                false,
            ),
        ] {
            let cursor = mouse::Cursor::Available(point);
            slider.update(
                &mut tree,
                &event,
                Layout::new(&node),
                cursor,
                &renderer,
                &mut cosmic::iced::advanced::clipboard::Null,
                &mut Shell::new(&mut messages),
                &viewport,
            );
            assert_eq!(slider.current.get(), expected);
            assert_eq!(
                slider.mouse_interaction(&tree, Layout::new(&node), cursor, &viewport, &renderer)
                    == mouse::Interaction::Grabbing,
                dragging
            );
            renderer.quads.clear();
            slider.draw(
                &tree,
                &mut renderer,
                &theme,
                &renderer::Style::default(),
                Layout::new(&node),
                cursor,
                &viewport,
            );
            assert_eq!(renderer.quads.len(), if dragging { 3 } else { 2 });
            assert_eq!(
                renderer.quads[0].bounds.height,
                if dragging {
                    ACTIVE_TRACK_HEIGHT
                } else {
                    track::HEIGHT
                }
            );
            assert_eq!(renderer.quads[1].bounds.width, 200.0 * expected);
            assert!(renderer.quads.iter().all(|quad| quad.snap));
        }
        assert_eq!(
            messages,
            vec![
                InputMessage::Changed(0.25),
                InputMessage::Changed(1.0),
                InputMessage::Released
            ]
        );
        assert_eq!(node.size().height, HIT_HEIGHT);
    }

    #[test]
    fn leaving_surface_hides_stale_hover_hint_across_view_rebuilds() {
        let mut slider =
            build::<_, RecordingRenderer>(0.5, true, InputMessage::Changed, InputMessage::Released);
        let mut tree = Tree::new(&slider as &dyn Widget<InputMessage, Theme, RecordingRenderer>);
        let mut renderer = RecordingRenderer::default();
        let theme = Theme::dark();
        let node = slider.layout(
            &mut tree,
            &renderer,
            &layout::Limits::new(Size::ZERO, Size::new(200.0, HIT_HEIGHT)),
        );
        let viewport = Rectangle::with_size(Size::new(300.0, 100.0));
        // Forced previews with no cursor still show the requested hover state.
        slider.draw(
            &tree,
            &mut renderer,
            &theme,
            &renderer::Style::default(),
            Layout::new(&node),
            mouse::Cursor::Unavailable,
            &viewport,
        );
        assert_eq!(renderer.quads.len(), 3);
        assert_eq!(renderer.quads[0].bounds.height, ACTIVE_TRACK_HEIGHT);
        let native_id = tree.children[0].id.clone();
        slider.update(
            &mut tree,
            &Event::Mouse(mouse::Event::CursorLeft),
            Layout::new(&node),
            mouse::Cursor::Unavailable,
            &renderer,
            &mut cosmic::iced::advanced::clipboard::Null,
            &mut Shell::new(&mut Vec::new()),
            &viewport,
        );
        let mut rebuilt =
            build::<_, RecordingRenderer>(0.5, true, InputMessage::Changed, InputMessage::Released);
        tree.diff(&mut rebuilt as &mut dyn Widget<InputMessage, Theme, RecordingRenderer>);
        assert_eq!(tree.children[0].id, native_id);
        renderer.quads.clear();
        rebuilt.draw(
            &tree,
            &mut renderer,
            &theme,
            &renderer::Style::default(),
            Layout::new(&node),
            mouse::Cursor::Unavailable,
            &viewport,
        );
        assert_eq!(renderer.quads.len(), 2);
        assert_eq!(renderer.quads[0].bounds.height, track::HEIGHT);
        assert_eq!(node.size().height, HIT_HEIGHT);
    }
}
