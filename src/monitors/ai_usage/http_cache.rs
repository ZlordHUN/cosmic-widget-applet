// SPDX-License-Identifier: MPL-2.0

//! Bounded parsing for exact Chromium HTTP cache entries. Provider readers
//! choose known usage keys; no cookies, credentials, or other requests are read.

use sha1::Digest as _;
use sha2::Digest as _;
use std::io::Read;

pub(super) const MAX_ENTRY_BYTES: u64 = 256 * 1024;
pub(super) const MAX_BODY_BYTES: u64 = 512 * 1024;
const MAX_METADATA_BYTES: usize = 64 * 1024;
pub(super) const HEADER_SIZE: usize = 24;
const TRAILER_SIZE: usize = 24;
const HEADER_MAGIC: u64 = 0xfcfb_6d1b_a772_5c30;
const TRAILER_MAGIC: u64 = 0xf4fa_6f45_970d_41d8;
const HAS_CRC: u32 = 1;
const HAS_KEY_SHA256: u32 = 2;

pub(super) struct CachedResponse {
    pub(super) updated_at: i64,
    pub(super) body: Vec<u8>,
}

pub(super) fn cache_filename(key: &str) -> String {
    let digest = sha1::Sha1::digest(key.as_bytes());
    let hash = u64::from_le_bytes(digest[..8].try_into().expect("SHA-1 has eight bytes"));
    format!("{hash:016x}_0")
}

fn u32_at(bytes: &[u8], offset: usize) -> Option<u32> {
    Some(u32::from_le_bytes(
        bytes.get(offset..offset.checked_add(4)?)?.try_into().ok()?,
    ))
}

fn u64_at(bytes: &[u8], offset: usize) -> Option<u64> {
    Some(u64::from_le_bytes(
        bytes.get(offset..offset.checked_add(8)?)?.try_into().ok()?,
    ))
}

pub(super) fn parse_entry(
    bytes: &[u8],
    key: &str,
    now: i64,
    max_age: i64,
) -> Option<CachedResponse> {
    if bytes.len() as u64 > MAX_ENTRY_BYTES
        || u64_at(bytes, 0)? != HEADER_MAGIC
        || u32_at(bytes, 8)? != 5
    {
        return None;
    }
    let key_size = u32_at(bytes, 12)? as usize;
    if key_size != key.len() || key_size > 512 {
        return None;
    }
    let body_start = HEADER_SIZE.checked_add(key_size)?;
    if bytes.get(HEADER_SIZE..body_start)? != key.as_bytes() {
        return None;
    }

    // The last trailer describes stream 0 (HTTP response metadata). Its key
    // digest immediately precedes it. Stream 1 (body) comes before metadata.
    let metadata_trailer = bytes.len().checked_sub(TRAILER_SIZE)?;
    if u64_at(bytes, metadata_trailer)? != TRAILER_MAGIC
        || u32_at(bytes, metadata_trailer + 8)? != (HAS_CRC | HAS_KEY_SHA256)
    {
        return None;
    }
    let metadata_size = u32_at(bytes, metadata_trailer + 16)? as usize;
    if metadata_size > MAX_METADATA_BYTES {
        return None;
    }
    let digest_start = metadata_trailer.checked_sub(32)?;
    if bytes.get(digest_start..metadata_trailer)? != sha2::Sha256::digest(key.as_bytes()).as_slice()
    {
        return None;
    }
    let metadata_start = digest_start.checked_sub(metadata_size)?;
    let body_end = metadata_start.checked_sub(TRAILER_SIZE)?;
    if body_end < body_start
        || u64_at(bytes, body_end)? != TRAILER_MAGIC
        || u32_at(bytes, body_end + 8)? != HAS_CRC
    {
        return None;
    }
    let body = bytes.get(body_start..body_end)?;
    let metadata = bytes.get(metadata_start..digest_start)?;
    if crc32fast::hash(body) != u32_at(bytes, body_end + 12)?
        || crc32fast::hash(metadata) != u32_at(bytes, metadata_trailer + 12)?
    {
        return None;
    }
    let (updated_at, encoding) = response_headers(metadata)?;
    let age = now.checked_sub(updated_at)?;
    if updated_at <= 0 || !(0..=max_age).contains(&age) {
        return None;
    }
    let body = decode_body(body, encoding)?;
    Some(CachedResponse { updated_at, body })
}

fn response_headers(metadata: &[u8]) -> Option<(i64, &str)> {
    // Chromium serializes the response header block as NUL-delimited strings
    // inside a Pickle. Locate that block without deserializing unrelated fields.
    let start = metadata
        .windows(9)
        .position(|bytes| bytes == b"HTTP/1.1 ")?;
    let mut lines = metadata.get(start..)?.split(|byte| *byte == 0);
    let status = std::str::from_utf8(lines.next()?).ok()?;
    if status.split_ascii_whitespace().nth(1)? != "200" {
        return None;
    }
    let mut date = None;
    let mut encoding = None;
    let mut content_type = None;
    let mut terminated = false;
    for line in lines {
        if line.is_empty() {
            terminated = true;
            break;
        }
        let line = std::str::from_utf8(line).ok()?;
        let (name, value) = line.split_once(':')?;
        let value = value.trim();
        if name.eq_ignore_ascii_case("date") {
            if date.is_some() {
                return None;
            }
            date = Some(
                chrono::DateTime::parse_from_rfc2822(value)
                    .ok()?
                    .timestamp(),
            );
        } else if name.eq_ignore_ascii_case("content-encoding") {
            if encoding.is_some() {
                return None;
            }
            encoding = Some(value);
        } else if name.eq_ignore_ascii_case("content-type") {
            if content_type.is_some() {
                return None;
            }
            content_type = Some(value);
        }
    }
    if !terminated
        || !content_type?
            .split(';')
            .next()?
            .trim()
            .eq_ignore_ascii_case("application/json")
    {
        return None;
    }
    Some((date?, encoding.unwrap_or("identity")))
}

pub(super) fn decode_body(body: &[u8], encoding: &str) -> Option<Vec<u8>> {
    let reader: Box<dyn Read + '_> = if encoding.eq_ignore_ascii_case("identity") {
        Box::new(body)
    } else if encoding.eq_ignore_ascii_case("zstd") {
        let mut decoder = zstd::stream::read::Decoder::new(body).ok()?;
        // Prevent small compressed files from requesting unbounded decoder RAM.
        // Desktop's HTTP compressor advertises a 2 MiB streaming window
        // even for its small usage payloads. Allow that with a 4 MiB ceiling.
        decoder.window_log_max(22).ok()?;
        Box::new(decoder)
    } else if encoding.eq_ignore_ascii_case("br") {
        // The library also accepts the large-window extension by default.
        // Validate the stream header before constructing it, matching zstd's
        // 4 MiB decoder window ceiling independently of the decoded size cap.
        if !bounded_brotli_window(*body.first()?) {
            return None;
        }
        Box::new(brotli_decompressor::Decompressor::new(body, 4096))
    } else if encoding.eq_ignore_ascii_case("gzip") {
        Box::new(flate2::read::MultiGzDecoder::new(body))
    } else {
        return None;
    };
    let mut decoded = Vec::new();
    reader
        .take(MAX_BODY_BYTES + 1)
        .read_to_end(&mut decoded)
        .ok()?;
    (decoded.len() as u64 <= MAX_BODY_BYTES).then_some(decoded)
}

// Brotli's WBITS field is packed least-significant bit first into at most
// seven bits. A low seven-bit value of 0x11 introduces the large-window format.
fn bounded_brotli_window(header: u8) -> bool {
    if header & 1 == 0 {
        return true; // 16 bits.
    }
    let next = (header >> 1) & 7;
    if next != 0 {
        return 17 + next <= 22;
    }
    (header >> 4) & 7 != 1 // 10..15 or 17 bits; reject the extension.
}

#[cfg(test)]
fn trailer(output: &mut Vec<u8>, flags: u32, crc: u32, size: u32) {
    output.extend_from_slice(&TRAILER_MAGIC.to_le_bytes());
    output.extend_from_slice(&flags.to_le_bytes());
    output.extend_from_slice(&crc.to_le_bytes());
    output.extend_from_slice(&size.to_le_bytes());
    output.extend_from_slice(&0u32.to_le_bytes());
}

#[cfg(test)]
pub(super) fn test_entry(key: &str, timestamp: i64, encoding: &str, body: &[u8]) -> Vec<u8> {
    let date = chrono::DateTime::from_timestamp(timestamp, 0)
        .unwrap()
        .format("%a, %d %b %Y %H:%M:%S GMT")
        .to_string();
    let mut metadata = vec![0; 40];
    metadata.extend_from_slice(format!(
        "HTTP/1.1 200\0date:{date}\0content-type:application/json\0content-encoding:{encoding}\0\0"
    ).as_bytes());
    test_assemble(key, &metadata, body)
}

#[cfg(test)]
pub(super) fn test_assemble(key: &str, metadata: &[u8], body: &[u8]) -> Vec<u8> {
    let mut output = Vec::new();
    output.extend_from_slice(&HEADER_MAGIC.to_le_bytes());
    output.extend_from_slice(&5u32.to_le_bytes());
    output.extend_from_slice(&(key.len() as u32).to_le_bytes());
    output.extend_from_slice(&[0; 8]);
    output.extend_from_slice(key.as_bytes());
    output.extend_from_slice(body);
    trailer(&mut output, HAS_CRC, crc32fast::hash(body), 0);
    output.extend_from_slice(metadata);
    output.extend_from_slice(sha2::Sha256::digest(key.as_bytes()).as_slice());
    trailer(
        &mut output,
        HAS_CRC | HAS_KEY_SHA256,
        crc32fast::hash(metadata),
        metadata.len() as u32,
    );
    output
}

#[cfg(test)]
mod tests {
    use super::*;

    const NOW: i64 = 1_800_000_000;
    const KEY: &str = "1/0/https://example.invalid/usage";
    const JSON: &[u8] = br#"{"ok":true,"remaining":42}"#;
    // Fixed Brotli fixtures generated with quality 5 and a 22-bit window.
    const BROTLI_JSON: &[u8] = &[
        139, 12, 128, 123, 34, 111, 107, 34, 58, 116, 114, 117, 101, 44, 34, 114, 101, 109, 97,
        105, 110, 105, 110, 103, 34, 58, 52, 50, 125, 3,
    ];
    const BROTLI_OVERSIZED: &[u8] = &[91, 0, 0, 8, 64, 2, 36, 30, 11, 68, 247, 254, 0];

    #[test]
    fn reads_brotli_json_with_original_response_timestamp() {
        let cached =
            parse_entry(&test_entry(KEY, NOW - 10, "br", BROTLI_JSON), KEY, NOW, 60).unwrap();
        assert_eq!(cached.updated_at, NOW - 10);
        assert_eq!(cached.body, JSON);
        // Each provider defines its own maximum acceptable report age.
        assert!(parse_entry(&test_entry(KEY, NOW - 10, "br", BROTLI_JSON), KEY, NOW, 9).is_none());
    }

    #[test]
    fn rejects_brotli_expansion_large_windows_and_incomplete_streams() {
        // This thirteen-byte frame expands to 512 KiB + 1 byte.
        assert!(decode_body(BROTLI_OVERSIZED, "br").is_none());
        let mut too_large_window = BROTLI_JSON.to_vec();
        too_large_window[0] = 141; // The same complete fixture with WBITS = 23.
        assert!(decode_body(&too_large_window, "br").is_none());
        assert!(decode_body(&[0x11, 30], "br").is_none());
        for length in 0..BROTLI_JSON.len() {
            assert!(
                decode_body(&BROTLI_JSON[..length], "br").is_none(),
                "length {length}"
            );
        }
    }
}
