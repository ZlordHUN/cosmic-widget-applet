// SPDX-License-Identifier: MPL-2.0

//! Shared OS accent feedback while preserving each control's resting appearance.

use cosmic::{theme, widget::button::Catalog as _};

pub(in crate::overlay) fn accent_hover(base: fn() -> theme::Button) -> theme::Button {
    theme::Button::Custom {
        active: Box::new(move |focused, theme| theme.active(focused, false, &base())),
        disabled: Box::new(move |theme| theme.disabled(&base())),
        hovered: Box::new(move |focused, theme| {
            let mut style = theme.hovered(focused, false, &theme::Button::Suggested);
            style.border_radius = theme.active(focused, false, &base()).border_radius;
            style
        }),
        pressed: Box::new(move |focused, theme| {
            let mut style = theme.pressed(focused, false, &theme::Button::Suggested);
            style.border_radius = theme.active(focused, false, &base()).border_radius;
            style
        }),
    }
}
