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

impl<Message> Widget<Message, Theme, Renderer> for SlideLeft<'_, Message> {
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

        let bounds = layout.bounds();
        let Some(clip) = bounds.intersection(viewport) else {
            return;
        };
        let translation = Vector::new(left_offset(bounds.width, self.progress), 0.0);

        renderer.with_layer(clip, |renderer| {
            renderer.with_translation(translation, |renderer| {
                self.content.as_widget().draw(
                    tree,
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
    use super::left_offset;

    #[test]
    fn slide_moves_from_rest_to_one_full_width_left() {
        assert_eq!(left_offset(320.0, 0.0), 0.0);
        assert_eq!(left_offset(320.0, 0.5), -160.0);
        assert_eq!(left_offset(320.0, 1.0), -320.0);
    }
}
