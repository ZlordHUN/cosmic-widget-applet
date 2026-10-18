// SPDX-License-Identifier: MPL-2.0

//! Reveal content from the top without changing its layout or drawing scale.

use cosmic::Element;
use cosmic::iced::advanced::widget::Operation;
use cosmic::iced::advanced::widget::tree::{self, Tree};
use cosmic::iced::advanced::{self, Clipboard, Layout, Shell, Widget, layout, overlay, renderer};
use cosmic::iced::{Event, Length, Rectangle, Size, Vector, mouse, touch};

/// `0.0` covers the content completely; `1.0` reveals its full natural height.
pub fn vertical<'a, Message: 'a>(
    content: Element<'a, Message>,
    progress: f32,
) -> Element<'a, Message> {
    Element::new(Reveal {
        content,
        progress: progress.clamp(0.0, 1.0),
    })
}

struct Reveal<'a, Message, Theme, Renderer> {
    content: cosmic::iced::Element<'a, Message, Theme, Renderer>,
    progress: f32,
}

struct RevealState;

impl<Message, Theme, Renderer: advanced::Renderer> Widget<Message, Theme, Renderer>
    for Reveal<'_, Message, Theme, Renderer>
{
    fn tag(&self) -> tree::Tag {
        tree::Tag::of::<RevealState>()
    }

    fn children(&self) -> Vec<Tree> {
        vec![Tree::new(self.content.as_widget())]
    }

    fn diff(&mut self, tree: &mut Tree) {
        tree.diff_children(std::slice::from_mut(&mut self.content));
    }

    fn size(&self) -> Size<Length> {
        Size::new(self.content.as_widget().size().width, Length::Shrink)
    }

    fn layout(
        &mut self,
        tree: &mut Tree,
        renderer: &Renderer,
        limits: &layout::Limits,
    ) -> layout::Node {
        // A shrinking ancestor must never squeeze or vertically recenter the
        // content. Only the wrapper's reported height changes between frames.
        let child_limits = layout::Limits::with_compression(
            Size::new(limits.min().width, 0.0),
            Size::new(limits.max().width, f32::INFINITY),
            Size::new(limits.compression().width, true),
        );
        let child =
            self.content
                .as_widget_mut()
                .layout(&mut tree.children[0], renderer, &child_limits);
        let size = child.size();
        layout::Node::with_children(
            Size::new(size.width, size.height * self.progress),
            vec![child],
        )
    }

    fn operate(
        &mut self,
        tree: &mut Tree,
        layout: Layout<'_>,
        renderer: &Renderer,
        operation: &mut dyn Operation,
    ) {
        // Covered controls must not become keyboard-focus or operation targets.
        if self.progress >= 1.0 {
            self.content.as_widget_mut().operate(
                &mut tree.children[0],
                child_layout(layout),
                renderer,
                operation,
            );
        }
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
        let clip = visible_bounds(layout, viewport);
        // Window/redraw events keep animated children alive even while the
        // reveal is running. Interaction resumes when all content is visible.
        if !matches!(event, Event::Window(_)) {
            let Some(clip) = clip else {
                return;
            };
            if self.progress < 1.0 || !touch_is_visible(event, clip) {
                return;
            }
        }
        let clip = clip.unwrap_or(Rectangle::with_size(Size::ZERO));
        self.content.as_widget_mut().update(
            &mut tree.children[0],
            event,
            child_layout(layout),
            visible_cursor(cursor, clip),
            renderer,
            clipboard,
            shell,
            &clip,
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
        let Some(clip) = visible_bounds(layout, viewport) else {
            return mouse::Interaction::None;
        };
        if self.progress < 1.0 || !cursor.is_over(clip) {
            return mouse::Interaction::None;
        }
        self.content.as_widget().mouse_interaction(
            &tree.children[0],
            child_layout(layout),
            cursor,
            &clip,
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
        let Some(clip) = visible_bounds(layout, viewport) else {
            return;
        };
        renderer.with_layer(clip, |renderer| {
            self.content.as_widget().draw(
                &tree.children[0],
                renderer,
                theme,
                style,
                child_layout(layout),
                visible_cursor(cursor, clip),
                &clip,
            );
        });
    }

    fn overlay<'a>(
        &'a mut self,
        tree: &'a mut Tree,
        layout: Layout<'a>,
        renderer: &Renderer,
        viewport: &Rectangle,
        translation: Vector,
    ) -> Option<overlay::Element<'a, Message, Theme, Renderer>> {
        // Overlays render outside their parent layer, so never create one for
        // content that is still being uncovered or covered.
        if self.progress < 1.0 {
            return None;
        }
        let clip = visible_bounds(layout, viewport)?;
        self.content.as_widget_mut().overlay(
            &mut tree.children[0],
            child_layout(layout),
            renderer,
            &clip,
            translation,
        )
    }
}

fn child_layout(layout: Layout<'_>) -> Layout<'_> {
    layout.child(0).with_virtual_offset(layout.virtual_offset())
}

fn visible_bounds(layout: Layout<'_>, viewport: &Rectangle) -> Option<Rectangle> {
    layout
        .bounds()
        .intersection(viewport)
        .filter(|clip| clip.width > 0.0 && clip.height > 0.0)
}

fn visible_cursor(cursor: mouse::Cursor, clip: Rectangle) -> mouse::Cursor {
    if cursor.is_over(clip) {
        cursor
    } else {
        mouse::Cursor::Unavailable
    }
}

fn touch_is_visible(event: &Event, clip: Rectangle) -> bool {
    match event {
        Event::Touch(
            touch::Event::FingerPressed { position, .. }
            | touch::Event::FingerMoved { position, .. }
            | touch::Event::FingerLifted { position, .. }
            | touch::Event::FingerLost { position, .. },
        ) => clip.contains(*position),
        _ => true,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use cosmic::iced::{Background, Point, Transformation, window};
    use std::cell::RefCell;
    use std::rc::Rc;

    #[derive(Default)]
    struct RecordingRenderer {
        clips: Vec<Rectangle>,
        transformations: usize,
    }

    impl advanced::Renderer for RecordingRenderer {
        fn start_layer(&mut self, bounds: Rectangle) {
            self.clips.push(bounds);
        }
        fn end_layer(&mut self) {}
        fn start_transformation(&mut self, _: Transformation) {
            self.transformations += 1;
        }
        fn end_transformation(&mut self) {}
        fn fill_quad(&mut self, _: renderer::Quad, _: impl Into<Background>) {}
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

    #[derive(Default)]
    struct Observation {
        draws: Vec<(Rectangle, Rectangle, mouse::Cursor)>,
        updates: Vec<Event>,
    }

    struct Probe(Rc<RefCell<Observation>>);

    impl Widget<(), (), RecordingRenderer> for Probe {
        fn size(&self) -> Size<Length> {
            Size::new(Length::Fill, Length::Shrink)
        }
        fn layout(
            &mut self,
            _: &mut Tree,
            _: &RecordingRenderer,
            limits: &layout::Limits,
        ) -> layout::Node {
            layout::Node::new(limits.resolve(Length::Fill, 40.0, Size::ZERO))
        }
        fn draw(
            &self,
            _: &Tree,
            _: &mut RecordingRenderer,
            _: &(),
            _: &renderer::Style,
            layout: Layout<'_>,
            cursor: mouse::Cursor,
            viewport: &Rectangle,
        ) {
            self.0
                .borrow_mut()
                .draws
                .push((layout.bounds(), *viewport, cursor));
        }
        fn update(
            &mut self,
            _: &mut Tree,
            event: &Event,
            _: Layout<'_>,
            _: mouse::Cursor,
            _: &RecordingRenderer,
            _: &mut dyn Clipboard,
            shell: &mut Shell<'_, ()>,
            _: &Rectangle,
        ) {
            self.0.borrow_mut().updates.push(event.clone());
            if matches!(event, Event::Window(window::Event::RedrawRequested(_))) {
                shell.request_redraw();
            }
        }
    }

    fn reveal(
        progress: f32,
        observation: Rc<RefCell<Observation>>,
    ) -> Reveal<'static, (), (), RecordingRenderer> {
        Reveal {
            content: cosmic::iced::Element::new(Probe(observation)),
            progress,
        }
    }

    #[test]
    fn reveal_keeps_full_child_layout_at_every_progress_and_parent_height_constraint() {
        let renderer = RecordingRenderer::default();
        for (progress, expected_height) in [(0.0, 0.0), (0.5, 20.0), (1.0, 40.0)] {
            let mut widget = reveal(progress, Rc::default());
            let mut tree = Tree::new(&widget as &dyn Widget<(), (), RecordingRenderer>);
            // A parent may only have room for the currently revealed portion.
            let limits = layout::Limits::new(Size::ZERO, Size::new(200.0, expected_height));
            let node = widget.layout(&mut tree, &renderer, &limits);
            assert_eq!(node.size(), Size::new(200.0, expected_height));
            assert_eq!(
                node.children()[0].bounds(),
                Rectangle::with_size(Size::new(200.0, 40.0))
            );
        }
    }

    #[test]
    fn reveal_clips_drawing_without_transforming_or_exposing_hidden_cursor() {
        let observation = Rc::new(RefCell::new(Observation::default()));
        let mut widget = reveal(0.5, observation.clone());
        let mut tree = Tree::new(&widget as &dyn Widget<(), (), RecordingRenderer>);
        let mut renderer = RecordingRenderer::default();
        let node = widget
            .layout(
                &mut tree,
                &renderer,
                &layout::Limits::new(Size::ZERO, Size::new(200.0, 100.0)),
            )
            .move_to(Point::new(10.0, 20.0));
        let viewport = Rectangle {
            x: 0.0,
            y: 25.0,
            width: 150.0,
            height: 100.0,
        };
        let clip = Rectangle {
            x: 10.0,
            y: 25.0,
            width: 140.0,
            height: 15.0,
        };
        widget.draw(
            &tree,
            &mut renderer,
            &(),
            &renderer::Style::default(),
            Layout::new(&node),
            mouse::Cursor::Available(Point::new(20.0, 50.0)),
            &viewport,
        );
        assert_eq!(renderer.clips, vec![clip]);
        assert_eq!(renderer.transformations, 0);
        assert_eq!(
            observation.borrow().draws,
            vec![(
                Rectangle {
                    x: 10.0,
                    y: 20.0,
                    width: 200.0,
                    height: 40.0
                },
                clip,
                mouse::Cursor::Unavailable
            )]
        );

        widget.progress = 0.0;
        let hidden = widget.layout(
            &mut tree,
            &renderer,
            &layout::Limits::new(Size::ZERO, Size::new(200.0, 100.0)),
        );
        widget.draw(
            &tree,
            &mut renderer,
            &(),
            &renderer::Style::default(),
            Layout::new(&hidden),
            mouse::Cursor::Unavailable,
            &viewport,
        );
        assert_eq!(observation.borrow().draws.len(), 1);
    }

    #[test]
    fn partial_reveal_forwards_redraws_but_blocks_input() {
        let observation = Rc::new(RefCell::new(Observation::default()));
        let mut widget = reveal(0.5, observation.clone());
        let mut tree = Tree::new(&widget as &dyn Widget<(), (), RecordingRenderer>);
        let renderer = RecordingRenderer::default();
        let node = widget.layout(
            &mut tree,
            &renderer,
            &layout::Limits::new(Size::ZERO, Size::new(200.0, 100.0)),
        );
        let viewport = Rectangle::with_size(Size::new(200.0, 100.0));
        let redraw = Event::Window(window::Event::RedrawRequested(
            cosmic::iced::time::Instant::now(),
        ));
        let click = Event::Mouse(mouse::Event::ButtonPressed(mouse::Button::Left));
        let mut messages = Vec::new();
        let mut shell = Shell::new(&mut messages);
        for event in [&redraw, &click] {
            widget.update(
                &mut tree,
                event,
                Layout::new(&node),
                mouse::Cursor::Available(Point::new(10.0, 10.0)),
                &renderer,
                &mut cosmic::iced::advanced::clipboard::Null,
                &mut shell,
                &viewport,
            );
        }
        assert_eq!(observation.borrow().updates, vec![redraw]);
        assert_eq!(shell.redraw_request(), window::RedrawRequest::NextFrame);
    }
}
