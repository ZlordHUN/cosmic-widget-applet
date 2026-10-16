// SPDX-License-Identifier: MPL-2.0

//! Rounded, center-cropped media thumbnails prepared once per artwork handle.

use crate::monitors::media::AlbumArt;
use crate::overlay::Message;
use cosmic::iced::core::image::{FilterMethod, Handle, Id};
use cosmic::iced::{ContentFit, Length};
use cosmic::{Element, widget};
use image::{DynamicImage, RgbaImage, imageops::FilterType};
use std::collections::VecDeque;
use std::sync::{Mutex, OnceLock};

const CORNER_RADIUS: f32 = 8.0;
const RASTER_SCALE: f32 = 3.0;
const CACHE_LIMIT: usize = 20;

#[derive(Clone, Copy, PartialEq, Eq)]
struct Key {
    source: Id,
    width: u32,
    height: u32,
}

#[derive(Default)]
struct Cache(VecDeque<(Key, Handle)>);

impl Cache {
    fn thumbnail(&mut self, source: &Handle, width: u32, height: u32) -> Handle {
        let key = Key {
            source: source.id(),
            width,
            height,
        };
        if let Some(index) = self.0.iter().position(|(cached, _)| *cached == key) {
            let entry = self.0.remove(index).unwrap();
            let handle = entry.1.clone();
            self.0.push_back(entry);
            return handle;
        }

        let handle = decode(source)
            .map(|image| {
                let pixels = rounded_thumbnail(image, width, height);
                Handle::from_rgba(width, height, pixels.into_raw())
            })
            // Retain the renderer's usual fallback behavior for invalid art,
            // and cache it too so a bad image is not decoded on every redraw.
            .unwrap_or_else(|| source.clone());
        if self.0.len() >= CACHE_LIMIT {
            self.0.pop_front();
        }
        self.0.push_back((key, handle.clone()));
        handle
    }
}

pub(in crate::overlay) fn view(
    art: &AlbumArt,
    width: f32,
    height: f32,
) -> Element<'static, Message> {
    static CACHE: OnceLock<Mutex<Cache>> = OnceLock::new();
    let raster_width = (width * RASTER_SCALE).round().max(1.0) as u32;
    let raster_height = (height * RASTER_SCALE).round().max(1.0) as u32;
    let handle = CACHE.get_or_init(Mutex::default).lock().unwrap().thumbnail(
        &art.iced_handle,
        raster_width,
        raster_height,
    );

    // Crop before rounding: Iced's Cover rendering can put the image's rounded
    // corners outside the visible bounds when the source aspect ratio differs.
    widget::image(handle)
        .width(Length::Fixed(width))
        .height(Length::Fixed(height))
        .content_fit(ContentFit::Cover)
        .filter_method(FilterMethod::Linear)
        .into()
}

fn decode(handle: &Handle) -> Option<DynamicImage> {
    match handle {
        Handle::Bytes(_, bytes) => image::load_from_memory(bytes.as_ref()).ok(),
        Handle::Path(_, path) => image::open(path).ok(),
        Handle::Rgba {
            width,
            height,
            pixels,
            ..
        } => RgbaImage::from_raw(*width, *height, pixels.as_ref().to_vec())
            .map(DynamicImage::ImageRgba8),
    }
}

fn rounded_thumbnail(image: DynamicImage, width: u32, height: u32) -> RgbaImage {
    let mut pixels = image
        .resize_to_fill(width, height, FilterType::Lanczos3)
        .into_rgba8();
    let radius = (CORNER_RADIUS * RASTER_SCALE).min(width.min(height) as f32 / 2.0);
    for y in 0..radius.ceil() as u32 {
        for x in 0..radius.ceil() as u32 {
            let dx = radius - (x as f32 + 0.5);
            let dy = radius - (y as f32 + 0.5);
            let coverage = (radius + 0.5 - dx.hypot(dy)).clamp(0.0, 1.0);
            for (px, py) in [
                (x, y),
                (width - 1 - x, y),
                (x, height - 1 - y),
                (width - 1 - x, height - 1 - y),
            ] {
                let pixel = pixels.get_pixel_mut(px, py);
                pixel[3] = (f32::from(pixel[3]) * coverage).round() as u8;
            }
        }
    }
    pixels
}

#[cfg(test)]
mod tests {
    use super::*;
    use image::Rgba;

    #[test]
    fn cropped_art_has_rounded_antialiased_corners_and_opaque_edges() {
        let source = RgbaImage::from_pixel(300, 100, Rgba([40, 80, 120, 255]));
        let pixels = rounded_thumbnail(DynamicImage::ImageRgba8(source), 384, 216);
        assert_eq!(pixels.dimensions(), (384, 216));
        for (x, y) in [(0, 0), (383, 0), (0, 215), (383, 215)] {
            assert_eq!(pixels.get_pixel(x, y)[3], 0);
        }
        for (x, y) in [(192, 0), (0, 108), (383, 108), (192, 215), (192, 108)] {
            assert_eq!(pixels.get_pixel(x, y), &Rgba([40, 80, 120, 255]));
        }
        assert!(pixels.pixels().any(|pixel| (1..255).contains(&pixel[3])));
        for y in 0..24 {
            for x in 0..24 {
                let alpha = pixels.get_pixel(x, y)[3];
                assert_eq!(alpha, pixels.get_pixel(383 - x, y)[3]);
                assert_eq!(alpha, pixels.get_pixel(x, 215 - y)[3]);
                assert_eq!(alpha, pixels.get_pixel(383 - x, 215 - y)[3]);
            }
        }
    }

    #[test]
    fn cover_crop_keeps_the_center_for_both_source_orientations() {
        for (width, height) in [(300, 100), (100, 300)] {
            let source = RgbaImage::from_fn(width, height, |x, y| {
                let position = if width > height { x } else { y };
                match position {
                    0..100 => Rgba([255, 0, 0, 255]),
                    100..200 => Rgba([0, 255, 0, 255]),
                    _ => Rgba([0, 0, 255, 255]),
                }
            });
            let pixels = rounded_thumbnail(DynamicImage::ImageRgba8(source), 288, 288);
            for (x, y) in [(32, 32), (144, 144), (255, 255)] {
                assert_eq!(pixels.get_pixel(x, y), &Rgba([0, 255, 0, 255]));
            }
        }
    }

    #[test]
    fn rounding_preserves_source_transparency() {
        let source = RgbaImage::from_pixel(96, 96, Rgba([40, 80, 120, 100]));
        let pixels = rounded_thumbnail(DynamicImage::ImageRgba8(source), 288, 288);
        assert_eq!(pixels.get_pixel(144, 144)[3], 100);
        assert_eq!(pixels.get_pixel(0, 0)[3], 0);
        assert!(pixels.pixels().all(|pixel| pixel[3] <= 100));
    }

    #[test]
    fn cache_reuses_handles_for_redraws_and_bounds_retained_artwork() {
        let source = Handle::from_rgba(32, 32, vec![255; 32 * 32 * 4]);
        let mut cache = Cache::default();
        let first = cache.thumbnail(&source, 32, 32);
        assert_eq!(first.id(), cache.thumbnail(&source, 32, 32).id());
        assert_ne!(first.id(), cache.thumbnail(&source, 48, 32).id());
        for _ in 0..CACHE_LIMIT {
            let other = Handle::from_rgba(32, 32, vec![255; 32 * 32 * 4]);
            cache.thumbnail(&other, 32, 32);
            assert!(cache.0.len() <= CACHE_LIMIT);
        }
        assert_ne!(first.id(), cache.thumbnail(&source, 32, 32).id());
    }
}
