// SPDX-License-Identifier: MPL-2.0

//! Read only the two HTTP cache entries written by Desktop's usage poller.
//! Chromium's keys identify the organization, and its checksums detect partial
//! writes. No cookies, credentials, or other cached requests are accessed.

use super::super::{UsageSnapshot, UsageSource, UsageStatus, claude, http_cache};
use super::{FRESH_SECONDS, FileStamp, MAX_AGE_SECONDS, read_bounded};
#[cfg(test)]
use http_cache::{HEADER_SIZE, MAX_BODY_BYTES, decode_body};
use http_cache::{MAX_ENTRY_BYTES, cache_filename};
use std::path::{Path, PathBuf};

#[derive(Default)]
pub(super) struct UsageCacheReader {
    identity: Option<(PathBuf, String)>,
    entries: Vec<CachedEntry>,
}

struct CachedEntry {
    key: String,
    path: PathBuf,
    stamp: Option<FileStamp>,
    snapshot: Option<UsageSnapshot>,
}

impl UsageCacheReader {
    pub(super) fn snapshot(
        &mut self,
        directory: &Path,
        organization: &str,
        now: i64,
    ) -> Option<UsageSnapshot> {
        if !valid_organization(organization) {
            self.identity = None;
            self.entries.clear();
            return None;
        }
        if self
            .identity
            .as_ref()
            .is_none_or(|(path, org)| path != directory || org != organization)
        {
            self.entries = ["", "?skip_spend=1"]
                .into_iter()
                .map(|suffix| {
                    let key = usage_key(organization, suffix);
                    let path = directory
                        .join("Cache/Cache_Data")
                        .join(cache_filename(&key));
                    CachedEntry {
                        key,
                        path,
                        stamp: None,
                        snapshot: None,
                    }
                })
                .collect();
            self.identity = Some((directory.to_owned(), organization.to_owned()));
        }
        for entry in &mut self.entries {
            let stamp = FileStamp::read(&entry.path, MAX_ENTRY_BYTES);
            if stamp.is_none() {
                entry.stamp = None;
                entry.snapshot = None;
                continue;
            }
            if stamp != entry.stamp {
                entry.snapshot = read_bounded(&entry.path, MAX_ENTRY_BYTES)
                    .and_then(|bytes| parse_entry(&bytes, &entry.key, now));
                if FileStamp::read(&entry.path, MAX_ENTRY_BYTES) != stamp {
                    entry.stamp = None;
                    entry.snapshot = None;
                } else {
                    entry.stamp = stamp;
                }
            }
        }
        self.entries
            .iter()
            .filter_map(|entry| entry.snapshot.as_ref())
            .filter_map(|snapshot| present(snapshot.clone(), now))
            .max_by_key(|snapshot| snapshot.updated_at)
    }
}

fn valid_organization(organization: &str) -> bool {
    organization.len() == 36
        && organization.bytes().enumerate().all(|(index, byte)| {
            if matches!(index, 8 | 13 | 18 | 23) {
                byte == b'-'
            } else {
                byte.is_ascii_hexdigit()
            }
        })
}

fn usage_key(organization: &str, suffix: &str) -> String {
    format!("1/0/https://claude.ai/api/organizations/{organization}/usage{suffix}")
}

fn parse_entry(bytes: &[u8], key: &str, now: i64) -> Option<UsageSnapshot> {
    let response = http_cache::parse_entry(bytes, key, now, MAX_AGE_SECONDS)?;
    let mut snapshot = claude::parse_usage(&response.body, response.updated_at)?;
    // Both main windows must belong to this response; partial responses do not
    // borrow a percentage or a reset timestamp from a different report.
    if !["five_hour", "seven_day"]
        .iter()
        .all(|id| snapshot.windows.iter().any(|window| window.limit_id == *id))
    {
        return None;
    }
    snapshot.source = Some(UsageSource::ClaudeDesktop);
    present(snapshot, now)
}

fn present(mut snapshot: UsageSnapshot, now: i64) -> Option<UsageSnapshot> {
    let age = now.checked_sub(snapshot.updated_at?)?;
    if !(0..=MAX_AGE_SECONDS).contains(&age) {
        return None;
    }
    for window in &mut snapshot.windows {
        window.is_stale = age > FRESH_SECONDS || window.resets_at.is_some_and(|reset| reset <= now);
    }
    snapshot.status = if snapshot.windows.iter().any(|window| window.is_stale) {
        UsageStatus::Stale
    } else {
        UsageStatus::Available
    };
    Some(snapshot)
}

#[cfg(test)]
pub(super) fn write_test_report(
    directory: &Path,
    organization: &str,
    timestamp: i64,
    used: [f64; 2],
    resets: [i64; 2],
) {
    let key = usage_key(organization, "");
    let path = directory
        .join("Cache/Cache_Data")
        .join(cache_filename(&key));
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    let body = tests::usage_body(used, resets);
    std::fs::write(
        path,
        http_cache::test_entry(&key, timestamp, "identity", &body),
    )
    .unwrap();
}

#[cfg(test)]
mod tests {
    use super::*;
    use http_cache::{test_assemble as assemble, test_entry as entry};
    use std::io::Write;
    use std::sync::atomic::{AtomicU64, Ordering};

    const NOW: i64 = 1_800_000_000;
    const ORG: &str = "11111111-2222-3333-4444-555555555555";
    const OTHER_ORG: &str = "aaaaaaaa-bbbb-cccc-dddd-eeeeeeeeeeee";

    pub(super) fn usage_body(used: [f64; 2], resets: [i64; 2]) -> Vec<u8> {
        let reset = |timestamp| {
            chrono::DateTime::from_timestamp(timestamp, 0)
                .unwrap()
                .to_rfc3339()
        };
        serde_json::to_vec(&serde_json::json!({
            "five_hour": {"utilization": used[0], "resets_at": reset(resets[0])},
            "seven_day": {"utilization": used[1], "resets_at": reset(resets[1])}
        }))
        .unwrap()
    }

    fn body() -> Vec<u8> {
        usage_body([12.0, 34.0], [NOW + 3600, NOW + 86_400])
    }

    struct Directory(PathBuf);

    impl Directory {
        fn new() -> Self {
            static NEXT: AtomicU64 = AtomicU64::new(0);
            let path = std::env::temp_dir().join(format!(
                "cosmic-claude-usage-cache-{}-{}",
                std::process::id(),
                NEXT.fetch_add(1, Ordering::Relaxed)
            ));
            std::fs::create_dir(&path).unwrap();
            Self(path)
        }

        fn write(
            &self,
            organization: &str,
            suffix: &str,
            timestamp: i64,
            used: [f64; 2],
        ) -> PathBuf {
            let key = usage_key(organization, suffix);
            let path = self.0.join("Cache/Cache_Data").join(cache_filename(&key));
            std::fs::create_dir_all(path.parent().unwrap()).unwrap();
            std::fs::write(
                &path,
                entry(
                    &key,
                    timestamp,
                    "identity",
                    &usage_body(used, [NOW + 3600, NOW + 86_400]),
                ),
            )
            .unwrap();
            path
        }
    }

    impl Drop for Directory {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    #[test]
    fn decodes_actual_resets_in_all_supported_encodings() {
        let key = usage_key(ORG, "");
        let body = body();
        let mut gzip = flate2::write::GzEncoder::new(Vec::new(), flate2::Compression::default());
        gzip.write_all(&body).unwrap();
        for (encoding, compressed) in [
            ("identity", body.clone()),
            ("gzip", gzip.finish().unwrap()),
            (
                "zstd",
                zstd::stream::encode_all(body.as_slice(), 1).unwrap(),
            ),
        ] {
            let snapshot =
                parse_entry(&entry(&key, NOW - 10, encoding, &compressed), &key, NOW).unwrap();
            assert_eq!(snapshot.source, Some(UsageSource::ClaudeDesktop));
            assert_eq!(snapshot.updated_at, Some(NOW - 10));
            assert_eq!(snapshot.windows[0].remaining_percent, 88.0);
            assert_eq!(snapshot.windows[0].resets_at, Some(NOW + 3600));
            assert_eq!(snapshot.windows[1].remaining_percent, 66.0);
            assert_eq!(snapshot.windows[1].resets_at, Some(NOW + 86_400));
            assert_eq!(snapshot.status, UsageStatus::Available);
        }
    }

    #[test]
    fn accepts_desktop_streaming_zstd_window_for_small_reports() {
        let key = usage_key(ORG, "");
        let mut encoder = zstd::stream::write::Encoder::new(Vec::new(), 1).unwrap();
        // Claude's HTTP response is a streaming frame with an unknown content
        // size and a 2 MiB window, even though the JSON itself is only a few KiB.
        encoder.window_log(21).unwrap();
        encoder.include_contentsize(false).unwrap();
        encoder.write_all(&body()).unwrap();
        let compressed = encoder.finish().unwrap();
        let snapshot = parse_entry(&entry(&key, NOW, "zstd", &compressed), &key, NOW).unwrap();
        assert_eq!(snapshot.windows[0].resets_at, Some(NOW + 3600));
        assert_eq!(snapshot.windows[1].resets_at, Some(NOW + 86_400));
    }

    #[test]
    fn rejects_corruption_truncation_and_unsupported_formats() {
        let key = usage_key(ORG, "");
        let bytes = entry(&key, NOW, "identity", &body());
        // Header, key, body CRC, metadata CRC, key digest, and both trailers.
        for position in [
            0,
            8,
            12,
            HEADER_SIZE,
            HEADER_SIZE + key.len(),
            bytes.len() - 24,
            bytes.len() - 28,
            bytes.len() - 40,
            bytes.len() - 12,
        ] {
            let mut corrupt = bytes.clone();
            corrupt[position] ^= 0x80;
            assert!(
                parse_entry(&corrupt, &key, NOW).is_none(),
                "offset {position}"
            );
        }
        for length in 0..bytes.len() {
            assert!(
                parse_entry(&bytes[..length], &key, NOW).is_none(),
                "length {length}"
            );
        }
        assert!(parse_entry(&bytes, &usage_key(OTHER_ORG, ""), NOW).is_none());
        assert!(parse_entry(&entry(&key, NOW, "br", &body()), &key, NOW).is_none());
        assert!(parse_entry(&entry(&key, NOW, "zstd", b"invalid"), &key, NOW).is_none());
        assert!(parse_entry(&entry(&key, NOW, "gzip", b"invalid"), &key, NOW).is_none());
    }

    #[test]
    fn rejects_bad_http_metadata_and_partial_usage() {
        let key = usage_key(ORG, "");
        let date = chrono::DateTime::from_timestamp(NOW, 0)
            .unwrap()
            .to_rfc2822();
        for headers in [
            format!("HTTP/1.1 403\0date:{date}\0content-type:application/json\0\0"),
            format!("HTTP/1.1 200\0date:{date}\0content-type:text/html\0\0"),
            "HTTP/1.1 200\0date:invalid\0content-type:application/json\0\0".into(),
            format!("HTTP/1.1 200\0date:{date}\0date:{date}\0content-type:application/json\0\0"),
            format!("HTTP/1.1 200\0date:{date}\0content-type:application/json"),
        ] {
            assert!(parse_entry(&assemble(&key, headers.as_bytes(), &body()), &key, NOW).is_none());
        }
        let partial = br#"{"five_hour":{"utilization":12,"resets_at":null}}"#;
        assert!(parse_entry(&entry(&key, NOW, "identity", partial), &key, NOW).is_none());
    }

    #[test]
    fn bounds_compressed_input_decoded_output_and_decoder_window() {
        let key = usage_key(ORG, "");
        let oversized = vec![b' '; MAX_BODY_BYTES as usize + 1];
        assert!(decode_body(&oversized, "identity").is_none());
        let compressed = zstd::stream::encode_all(oversized.as_slice(), 1).unwrap();
        assert!(parse_entry(&entry(&key, NOW, "zstd", &compressed), &key, NOW).is_none());
        let mut gzip = flate2::write::GzEncoder::new(Vec::new(), flate2::Compression::default());
        gzip.write_all(&oversized).unwrap();
        assert!(decode_body(&gzip.finish().unwrap(), "gzip").is_none());
        assert!(parse_entry(&vec![0; MAX_ENTRY_BYTES as usize + 1], &key, NOW).is_none());
        // A frame advertising an 8 MiB window must fail even before producing
        // enough output to hit the independent 512 KiB output bound.
        let mut encoder = zstd::stream::write::Encoder::new(Vec::new(), 1).unwrap();
        encoder.window_log(23).unwrap();
        encoder.include_contentsize(false).unwrap();
        encoder.write_all(&vec![0; 2 * 1024 * 1024]).unwrap();
        let compressed = encoder.finish().unwrap();
        assert!(decode_body(&compressed, "zstd").is_none());
    }

    #[test]
    fn preserves_report_age_and_marks_elapsed_reset_windows_stale() {
        let key = usage_key(ORG, "");
        assert!(parse_entry(&entry(&key, NOW + 1, "identity", &body()), &key, NOW).is_none());
        assert!(
            parse_entry(
                &entry(&key, NOW - MAX_AGE_SECONDS - 1, "identity", &body()),
                &key,
                NOW
            )
            .is_none()
        );
        let stale = parse_entry(
            &entry(&key, NOW - FRESH_SECONDS - 1, "identity", &body()),
            &key,
            NOW,
        )
        .unwrap();
        assert_eq!(stale.status, UsageStatus::Stale);
        assert!(stale.windows.iter().all(|window| window.is_stale));
        let elapsed = usage_body([12.0, 34.0], [NOW, NOW + 100]);
        let expired = parse_entry(&entry(&key, NOW, "identity", &elapsed), &key, NOW).unwrap();
        assert!(expired.windows[0].is_stale);
        assert!(!expired.windows[1].is_stale);
    }

    #[test]
    fn reader_uses_newest_complete_snapshot_and_invalidates_changed_entries() {
        let directory = Directory::new();
        let old_path = directory.write(ORG, "", NOW - 100, [10.0, 20.0]);
        let new_path = directory.write(ORG, "?skip_spend=1", NOW - 10, [30.0, 40.0]);
        let mut reader = UsageCacheReader::default();
        let snapshot = reader.snapshot(&directory.0, ORG, NOW).unwrap();
        assert_eq!(snapshot.windows[0].remaining_percent, 70.0);
        assert_eq!(snapshot.windows[1].remaining_percent, 60.0);
        assert_eq!(snapshot.updated_at, Some(NOW - 10));
        // Cached bytes keep their original timestamp and expire normally.
        assert_eq!(
            reader
                .snapshot(&directory.0, ORG, NOW + FRESH_SECONDS + 1)
                .unwrap()
                .status,
            UsageStatus::Stale
        );
        assert!(
            reader
                .snapshot(&directory.0, ORG, NOW + MAX_AGE_SECONDS + 1)
                .is_none()
        );
        std::fs::write(&new_path, b"in-progress rewrite").unwrap();
        assert_eq!(
            reader.snapshot(&directory.0, ORG, NOW).unwrap().updated_at,
            Some(NOW - 100)
        );
        std::fs::remove_file(old_path).unwrap();
        assert!(reader.snapshot(&directory.0, ORG, NOW).is_none());
    }

    #[test]
    fn cache_paths_are_bound_to_exact_organization_and_directory() {
        let directory = Directory::new();
        directory.write(ORG, "", NOW, [10.0, 20.0]);
        let mut reader = UsageCacheReader::default();
        assert!(reader.snapshot(&directory.0, ORG, NOW).is_some());
        assert!(reader.snapshot(&directory.0, OTHER_ORG, NOW).is_none());
        assert!(
            reader
                .snapshot(&directory.0, "../../another-file", NOW)
                .is_none()
        );
        let other = Directory::new();
        assert!(reader.snapshot(&other.0, ORG, NOW).is_none());
        assert!(reader.snapshot(&directory.0, ORG, NOW).is_some());
        // A cache hash collision or misplaced entry still has to match its key.
        let source = directory
            .0
            .join("Cache/Cache_Data")
            .join(cache_filename(&usage_key(ORG, "")));
        let destination = directory
            .0
            .join("Cache/Cache_Data")
            .join(cache_filename(&usage_key(OTHER_ORG, "")));
        std::fs::copy(source, destination).unwrap();
        assert!(reader.snapshot(&directory.0, OTHER_ORG, NOW).is_none());
    }

    #[test]
    #[ignore = "reads local Claude Desktop usage cache"]
    fn live_desktop_cache_contains_reset_timestamps() {
        let directory = dirs::config_dir().unwrap().join("Claude");
        let bytes = super::super::read_bounded(
            &directory.join("plan-usage-history.json"),
            super::super::MAX_HISTORY_BYTES,
        )
        .unwrap();
        let history: super::super::DesktopHistory = serde_json::from_slice(&bytes).unwrap();
        let org = history
            .samples
            .iter()
            .max_by_key(|sample| sample.t)
            .unwrap()
            .org
            .as_deref()
            .unwrap();
        let snapshot = UsageCacheReader::default()
            .snapshot(&directory, org, chrono::Utc::now().timestamp())
            .unwrap();
        assert_eq!(snapshot.source, Some(UsageSource::ClaudeDesktop));
        assert!(
            snapshot
                .windows
                .iter()
                .any(|window| window.resets_at.is_some())
        );
        eprintln!(
            "{} usage windows; {} reset timestamps",
            snapshot.windows.len(),
            snapshot
                .windows
                .iter()
                .filter(|window| window.resets_at.is_some())
                .count()
        );
    }
}
