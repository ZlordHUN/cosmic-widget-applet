// SPDX-License-Identifier: MPL-2.0

//! Apply the selector's glass surface to the native subtitle menu.

use cosmic::Theme;
use cosmic::iced::overlay::menu;
use cosmic::iced::theme::{self, Base};
use cosmic::iced::widget::{container, pick_list, scrollable};
use cosmic::iced::{Background, Border, Color, Radians, gradient};

pub(super) struct SubtitleTheme(pub Theme);

pub(super) fn card_style(theme: &Theme) -> container::Style {
    let foreground: Color = theme.current_container().on.into();
    let glass = gradient::Linear::new(Radians(std::f32::consts::FRAC_PI_2))
        .add_stop(
            0.0,
            Color {
                a: 0.10,
                ..foreground
            },
        )
        .add_stop(
            1.0,
            Color {
                a: 0.06,
                ..foreground
            },
        );
    container::Style {
        background: Some(Background::Gradient(glass.into())),
        border: Border {
            color: Color {
                a: 0.14,
                ..foreground
            },
            width: 1.0,
            radius: 8.0.into(),
        },
        ..Default::default()
    }
}

impl Base for SubtitleTheme {
    fn default(preference: theme::Mode) -> Self {
        Self(<Theme as Base>::default(preference))
    }

    fn mode(&self) -> theme::Mode {
        self.0.mode()
    }

    fn base(&self) -> theme::Style {
        self.0.base()
    }

    fn palette(&self) -> Option<theme::Palette> {
        self.0.palette()
    }

    fn name(&self) -> &str {
        self.0.name()
    }
}

impl scrollable::Catalog for SubtitleTheme {
    type Class<'a> = <Theme as scrollable::Catalog>::Class<'a>;

    fn default<'a>() -> Self::Class<'a> {
        <Theme as scrollable::Catalog>::default()
    }

    fn style(&self, class: &Self::Class<'_>, status: scrollable::Status) -> scrollable::Style {
        <Theme as scrollable::Catalog>::style(&self.0, class, status)
    }
}

impl menu::Catalog for SubtitleTheme {
    type Class<'a> = ();

    fn default<'a>() -> <Self as menu::Catalog>::Class<'a> {}

    fn style(&self, class: &<Self as menu::Catalog>::Class<'_>) -> menu::Style {
        let native = <Theme as menu::Catalog>::style(&self.0, class);
        let card = card_style(&self.0);
        menu::Style {
            background: card.background.unwrap_or(native.background),
            border: card.border,
            ..native
        }
    }
}

impl pick_list::Catalog for SubtitleTheme {
    type Class<'a> = ();

    fn default<'a>() -> <Self as pick_list::Catalog>::Class<'a> {}

    fn style(
        &self,
        class: &<Self as pick_list::Catalog>::Class<'_>,
        status: pick_list::Status,
    ) -> pick_list::Style {
        <Theme as pick_list::Catalog>::style(&self.0, class, status)
    }
}
