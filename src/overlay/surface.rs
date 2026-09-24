// SPDX-License-Identifier: MPL-2.0

//! Wayland layer surface placement, rounded regions, and compositor styling.

use super::Message;
use crate::config::Config;
use cosmic::iced::platform_specific::runtime::wayland::{
    self, CornerRadius,
    layer_surface::{IcedMargin, SctkLayerSurfaceSettings},
};
use cosmic::iced::platform_specific::shell::commands::layer_surface::{
    self, Anchor, KeyboardInteractivity, Layer,
};
use cosmic::iced::platform_specific::shell::commands::{blur, corner_radius};
use cosmic::iced::{Point, Rectangle, Size, Task, window};

pub(super) const SURFACE_WIDTH: u32 = 370;

pub(super) fn create_overlay_surface(
    id: window::Id,
    config: &Config,
    height: u32,
    frosted: bool,
) -> Task<Message> {
    let create_surface = layer_surface::get_layer_surface(SctkLayerSurfaceSettings {
        id,
        layer: Layer::Bottom,
        keyboard_interactivity: KeyboardInteractivity::OnDemand,
        anchor: Anchor::TOP.union(Anchor::BOTTOM).union(Anchor::LEFT),
        namespace: "cosmic-widget-iced".to_string(),
        margin: IcedMargin {
            top: config.widget_y,
            left: config.widget_x,
            ..IcedMargin::default()
        },
        // An unspecified size becomes 1x1 for a surface anchored to only
        // one horizontal and vertical edge in the pinned Iced backend.
        size: Some((Some(SURFACE_WIDTH), None)),
        input_zone: Some(vec![surface_region(height)]),
        exclusive_zone: -1,
        ..SctkLayerSurfaceSettings::default()
    });

    if frosted {
        create_surface.chain(set_surface_blur(id, true, height))
    } else {
        create_surface
    }
}

pub(super) fn dragged_overlay_position(
    x: i32,
    y: i32,
    previous: Point,
    current: Point,
) -> (i32, i32) {
    let delta_x = (current.x - previous.x).round() as i32;
    let delta_y = (current.y - previous.y).round() as i32;
    (x.saturating_add(delta_x), y.saturating_add(delta_y))
}

pub(super) fn system_theme() -> cosmic::Theme {
    let mut theme = cosmic::theme::system_preference();
    theme.transparent = theme.cosmic().frosted_system_interface;
    theme
}

pub(super) fn frosted_enabled() -> bool {
    cosmic::theme::system_preference()
        .cosmic()
        .frosted_system_interface
}

pub(super) fn overlay_corners() -> CornerRadius {
    let radius = cosmic::theme::system_preference()
        .cosmic()
        .corner_radii
        .radius_l;

    CornerRadius {
        top_left: radius[0].round() as u32,
        top_right: radius[1].round() as u32,
        bottom_right: radius[2].round() as u32,
        bottom_left: radius[3].round() as u32,
    }
}

pub(super) fn set_surface_corners(id: window::Id, corners: CornerRadius) -> Task<Message> {
    corner_radius::corner_radius(id, Some(corners)).discard()
}

pub(super) fn set_surface_blur(id: window::Id, enabled: bool, height: u32) -> Task<Message> {
    let region = enabled.then(|| rounded_surface_regions(height, overlay_corners()));

    blur::blur(id, region).discard()
}

pub(super) fn rounded_surface_regions(height: u32, corners: CornerRadius) -> Vec<Rectangle> {
    let max_radius = SURFACE_WIDTH.min(height) / 2;
    let top_left = corners.top_left.min(max_radius);
    let top_right = corners.top_right.min(max_radius);
    let bottom_left = corners.bottom_left.min(max_radius);
    let bottom_right = corners.bottom_right.min(max_radius);
    let top_rows = top_left.max(top_right);
    let bottom_rows = bottom_left.max(bottom_right);
    let mut regions = Vec::with_capacity((top_rows + bottom_rows + 1) as usize);

    for row in 0..top_rows {
        regions.push(rounded_region_row(
            row,
            corner_inset(top_left, row),
            corner_inset(top_right, row),
        ));
    }

    let middle_height = height.saturating_sub(top_rows + bottom_rows);
    if middle_height > 0 {
        regions.push(Rectangle::new(
            Point::new(0.0, top_rows as f32),
            Size::new(SURFACE_WIDTH as f32, middle_height as f32),
        ));
    }

    for row in 0..bottom_rows {
        regions.push(rounded_region_row(
            height - row - 1,
            corner_inset(bottom_left, row),
            corner_inset(bottom_right, row),
        ));
    }

    regions
}

fn rounded_region_row(y: u32, left_inset: u32, right_inset: u32) -> Rectangle {
    Rectangle::new(
        Point::new(left_inset as f32, y as f32),
        Size::new(
            SURFACE_WIDTH.saturating_sub(left_inset + right_inset) as f32,
            1.0,
        ),
    )
}

fn corner_inset(radius: u32, row_from_edge: u32) -> u32 {
    if radius == 0 || row_from_edge >= radius {
        return 0;
    }

    let radius = radius as f32;
    let distance = radius - (row_from_edge as f32 + 0.5);
    (radius - (radius * radius - distance * distance).sqrt()).ceil() as u32
}

pub(super) fn set_surface_regions(id: window::Id, height: u32, frosted: bool) -> Task<Message> {
    Task::batch([
        set_surface_input_zone(id, height),
        set_surface_blur(id, frosted, height),
    ])
}

fn surface_region(height: u32) -> Rectangle {
    Rectangle::new(
        Point::ORIGIN,
        Size::new(SURFACE_WIDTH as f32, height as f32),
    )
}

fn set_surface_input_zone(id: window::Id, height: u32) -> Task<Message> {
    cosmic::iced::runtime::task::effect(cosmic::iced::runtime::Action::PlatformSpecific(
        cosmic::iced::runtime::platform_specific::Action::Wayland(wayland::Action::LayerSurface(
            wayland::layer_surface::Action::InputZone {
                id,
                zone: Some(vec![surface_region(height)]),
            },
        )),
    ))
}
