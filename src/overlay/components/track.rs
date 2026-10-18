// SPDX-License-Identifier: MPL-2.0

//! Shared rail and fill drawing for indicators and the interactive media slider.

use cosmic::iced::advanced::{self, renderer};
use cosmic::iced::widget::progress_bar;
use cosmic::iced::{Border, Rectangle};

pub(in crate::overlay) const HEIGHT: f32 = 6.0;

pub(in crate::overlay) fn draw(
    renderer: &mut impl advanced::Renderer,
    bounds: Rectangle,
    progress: f32,
    mut style: progress_bar::Style,
) {
    if bounds.width <= 0.0 || bounds.height <= 0.0 {
        return;
    }
    // The media rail grows on hover, so derive its radius from the actual
    // height instead of carrying the idle-height radius into the larger rail.
    style.border.radius = (bounds.height / 2.0).into();
    renderer.fill_quad(
        renderer::Quad {
            bounds,
            border: style.border,
            snap: true,
            ..Default::default()
        },
        style.background,
    );
    let progress = if progress.is_finite() {
        progress.clamp(0.0, 1.0)
    } else {
        0.0
    };
    if progress > 0.0005 {
        let fill = Rectangle {
            width: bounds.width * progress,
            ..bounds
        };
        // Draw a rounded fill rectangle rather than clipping a full-width
        // track, which would leave the advancing end square.
        renderer.fill_quad(
            renderer::Quad {
                bounds: fill,
                border: Border {
                    radius: (fill.height.min(fill.width) / 2.0).into(),
                    ..Default::default()
                },
                snap: true,
                ..Default::default()
            },
            style.bar,
        );
    }
}
