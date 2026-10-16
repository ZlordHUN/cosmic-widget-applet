// SPDX-License-Identifier: MPL-2.0

//! Remove transparent icon margins once, keeping a consistent visible size.

use cosmic::widget;
use image::{DynamicImage, ImageReader, RgbaImage};
use resvg::{tiny_skia, usvg};
use std::collections::HashMap;
use std::fs::File;
use std::io::{Cursor, Read};
use std::path::{Path, PathBuf};
use std::sync::{Mutex, OnceLock};

const RENDER_SIZE: u32 = super::SIZE as u32 * 4;
const MAX_FILE_BYTES: u64 = 4 * 1024 * 1024;
const MAX_IMAGE_DIMENSION: u32 = 2048;

pub(super) fn from_path(path: &Path) -> Option<widget::icon::Handle> {
    // Several conversation identities can share the same application icon.
    // Cache paths too, so each source asset is only decoded once.
    static CACHE: OnceLock<Mutex<HashMap<PathBuf, Option<widget::icon::Handle>>>> = OnceLock::new();
    let mut cache = CACHE.get_or_init(Mutex::default).lock().unwrap();
    if let Some(handle) = cache.get(path) {
        return handle.clone();
    }
    let handle = load(path);
    if cache.len() >= super::CACHE_LIMIT {
        cache.clear();
    }
    cache.insert(path.to_owned(), handle.clone());
    handle
}

fn load(path: &Path) -> Option<widget::icon::Handle> {
    let extension = path.extension()?.to_str()?.to_ascii_lowercase();
    if !matches!(extension.as_str(), "svg" | "png" | "jpg" | "jpeg") {
        return None;
    }
    let file = File::open(path).ok()?;
    let metadata = file.metadata().ok()?;
    if !metadata.is_file() || metadata.len() > MAX_FILE_BYTES {
        return None;
    }
    let mut bytes = Vec::new();
    file.take(MAX_FILE_BYTES + 1).read_to_end(&mut bytes).ok()?;
    if bytes.len() as u64 > MAX_FILE_BYTES {
        return None;
    }
    if extension == "svg" {
        let symbolic = path
            .file_stem()
            .and_then(|name| name.to_str())
            .is_some_and(|name| name.ends_with("-symbolic"));
        from_svg_bytes(&bytes, symbolic)
    } else {
        let image = normalize_raster(&bytes)?;
        Some(widget::icon::from_raster_pixels(
            image.width(),
            image.height(),
            image.into_raw(),
        ))
    }
}

pub(super) fn from_svg_bytes(bytes: &[u8], symbolic: bool) -> Option<widget::icon::Handle> {
    let mut handle = widget::icon::from_svg_bytes(normalize_svg(bytes)?.into_bytes());
    handle.symbolic = symbolic;
    Some(handle)
}

fn normalize_raster(bytes: &[u8]) -> Option<RgbaImage> {
    let mut reader = ImageReader::new(Cursor::new(bytes))
        .with_guessed_format()
        .ok()?;
    let mut limits = image::Limits::default();
    limits.max_image_width = Some(MAX_IMAGE_DIMENSION);
    limits.max_image_height = Some(MAX_IMAGE_DIMENSION);
    limits.max_alloc = Some(32 * 1024 * 1024);
    reader.limits(limits);
    let image = reader.decode().ok()?.to_rgba8();
    let (x, y, width, height) = alpha_bounds(&image)?;
    let cropped = image::imageops::crop_imm(&image, x, y, width, height).to_image();
    // Keep cached textures small while retaining detail at fractional/HiDPI scales.
    // `resize` preserves aspect ratio; non-square logos are never stretched.
    Some(
        DynamicImage::ImageRgba8(cropped)
            .resize(
                RENDER_SIZE,
                RENDER_SIZE,
                image::imageops::FilterType::Lanczos3,
            )
            .to_rgba8(),
    )
}

fn normalize_svg(bytes: &[u8]) -> Option<String> {
    let options = usvg::Options {
        image_href_resolver: usvg::ImageHrefResolver {
            // An application icon must be self-contained. Do not follow arbitrary
            // external paths while measuring a notification-supplied SVG.
            resolve_string: Box::new(|_, _| None),
            ..Default::default()
        },
        ..Default::default()
    };
    let tree = usvg::Tree::from_data(bytes, &options).ok()?;
    let scale = RENDER_SIZE as f32 / tree.size().width().max(tree.size().height());
    let width = (tree.size().width() * scale).ceil().max(1.0) as u32;
    let height = (tree.size().height() * scale).ceil().max(1.0) as u32;
    let mut pixmap = tiny_skia::Pixmap::new(width, height)?;
    resvg::render(
        &tree,
        tiny_skia::Transform::from_scale(scale, scale),
        &mut pixmap.as_mut(),
    );
    let image = RgbaImage::from_raw(width, height, pixmap.take())?;
    let (x, y, width, height) = alpha_bounds(&image)?;
    let (x, y, width, height) = (
        x as f32 / scale,
        y as f32 / scale,
        width as f32 / scale,
        height as f32 / scale,
    );
    // Keep SVGs as vectors so symbolic icons still receive the theme's tint.
    // Serializing first also resolves the source's units, transforms, and viewBox.
    let source = tree.to_string(&usvg::WriteOptions::default());
    Some(format!(
        "<svg xmlns=\"http://www.w3.org/2000/svg\" width=\"{width}\" height=\"{height}\" viewBox=\"{x} {y} {width} {height}\">{source}</svg>"
    ))
}

fn alpha_bounds(image: &RgbaImage) -> Option<(u32, u32, u32, u32)> {
    let (mut left, mut top) = image.dimensions();
    let (mut right, mut bottom) = (0, 0);
    for (x, y, pixel) in image.enumerate_pixels() {
        if pixel[3] != 0 {
            left = left.min(x);
            top = top.min(y);
            right = right.max(x + 1);
            bottom = bottom.max(y + 1);
        }
    }
    (right > left && bottom > top).then(|| (left, top, right - left, bottom - top))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn png(image: RgbaImage) -> Vec<u8> {
        let mut bytes = Cursor::new(Vec::new());
        image.write_to(&mut bytes, image::ImageFormat::Png).unwrap();
        bytes.into_inner()
    }

    #[test]
    fn transparent_margins_are_removed_without_stretching_rectangular_icons() {
        let mut image = RgbaImage::new(256, 256);
        for y in 80..180 {
            for x in 25..225 {
                image.put_pixel(x, y, image::Rgba([200, 50, 100, 255]));
            }
        }
        let normalized = normalize_raster(&png(image)).unwrap();
        assert_eq!(normalized.dimensions(), (80, 40));
        assert_eq!(alpha_bounds(&normalized), Some((0, 0, 80, 40)));
        assert_eq!(normalized.get_pixel(40, 20)[3], 255);
    }

    #[test]
    fn empty_and_oversized_rasters_are_rejected() {
        assert!(normalize_raster(&png(RgbaImage::new(20, 20))).is_none());
        assert!(normalize_raster(&png(RgbaImage::new(MAX_IMAGE_DIMENSION + 1, 1))).is_none());
        assert!(normalize_raster(b"not an image").is_none());
    }

    #[test]
    #[ignore = "requires the locally installed Cider icon"]
    fn normalizes_installed_cider_icon() {
        let bytes = std::fs::read("/usr/share/pixmaps/cider.png").unwrap();
        let original = image::load_from_memory(&bytes).unwrap().to_rgba8();
        let normalized = normalize_raster(&bytes).unwrap();
        println!(
            "Cider: {:?} bounds {:?} -> {:?} bounds {:?}",
            original.dimensions(),
            alpha_bounds(&original),
            normalized.dimensions(),
            alpha_bounds(&normalized)
        );
        assert_eq!(normalized.width().max(normalized.height()), RENDER_SIZE);
        let (_, _, width, height) = alpha_bounds(&normalized).unwrap();
        assert_eq!(width.max(height), RENDER_SIZE);
    }

    #[test]
    fn svg_crop_preserves_visible_artwork_and_aspect_ratio() {
        let source = br#"<svg xmlns="http://www.w3.org/2000/svg" width="100" height="100" viewBox="0 0 200 200"><rect x="40" y="60" width="120" height="80" fill="red"/></svg>"#;
        let svg = normalize_svg(source).unwrap();
        let tree = usvg::Tree::from_str(&svg, &usvg::Options::default()).unwrap();
        assert_eq!(tree.size().width(), 60.0);
        assert_eq!(tree.size().height(), 40.0);
        let mut pixels = tiny_skia::Pixmap::new(60, 40).unwrap();
        resvg::render(
            &tree,
            tiny_skia::Transform::identity(),
            &mut pixels.as_mut(),
        );
        let image = RgbaImage::from_raw(60, 40, pixels.take()).unwrap();
        assert_eq!(alpha_bounds(&image), Some((0, 0, 60, 40)));
        assert_eq!(*image.get_pixel(0, 0), image::Rgba([255, 0, 0, 255]));
        assert_eq!(*image.get_pixel(59, 39), image::Rgba([255, 0, 0, 255]));
    }
}
