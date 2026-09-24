// SPDX-License-Identifier: MPL-2.0

use cosmic::iced::advanced::widget::tree::{self, Tree};
use cosmic::iced::advanced::{self, Layout, Widget, layout, renderer};
use cosmic::iced::{Length, Rectangle, Size, Transformation, mouse};
use cosmic::{Element, Renderer, Theme};

pub fn out<'a, Message: 'a>(content: Element<'a, Message>, progress: f32) -> Element<'a, Message> {
    Element::new(ShrinkOut {
        content,
        scale: scale_from_progress(progress),
    })
}

struct ShrinkOut<'a, Message> {
    content: Element<'a, Message>,
    scale: f32,
}

impl<Message> Widget<Message, Theme, Renderer> for ShrinkOut<'_, Message> {
    fn tag(&self) -> tree::Tag {
        self.content.as_widget().tag()
    }

    fn state(&self) -> tree::State {
        self.content.as_widget().state()
    }

    fn children(&self) -> Vec<Tree> {
        self.content.as_widget().children()
    }

    fn diff(&mut self, tree: &mut Tree) {
        self.content.as_widget_mut().diff(tree);
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
        self.content.as_widget_mut().layout(tree, renderer, limits)
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

        if self.scale <= f32::EPSILON {
            return;
        }

        let bounds = layout.bounds();
        let anchor_x = bounds.x + bounds.width;
        let anchor_y = bounds.y + bounds.height / 2.0;
        let transformation = Transformation::translate(anchor_x, anchor_y)
            * Transformation::scale(self.scale)
            * Transformation::translate(-anchor_x, -anchor_y);
        let inverse = transformation.inverse();

        renderer.with_layer(bounds, |renderer| {
            renderer.with_transformation(transformation, |renderer| {
                self.content.as_widget().draw(
                    tree,
                    renderer,
                    theme,
                    style,
                    layout,
                    cursor * inverse,
                    &(*viewport * inverse),
                );
            });
        });
    }
}

fn scale_from_progress(progress: f32) -> f32 {
    1.0 - progress.clamp(0.0, 1.0)
}

#[cfg(test)]
mod tests {
    use super::scale_from_progress;

    #[test]
    fn shrink_reaches_zero_without_overshooting() {
        assert_eq!(scale_from_progress(-1.0), 1.0);
        assert_eq!(scale_from_progress(0.0), 1.0);
        assert_eq!(scale_from_progress(0.5), 0.5);
        assert_eq!(scale_from_progress(1.0), 0.0);
        assert_eq!(scale_from_progress(2.0), 0.0);
    }
}
