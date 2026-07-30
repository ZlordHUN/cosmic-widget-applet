// SPDX-License-Identifier: MPL-2.0

//! Linux hidraw discovery for Logitech HID++ endpoints.

use std::collections::{HashMap, HashSet};
use std::fs;
use std::path::{Path, PathBuf};

pub(super) const HIDRAW_ROOT: &str = "/sys/class/hidraw";
pub(super) const LOGITECH_VENDOR_ID: u16 = 0x046d;
const LENOVO_VENDOR_ID: u16 = 0x17ef;
const LENOVO_NANO_RECEIVER_PRODUCT_ID: u16 = 0x6042;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum Bus {
    Bluetooth,
    Usb,
    Other(u16),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum ReceiverKind {
    Bolt,
    Unifying,
    Nano,
    Lightspeed,
    Legacy27Mhz,
    Unknown,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum CenturionReport {
    Standard,
    Addressed,
}

impl CenturionReport {
    pub(super) const fn id(self) -> u8 {
        match self {
            Self::Standard => 0x51,
            Self::Addressed => 0x50,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) enum EndpointKind {
    Receiver(ReceiverKind),
    Centurion(CenturionReport),
    Direct,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct HidrawEndpoint {
    pub(super) path: PathBuf,
    pub(super) bus: Bus,
    pub(super) product_id: u16,
    pub(super) name: String,
    pub(super) kind: EndpointKind,
}

pub(super) fn discover_hidpp_endpoints() -> Vec<HidrawEndpoint> {
    discover_hidpp_endpoints_at(Path::new(HIDRAW_ROOT))
}

fn discover_hidpp_endpoints_at(root: &Path) -> Vec<HidrawEndpoint> {
    let Ok(entries) = fs::read_dir(root) else {
        return Vec::new();
    };

    let mut endpoints: Vec<_> = entries
        .filter_map(Result::ok)
        .filter_map(|entry| parse_hidraw_endpoint(&entry.path(), &entry.file_name()))
        .collect();
    endpoints.sort_by(|left, right| left.path.cmp(&right.path));
    endpoints
}

fn parse_hidraw_endpoint(class_path: &Path, file_name: &std::ffi::OsStr) -> Option<HidrawEndpoint> {
    let uevent = fs::read_to_string(class_path.join("device/uevent")).ok()?;
    let descriptor = fs::read(class_path.join("device/report_descriptor")).ok()?;
    let report_kind = supported_report_kind(&descriptor)?;

    let (bus_id, vendor_id, product_id) = parse_hid_id(&uevent)?;
    if vendor_id != LOGITECH_VENDOR_ID
        && !(vendor_id == LENOVO_VENDOR_ID && product_id == LENOVO_NANO_RECEIVER_PRODUCT_ID)
    {
        return None;
    }

    let name = uevent_value(&uevent, "HID_NAME")
        .unwrap_or("Logitech device")
        .trim()
        .to_string();
    let kind = match report_kind {
        ReportKind::Centurion(report) => EndpointKind::Centurion(report),
        ReportKind::Hidpp => receiver_kind(vendor_id, product_id)
            .map(EndpointKind::Receiver)
            .unwrap_or(EndpointKind::Direct),
    };

    Some(HidrawEndpoint {
        path: Path::new("/dev").join(file_name),
        bus: match bus_id {
            0x0003 => Bus::Usb,
            0x0005 => Bus::Bluetooth,
            other => Bus::Other(other),
        },
        product_id,
        name,
        kind,
    })
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ReportKind {
    Hidpp,
    Centurion(CenturionReport),
}

fn supported_report_kind(descriptor: &[u8]) -> Option<ReportKind> {
    let reports = parse_report_descriptor(descriptor)?;
    if reports.input_bits.get(&0x50) == Some(&(63 * 8)) && reports.output_ids.contains(&0x50) {
        return Some(ReportKind::Centurion(CenturionReport::Addressed));
    }
    if reports.input_bits.get(&0x51) == Some(&(63 * 8)) && reports.output_ids.contains(&0x51) {
        return Some(ReportKind::Centurion(CenturionReport::Standard));
    }
    (reports.input_bits.get(&0x10) == Some(&(6 * 8))
        || reports.input_bits.get(&0x11) == Some(&(19 * 8)))
    .then_some(ReportKind::Hidpp)
}

#[derive(Default)]
struct ReportDescriptor {
    input_bits: HashMap<u8, usize>,
    output_ids: HashSet<u8>,
}

fn parse_report_descriptor(descriptor: &[u8]) -> Option<ReportDescriptor> {
    let mut reports = ReportDescriptor::default();
    let mut report_id = 0;
    let mut report_size = 0;
    let mut report_count = 0;
    let mut globals = Vec::new();
    let mut offset = 0;

    while offset < descriptor.len() {
        let prefix = descriptor[offset];
        offset += 1;
        if prefix == 0xfe {
            let length = usize::from(*descriptor.get(offset)?);
            offset = offset.checked_add(length + 2)?;
            if offset > descriptor.len() {
                return None;
            }
            continue;
        }

        let size = match prefix & 0x03 {
            0 => 0,
            1 => 1,
            2 => 2,
            _ => 4,
        };
        let data = descriptor.get(offset..offset + size)?;
        offset += size;
        let value = data
            .iter()
            .enumerate()
            .fold(0usize, |value, (shift, byte)| {
                value | (usize::from(*byte) << (shift * 8))
            });
        let item_type = (prefix >> 2) & 0x03;
        let tag = prefix >> 4;

        match (item_type, tag) {
            (1, 7) => report_size = value,
            (1, 8) => report_id = u8::try_from(value).ok()?,
            (1, 9) => report_count = value,
            (1, 10) => globals.push((report_id, report_size, report_count)),
            (1, 11) => (report_id, report_size, report_count) = globals.pop()?,
            (0, 8) => {
                *reports.input_bits.entry(report_id).or_default() +=
                    report_size.checked_mul(report_count)?;
            }
            (0, 9) => {
                reports.output_ids.insert(report_id);
            }
            _ => {}
        }
    }
    Some(reports)
}

pub(super) fn parse_hid_id(uevent: &str) -> Option<(u16, u16, u16)> {
    let value = uevent_value(uevent, "HID_ID")?;
    let mut parts = value.split(':');
    let bus_id = u32::from_str_radix(parts.next()?, 16).ok()?;
    let vendor_id = u32::from_str_radix(parts.next()?, 16).ok()?;
    let product_id = u32::from_str_radix(parts.next()?, 16).ok()?;
    Some((
        u16::try_from(bus_id).ok()?,
        u16::try_from(vendor_id).ok()?,
        u16::try_from(product_id).ok()?,
    ))
}

fn uevent_value<'a>(uevent: &'a str, key: &str) -> Option<&'a str> {
    let prefix = format!("{key}=");
    uevent.lines().find_map(|line| line.strip_prefix(&prefix))
}

fn receiver_kind(vendor_id: u16, product_id: u16) -> Option<ReceiverKind> {
    if vendor_id == LENOVO_VENDOR_ID && product_id == LENOVO_NANO_RECEIVER_PRODUCT_ID {
        return Some(ReceiverKind::Nano);
    }
    if vendor_id != LOGITECH_VENDOR_ID {
        return None;
    }

    match product_id {
        0xc548 => Some(ReceiverKind::Bolt),
        0xc52b | 0xc532 => Some(ReceiverKind::Unifying),
        0xc52f | 0xc518 | 0xc51a | 0xc51b | 0xc521 | 0xc525 | 0xc526 | 0xc52e | 0xc531 | 0xc534
        | 0xc535 | 0xc537 => Some(ReceiverKind::Nano),
        0xc539 | 0xc53a | 0xc53d | 0xc53f | 0xc541 | 0xc545 | 0xc547 | 0xc54d => {
            Some(ReceiverKind::Lightspeed)
        }
        0xc517 => Some(ReceiverKind::Legacy27Mhz),
        0xc500..=0xc5ff => Some(ReceiverKind::Unknown),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::{
        CenturionReport, ReceiverKind, ReportKind, parse_hid_id, receiver_kind,
        supported_report_kind,
    };

    #[test]
    fn parses_logitech_hid_identity() {
        let uevent = concat!(
            "HID_ID=0003:0000046D:0000C548\n",
            "HID_NAME=Logitech USB Receiver\n",
            "HID_PHYS=usb-test/input2\n",
        );
        assert_eq!(parse_hid_id(uevent), Some((0x0003, 0x046d, 0xc548)));
    }

    #[test]
    fn recognizes_every_solaar_receiver_family() {
        assert_eq!(receiver_kind(0x046d, 0xc548), Some(ReceiverKind::Bolt));
        assert_eq!(receiver_kind(0x046d, 0xc52b), Some(ReceiverKind::Unifying));
        assert_eq!(receiver_kind(0x046d, 0xc534), Some(ReceiverKind::Nano));
        assert_eq!(
            receiver_kind(0x046d, 0xc547),
            Some(ReceiverKind::Lightspeed)
        );
        assert_eq!(
            receiver_kind(0x046d, 0xc517),
            Some(ReceiverKind::Legacy27Mhz)
        );
        assert_eq!(receiver_kind(0x046d, 0xc5fe), Some(ReceiverKind::Unknown));
        assert_eq!(receiver_kind(0x046d, 0x40b1), None);
    }

    #[test]
    fn detects_hidpp_report_ids() {
        assert_eq!(
            supported_report_kind(&[0x85, 0x10, 0x75, 8, 0x95, 6, 0x81, 0]),
            Some(ReportKind::Hidpp)
        );
        assert_eq!(
            supported_report_kind(&[0x85, 0x11, 0x75, 8, 0x95, 19, 0x81, 0]),
            Some(ReportKind::Hidpp)
        );
        assert_eq!(
            supported_report_kind(&[0x85, 0x20, 0x75, 8, 0x95, 19, 0x81, 0]),
            None
        );
    }

    #[test]
    fn detects_both_centurion_report_variants() {
        assert_eq!(
            supported_report_kind(&[0x85, 0x51, 0x75, 8, 0x95, 63, 0x81, 0, 0x91, 0]),
            Some(ReportKind::Centurion(CenturionReport::Standard))
        );
        assert_eq!(
            supported_report_kind(&[0x85, 0x50, 0x75, 8, 0x95, 63, 0x81, 0, 0x91, 0]),
            Some(ReportKind::Centurion(CenturionReport::Addressed))
        );
        assert_eq!(
            supported_report_kind(&[
                0x85, 0x11, 0x75, 8, 0x95, 19, 0x81, 0, 0x85, 0x51, 0x75, 8, 0x95, 63, 0x81, 0,
                0x91, 0,
            ]),
            Some(ReportKind::Centurion(CenturionReport::Standard))
        );
    }
}
