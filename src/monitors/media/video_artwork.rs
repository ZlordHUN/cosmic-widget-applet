// SPDX-License-Identifier: MPL-2.0

//! Bounded, asynchronous thumbnails for local videos without controlling playback.
//!
//! FFmpeg is optional at runtime. A missing executable or unsupported video leaves
//! the existing artwork fallback in place. The caller caches successful results.

use super::{AlbumArt, MediaMonitor, video_title};
use std::path::Path;
use std::process::Stdio;
use std::time::Duration;
use tokio::io::{AsyncRead, AsyncReadExt};
use tokio::process::Command;

const EXTRACTION_TIMEOUT: Duration = Duration::from_secs(12);
const MAX_FRAME_BYTES: usize = 2 * 1024 * 1024;
const MAX_FRAME_DIMENSION: u32 = 640;

/// Extract a single real video frame from a regular local video file.
///
/// The 10-second sample skips most initial black frames. Very short clips fall
/// back to their first frame. Both attempts share a deadline, and cancellation
/// kills the child process. No media bytes are loaded into the widget except
/// the bounded thumbnail; playback and the user's position are untouched.
pub(super) async fn load(media_url: &str) -> Option<AlbumArt> {
    let path = video_title::local_video_path(media_url)?;
    let extract = async {
        let metadata = tokio::fs::metadata(&path).await.ok()?;
        if !metadata.is_file() || metadata.len() == 0 {
            return None;
        }

        if let Some(artwork) = extract_frame(&path, "10").await {
            return Some(artwork);
        }
        extract_frame(&path, "0").await
    };

    match tokio::time::timeout(EXTRACTION_TIMEOUT, extract).await {
        Ok(artwork) => artwork,
        Err(_) => {
            log::debug!("Local video thumbnail extraction timed out");
            None
        }
    }
}

async fn extract_frame(path: &Path, position: &str) -> Option<AlbumArt> {
    let mut child = Command::new("ffmpeg")
        .args([
            "-hide_banner",
            "-loglevel",
            "error",
            "-nostdin",
            "-threads",
            "1",
            "-filter_threads",
            "1",
            // Local containers must never initiate network requests.
            "-protocol_whitelist",
            "file,pipe",
            "-probesize",
            "8388608",
            "-analyzeduration",
            "5000000",
            // Input seeking avoids decoding the entire first ten seconds.
            "-ss",
            position,
            "-i",
        ])
        .arg(path)
        .args([
            // Uppercase V excludes attached album covers and thumbnails.
            "-map",
            "0:V:0",
            "-an",
            "-sn",
            "-dn",
            "-frames:v",
            "1",
            "-vf",
            // Fit the display aspect ratio, including anamorphic pixels, into
            // the bounding box before converting to square-pixel artwork.
            "scale=w='if(gte(dar,1),640,640*dar)':h='if(gte(dar,1),640/dar,640)',setsar=1",
            "-c:v",
            "png",
            "-pix_fmt",
            "rgb24",
            "-threads",
            "1",
            "-f",
            "image2pipe",
            "pipe:1",
        ])
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .kill_on_drop(true)
        .spawn()
        .ok()?;
    let stdout = child.stdout.take()?;
    let Some(bytes) = read_frame(stdout).await else {
        // Stop oversized/corrupt output immediately, without waiting for EOF.
        let _ = child.kill().await;
        return None;
    };
    if !child.wait().await.ok()?.success() {
        return None;
    }

    let artwork = MediaMonitor::decode_artwork(bytes)?;
    (artwork.source_width <= MAX_FRAME_DIMENSION && artwork.source_height <= MAX_FRAME_DIMENSION)
        .then_some(artwork)
}

async fn read_frame(reader: impl AsyncRead + Unpin) -> Option<Vec<u8>> {
    let mut bytes = Vec::new();
    reader
        .take(MAX_FRAME_BYTES as u64 + 1)
        .read_to_end(&mut bytes)
        .await
        .ok()?;
    (!bytes.is_empty() && bytes.len() <= MAX_FRAME_BYTES).then_some(bytes)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicU64, Ordering};

    #[tokio::test]
    async fn rejects_remote_audio_and_missing_sources() {
        for url in [
            "https://example.test/episode.mkv",
            "file://remote/share/episode.mkv",
            "file:///tmp/song.mp3",
            "file:///tmp/nonexistent-cosmic-thumbnail-test.mkv",
        ] {
            assert!(load(url).await.is_none(), "unexpected artwork for {url}");
        }
    }

    #[tokio::test]
    async fn thumbnail_output_is_bounded_even_without_eof() {
        // repeat() never reaches EOF; the byte cap must terminate this read.
        assert!(read_frame(tokio::io::repeat(1)).await.is_none());
        assert!(read_frame(tokio::io::empty()).await.is_none());
        assert_eq!(read_frame(&b"png bytes"[..]).await.unwrap(), b"png bytes");
    }

    struct Fixture(std::path::PathBuf);

    impl Drop for Fixture {
        fn drop(&mut self) {
            let _ = std::fs::remove_file(&self.0);
        }
    }

    async fn generated_video(size: &str, sample_aspect_ratio: &str) -> Fixture {
        static NEXT_FIXTURE: AtomicU64 = AtomicU64::new(0);
        let path = std::env::temp_dir().join(format!(
            "cosmic-thumbnail-{}-{}-'$(not-a-command)'.mkv",
            std::process::id(),
            NEXT_FIXTURE.fetch_add(1, Ordering::Relaxed)
        ));
        let fixture = Fixture(path);
        let source = format!("color=c=red:size={size}:rate=1,setsar={sample_aspect_ratio}");
        let mut child = Command::new("ffmpeg")
            .args([
                "-hide_banner",
                "-loglevel",
                "error",
                "-nostdin",
                "-f",
                "lavfi",
                "-i",
                &source,
                "-t",
                "2",
                "-c:v",
                "ffv1",
                "-threads",
                "1",
                "-n",
            ])
            .arg(&fixture.0)
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .kill_on_drop(true)
            .spawn()
            .expect("ffmpeg must be installed for this ignored integration test");
        let status = tokio::time::timeout(EXTRACTION_TIMEOUT, child.wait())
            .await
            .expect("fixture generation timed out")
            .expect("fixture generation failed");
        assert!(status.success());
        fixture
    }

    #[tokio::test]
    #[ignore = "requires ffmpeg installed; generates and removes temporary video fixtures"]
    async fn preserves_anamorphic_and_portrait_display_aspect_ratios() {
        for (size, sample_aspect_ratio, expected) in [
            // DVD-style non-square pixels: 720:480 * 8:9 is a 4:3 display.
            ("720x480", "8/9", (640, 480)),
            // Tall videos must remain within the same 640-pixel height limit.
            ("480x720", "1/1", (426, 640)),
        ] {
            let fixture = generated_video(size, sample_aspect_ratio).await;
            let url = reqwest::Url::from_file_path(&fixture.0).unwrap();
            let artwork = load(url.as_str()).await.expect("missing video thumbnail");
            assert_eq!(
                (artwork.source_width, artwork.source_height),
                expected,
                "incorrect thumbnail for {size} with SAR {sample_aspect_ratio}"
            );
        }
    }

    #[tokio::test]
    #[ignore = "requires ffmpeg installed; generates and removes a temporary video fixture"]
    async fn renders_short_video_without_shell_interpreting_its_filename() {
        let fixture = generated_video("800x400", "1/1").await;
        let url = reqwest::Url::from_file_path(&fixture.0).unwrap();
        // Ten seconds is past EOF, so load must retry the first frame.
        assert!(extract_frame(&fixture.0, "10").await.is_none());
        let artwork = load(url.as_str()).await.expect("missing video thumbnail");
        assert_eq!((artwork.source_width, artwork.source_height), (640, 320));
        let cosmic::iced::widget::image::Handle::Bytes(_, bytes) = &artwork.iced_handle else {
            panic!("video thumbnail must retain its encoded frame");
        };
        let frame = image::load_from_memory(bytes).unwrap().to_rgb8();
        let [red, green, blue] = frame.get_pixel(320, 160).0;
        assert!(
            red > 240 && green < 15 && blue < 15,
            "expected the red video frame"
        );
    }

    #[tokio::test]
    #[ignore = "set COSMIC_WIDGET_TEST_VIDEO to a real local video path; requires ffmpeg"]
    async fn loads_actual_local_video_without_seeking_the_player() {
        let path = std::env::var_os("COSMIC_WIDGET_TEST_VIDEO")
            .expect("set COSMIC_WIDGET_TEST_VIDEO to a local video path");
        let url = reqwest::Url::from_file_path(path).expect("video path must be absolute");
        let artwork = load(url.as_str()).await.expect("missing video thumbnail");
        assert!(artwork.source_width > 0 && artwork.source_width <= MAX_FRAME_DIMENSION);
        assert!(artwork.source_height > 0 && artwork.source_height <= MAX_FRAME_DIMENSION);
        assert!(artwork.encoded_bytes <= MAX_FRAME_BYTES);
    }
}
