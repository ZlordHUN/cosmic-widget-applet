# Supported Devices

[Overview](README.md) | [Architecture](ARCHITECTURE.md) | [Supported Devices](SUPPORTED_DEVICES.md)

This document is the battery compatibility contract for COSMIC Widget. It lists
every explicit native device identity in the source tree and the complete
protocol rules used for dynamically discovered Logitech peripherals.

Battery percentage and charging state are separate capabilities. A device can
provide an accurate percentage without exposing charging state.

## Support Levels

- **Native, protocol-based**: recognized by transport and battery features, not
  by a model whitelist.
- **Native, explicit**: recognized by the USB identities listed below.
- **Fallback**: recognized by an installed Solaar or HeadsetControl version
  after a native query is unavailable.

The native readers are the normal path. External tools are optional.

## Logitech HID++ Peripherals

Logitech support is intentionally protocol-based. Every Logitech mouse,
keyboard, trackball, touchpad, presenter, numpad, or headset is compatible when
Linux exposes a supported HID++ endpoint and the device exposes at least one of
these battery interfaces:

- HID++ 2.0 Battery Status (`0x1000`)
- HID++ 2.0 Battery Voltage (`0x1001`)
- HID++ 2.0 Unified Battery (`0x1004`)
- HID++ 2.0 ADC Measurement (`0x1f20`)
- Logitech Centurion Battery SoC (`0x0104`)
- HID++ 1.0 battery charge register (`0x0d`)
- HID++ 1.0 battery status register (`0x07`)
- Linux `power_supply` entries with manufacturer `Logitech` and scope `Device`

Device names and kinds are read from the hardware. There is no finite Logitech
model whitelist to keep in sync, so compatibility is defined by the interfaces
above rather than a marketing-name list.

### Logitech Transports

| Transport | Recognized USB identities |
| --- | --- |
| Bolt receiver | `046d:c548` |
| Unifying receiver | `046d:c52b`, `046d:c532` |
| Nano receivers | `046d:c52f`, `046d:c518`, `046d:c51a`, `046d:c51b`, `046d:c521`, `046d:c525`, `046d:c526`, `046d:c52e`, `046d:c531`, `046d:c534`, `046d:c535`, `046d:c537` |
| LIGHTSPEED receivers | `046d:c539`, `046d:c53a`, `046d:c53d`, `046d:c53f`, `046d:c541`, `046d:c545`, `046d:c547`, `046d:c54d` |
| Legacy 27 MHz receiver | `046d:c517` |
| Other Logitech receivers | `046d:c500` through `046d:c5ff` when the HID report descriptor is compatible |
| Lenovo-branded Nano receiver | `17ef:6042` |
| Direct USB or Bluetooth | Logitech vendor `046d` with compatible HID++ or Centurion reports |

The project has been hardware-verified with:

- Logitech G309 LIGHTSPEED
- Logitech MX Mechanical Mini

Solaar can be enabled in Settings as a fallback for a compatible device that is
blocked from the native hidraw path or uses a newly introduced transport.

## Native Gaming Headsets

All identities in this section have model-specific native battery readers. The
USB IDs are the authoritative match criteria.

### Audeze

| Device | USB IDs |
| --- | --- |
| Audeze Maxwell, PlayStation/PC receiver | `3329:4b19` |
| Audeze Maxwell, Xbox receiver | `3329:4b18` |
| Audeze Maxwell 2, PlayStation/PC | `3329:4b29` |

For the first-generation Maxwell, `3329:4b1a` and `3329:4b1e` are also observed
to determine whether a connected headset is charging over USB-C.

### Corsair

Corsair's protocol registry does not expose distinct marketing names for every
product ID, so the complete identity list is grouped by protocol.

| Device family | USB IDs |
| --- | --- |
| Corsair VOID protocol family | `1b1c:1b1c`, `1b1c:1b27`, `1b1c:0a14`, `1b1c:0a16`, `1b1c:0a17`, `1b1c:0a1d`, `1b1c:0a1a`, `1b1c:1b2a`, `1b1c:1b23`, `1b1c:1b29`, `1b1c:0a55`, `1b1c:0a51`, `1b1c:0a52`, `1b1c:0a38`, `1b1c:0a4f`, `1b1c:0a2b`, `1b1c:0a75`, `1b1c:0a56` |
| Corsair Wireless V2 protocol family | `1b1c:2a08`, `1b1c:2a02` |

### HyperX

| Device | USB IDs |
| --- | --- |
| HyperX Cloud Alpha Wireless | `03f0:098d` |
| HyperX Cloud Flight Wireless, original | `0951:16c4` |
| HyperX Cloud Flight Wireless, newer revision | `0951:1723` |
| HyperX Cloud II Wireless, HP revision | `03f0:0696` |
| HyperX Cloud II Wireless, Kingston revision | `0951:1718` |

### Logitech Gaming Headsets

These are explicit model-specific readers in addition to the generic HID++
backend.

| Device | USB IDs |
| --- | --- |
| Logitech G930 | `046d:0a1f` |
| Logitech G533 | `046d:0a66` |
| Logitech G535 | `046d:0ac4` |
| Logitech G633 | `046d:0a5c` |
| Logitech G635 | `046d:0a89` |
| Logitech G933 | `046d:0a5b` |
| Logitech G935 | `046d:0a87` |
| Logitech G733 revisions | `046d:0ab5`, `046d:0afe`, `046d:0b1f` |
| Logitech G PRO | `046d:0aa7` |
| Logitech G PRO X revisions | `046d:0aaa`, `046d:0aba` |
| Logitech G PRO X2 revisions | `046d:0afb`, `046d:0afc` |
| Logitech G PRO X 2 LIGHTSPEED | `046d:0af7` |
| Logitech G522 LIGHTSPEED | `046d:0b18` |
| Logitech ASTRO A50 Gen 5 | `046d:0b1c` |

### SteelSeries

| Device | USB IDs |
| --- | --- |
| Arctis 1 | `1038:12b3` |
| Arctis 1 Xbox | `1038:12b6` |
| Arctis 7X | `1038:12d7` |
| Arctis 7P | `1038:12d5` |
| Arctis 7 | `1038:1260` |
| Arctis 7 2019 | `1038:12ad` |
| Arctis Pro 2019 | `1038:1252` |
| Arctis Pro GameDAC | `1038:1280` |
| Arctis 7+ revisions | `1038:220e`, `1038:2212`, `1038:2216`, `1038:2236` |
| Arctis 9 | `1038:12c2` |
| Arctis Pro Wireless | `1038:1290` |
| Arctis Nova Pro Wireless | `1038:12e0`, `1038:12e5` |
| Arctis Nova 7 | `1038:2202`, `1038:22a1` |
| Arctis Nova 7 Wireless Gen 2 | `1038:227e` |
| Arctis Nova 7X revisions | `1038:2206`, `1038:2258`, `1038:229e`, `1038:22ad`, `1038:22a4`, `1038:22a5` |
| Arctis Nova 7 Diablo IV revisions | `1038:223a`, `1038:22a9` |
| Arctis Nova 7 WoW Edition | `1038:227a` |
| Arctis Nova 7P revisions | `1038:220a`, `1038:22a7` |
| Arctis Nova 5 | `1038:2232` |
| Arctis Nova 5X | `1038:2253` |
| Arctis Nova 3P Wireless | `1038:2269` |
| Arctis Nova 3X Wireless | `1038:226d` |
| Arctis GameBuds | `1038:230a` |

### Other Headsets

| Device | USB IDs |
| --- | --- |
| Lenovo Wireless VoIP Headset | `17ef:a07d` |
| Sony INZONE Buds | `054c:0ec2` |

HeadsetControl remains an optional fallback. The built-in registry covers every
battery-capable device in the HeadsetControl source used for this
implementation; a newer installed HeadsetControl release can extend fallback
coverage without changing the native registry.

## Native Controller

| Device | USB IDs |
| --- | --- |
| Razer Wolverine V3 Pro 8K PC, wired | `1532:0a57` |
| Razer Wolverine V3 Pro 8K PC, wireless dongle | `1532:0a59` |

The controller is shown only after a valid battery response. This prevents an
idle USB dongle from making a powered-off controller appear connected.

## Connection and Cache Behavior

- Disconnected explicit devices are removed from the visible list.
- Sleeping Logitech receiver devices retain their last confirmed live reading
  until a new reading arrives.
- On overlay startup, a detected device can temporarily show its cached level
  with a distinct accent-colored icon.
- Cached data becomes the unavailable state if the live backend cannot confirm
  a reading.
- Charging is shown only when the corresponding native protocol reports it.

## Reporting Another Device

Include the device name, connection type, and USB identity from:

```bash
lsusb
```

For hidraw devices, the relevant kernel metadata is available under
`/sys/class/hidraw/*/device/uevent`.
