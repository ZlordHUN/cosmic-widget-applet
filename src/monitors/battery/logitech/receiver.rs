// SPDX-License-Identifier: MPL-2.0

//! Pairing metadata discovery for Logitech receiver families.

use std::fs::File;

use super::sysfs::ReceiverKind;
use super::transport::receiver_request;

#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct PairedDevice {
    pub(super) slot: u8,
    pub(super) name: Option<String>,
    pub(super) kind: Option<String>,
}

pub(super) fn paired_devices(
    handle: &mut File,
    receiver_kind: ReceiverKind,
) -> Result<Vec<PairedDevice>, String> {
    let connection = receiver_request(handle, 0x8102, &[])?;
    let expected = connection
        .get(1)
        .copied()
        .ok_or_else(|| "Logitech receiver connection count was missing".to_string())?
        as usize;
    let mut devices = Vec::with_capacity(expected);

    for slot in 1..=6 {
        if devices.len() >= expected {
            break;
        }
        let paired = match receiver_kind {
            ReceiverKind::Bolt => paired_bolt_device(handle, slot),
            ReceiverKind::Legacy27Mhz => None,
            ReceiverKind::Unifying
            | ReceiverKind::Nano
            | ReceiverKind::Lightspeed
            | ReceiverKind::Unknown => paired_standard_device(handle, slot),
        };
        if let Some(paired) = paired {
            devices.push(paired);
        }
    }

    if devices.is_empty() {
        let fallback_slots = match receiver_kind {
            ReceiverKind::Legacy27Mhz => expected.min(4),
            _ => expected.min(6),
        };
        devices.extend((1..=fallback_slots).map(|slot| PairedDevice {
            slot: slot as u8,
            name: None,
            kind: legacy_receiver_kind(receiver_kind, slot as u8),
        }));
    }

    Ok(devices)
}

fn paired_bolt_device(handle: &mut File, slot: u8) -> Option<PairedDevice> {
    let pairing = receiver_request(handle, 0x83b5, &[0x50 + slot]).ok()?;
    let kind = pairing.get(1).copied().map(|value| value & 0x0f);
    let product_id = parse_bolt_product_id(&pairing);
    let name = receiver_request(handle, 0x83b5, &[0x60 + slot, 0x01])
        .ok()
        .and_then(|response| parse_bolt_name(&response))
        .filter(|name| name.chars().count() > 2)
        .or_else(|| product_id.and_then(known_logitech_name).map(str::to_string));

    Some(PairedDevice {
        slot,
        name,
        kind: kind.and_then(hidpp10_kind),
    })
}

fn paired_standard_device(handle: &mut File, slot: u8) -> Option<PairedDevice> {
    let pairing_subregister = 0x20 + slot - 1;
    let pairing = receiver_request(handle, 0x83b5, &[pairing_subregister]).ok()?;
    let kind = pairing.get(7).copied().map(|value| value & 0x0f);
    let name_subregister = 0x40 + slot - 1;
    let name = receiver_request(handle, 0x83b5, &[name_subregister])
        .ok()
        .and_then(|response| parse_receiver_codename(&response));

    Some(PairedDevice {
        slot,
        name,
        kind: kind.and_then(hidpp10_kind),
    })
}

fn parse_bolt_name(response: &[u8]) -> Option<String> {
    let length = usize::from(*response.get(2)?).min(14);
    let name = response.get(3..3 + length)?;
    std::str::from_utf8(name)
        .ok()
        .map(|name| {
            name.trim_matches(|character: char| character == '\0' || character.is_whitespace())
        })
        .filter(|name| !name.is_empty())
        .map(str::to_string)
}

fn parse_receiver_codename(response: &[u8]) -> Option<String> {
    let length = usize::from(*response.get(1)?);
    let name = response.get(2..2 + length)?;
    std::str::from_utf8(name)
        .ok()
        .map(|name| {
            name.trim_matches(|character: char| character == '\0' || character.is_whitespace())
        })
        .filter(|name| !name.is_empty())
        .map(str::to_string)
}

fn parse_bolt_product_id(response: &[u8]) -> Option<u16> {
    Some(u16::from_be_bytes([*response.get(3)?, *response.get(2)?]))
}

fn known_logitech_name(product_id: u16) -> Option<&'static str> {
    match product_id {
        0xb367 => Some("MX Mechanical Mini"),
        _ => None,
    }
}

fn legacy_receiver_kind(receiver_kind: ReceiverKind, slot: u8) -> Option<String> {
    if receiver_kind != ReceiverKind::Legacy27Mhz {
        return None;
    }
    match slot {
        1 | 2 => Some("mouse".to_string()),
        3 => Some("keyboard".to_string()),
        4 => Some("numpad".to_string()),
        _ => None,
    }
}

fn hidpp10_kind(kind: u8) -> Option<String> {
    let kind = match kind {
        0x01 => "keyboard",
        0x02 => "mouse",
        0x03 => "numpad",
        0x04 => "presenter",
        0x08 => "trackball",
        0x09 => "touchpad",
        0x0d => "headset",
        _ => return None,
    };
    Some(kind.to_string())
}

#[cfg(test)]
mod tests {
    use super::{parse_bolt_name, parse_bolt_product_id, parse_receiver_codename};

    #[test]
    fn parses_bolt_pairing_metadata() {
        assert_eq!(
            parse_bolt_product_id(&[0x54, 0x01, 0x67, 0xb3, 0x79, 0x66, 0x51, 0xb5]),
            Some(0xb367)
        );
        assert_eq!(
            parse_bolt_name(&[0x64, 0x01, 0x04, b'K', b'E', b'Y', b'S']),
            Some("KEYS".to_string())
        );
    }

    #[test]
    fn parses_unifying_codename() {
        assert_eq!(
            parse_receiver_codename(&[0x40, 0x05, b'M', b'5', b'1', b'0', b'\0']),
            Some("M510".to_string())
        );
    }
}
