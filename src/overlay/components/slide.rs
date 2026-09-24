// SPDX-License-Identifier: MPL-2.0

use cosmic::iced::advanced::widget::tree::{self, Tree};
use cosmic::iced::advanced::{self, Layout, Widget, layout, renderer};
use cosmic::iced::{Length, Rectangle, Size, Vector, mouse};
use cosmic::{Element, Renderer, Theme};

pub fn left<'a, Message: 'a>(content: Element<'a, Message>, progress: f32) -> Element<'a, Message> {
    Element::new(SlideLeft {
        content,
        progress: progress.clamp(0.0, 1.0),
    })
}

struct SlideLeft<'a, Message> {
    content: Element<'a, Message>,
    progress: f32,
}

// Exit cards ignore input. A distinct subtree prevents interrupted drag/press
// state from leaking into a live card when the transition ends or is cancelled.
struct SlideState;

impl<Message> Widget<Message, Theme, Renderer> for SlideLeft<'_, Message> {
    fn tag(&self) -> tree::Tag {
        tree::Tag::of::<SlideState>()
    }

    fn state(&self) -> tree::State {
        tree::State::new(SlideState)
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
        use advanced::Renderer as _;

        let bounds = layout.bounds();
        let Some(clip) = bounds.intersection(viewport) else {
            return;
        };
        let translation = Vector::new(left_offset(bounds.width, self.progress), 0.0);

        renderer.with_layer(clip, |renderer| {
            renderer.with_translation(translation, |renderer| {
                self.content.as_widget().draw(
                    &tree.children[0],
                    renderer,
                    theme,
                    style,
                    layout,
                    cursor,
                    &(*viewport - translation),
                );
            });
        });
    }
}

fn left_offset(width: f32, progress: f32) -> f32 {
    -width.max(0.0) * progress.clamp(0.0, 1.0)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn slide_moves_from_rest_to_one_full_width_left() {
        assert_eq!(left_offset(320.0, 0.0), 0.0);
        assert_eq!(left_offset(320.0, 0.5), -160.0);
        assert_eq!(left_offset(320.0, 1.0), -320.0);
    }

    #[derive(Default)]
    struct InputState {
        dragging: bool,
    }

    struct StatefulInput;

    impl Widget<(), Theme, Renderer> for StatefulInput {
        fn tag(&self) -> tree::Tag {
            tree::Tag::of::<InputState>()
        }
        fn state(&self) -> tree::State {
            tree::State::new(InputState::default())
        }
        fn size(&self) -> Size<Length> {
            Size::new(Length::Shrink, Length::Shrink)
        }
        fn layout(&mut self, _: &mut Tree, _: &Renderer, _: &layout::Limits) -> layout::Node {
            layout::Node::new(Size::new(100.0, 20.0))
        }
        fn draw(
            &self,
            _: &Tree,
            _: &mut Renderer,
            _: &Theme,
            _: &renderer::Style,
            _: Layout<'_>,
            _: mouse::Cursor,
            _: &Rectangle,
        ) {
        }
    }

    #[test]
    fn interrupted_drag_is_reset_when_entering_and_leaving_slide() {
        let mut input: Element<'_, ()> = Element::new(StatefulInput);
        let mut tree = Tree::new(input.as_widget());
        tree.state.downcast_mut::<InputState>().dragging = true;

        let mut outgoing = left(Element::new(StatefulInput), 0.0);
        tree.diff(outgoing.as_widget_mut());
        assert!(!tree.children[0].state.downcast_ref::<InputState>().dragging);

        // Even private input state inside the outgoing card must never be
        // inherited by a replacement source with an identical control layout.
        tree.children[0].state.downcast_mut::<InputState>().dragging = true;
        tree.diff(input.as_widget_mut());
        assert!(!tree.state.downcast_ref::<InputState>().dragging);
    }
}
