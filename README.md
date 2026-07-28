# COSMIC Widget

[Overview](README.md) | [Architecture](ARCHITECTURE.md) | [Supported Devices](SUPPORTED_DEVICES.md)

A configurable system monitor overlay for the
[COSMIC desktop](https://github.com/pop-os/cosmic-epoch).

[![License: MPL 2.0](https://img.shields.io/badge/License-MPL_2.0-brightgreen.svg)](https://opensource.org/licenses/MPL-2.0)

COSMIC Widget combines a panel applet, a native COSMIC settings application,
and a frosted-glass desktop overlay.

## Features

- CPU, memory, GPU, network, and disk I/O monitoring
- CPU and GPU temperatures with arc, circular, or text displays
- Local and mounted storage usage, including network filesystems
- Native battery monitoring for Logitech peripherals, gaming headsets, and the
  Razer Wolverine V3 Pro 8K PC
- Open-Meteo weather with no API key
- Grouped, expandable COSMIC notifications with synchronized dismissal
- Multi-source media controls for MPRIS players, Cider, Emby, and browser media
- Reorderable and individually configurable sections
- COSMIC theming, accent colors, blur, rounded corners, and drag-to-position
- Cached weather, notification, storage, and battery state for fast startup

See [Supported Devices](SUPPORTED_DEVICES.md) for the complete battery support
matrix.

## Install

The project requires Rust, Cargo, `just`, a COSMIC desktop session, and the
development packages needed by libcosmic and hidapi.

```bash
just build-release
sudo just install
sudo udevadm control --reload-rules
sudo udevadm trigger
```

Reconnect newly supported USB receivers after the first installation so the
udev permissions take effect. The install recipe places the applet, overlay,
settings application, desktop entries, icon, metadata, and headset udev rules
under `/usr/local`.

Add **COSMIC Widget** to the COSMIC panel. Its popup can show or hide the
overlay and open the settings application.

## Run From Source

```bash
cargo run --release --bin cosmic-widget-applet
cargo run --release --bin cosmic-widget-iced
cargo run --release --bin cosmic-widget-settings
```

Only one overlay instance can run at a time.

## Optional Integrations

- [Solaar](https://github.com/pwr-Solaar/Solaar) can be enabled as a fallback
  for Logitech hardware that the native HID++ reader cannot access.
- [HeadsetControl](https://github.com/Sapd/HeadsetControl) can provide fallback
  support for headset models newer than the built-in registry.
- Cider's local API adds direct Apple Music polling and controls. Standard
  MPRIS players work without Cider.

## Data Locations

Configuration is stored through `cosmic-config` under:

```text
~/.config/cosmic/com.github.zoliviragh.CosmicWidget/v1/
```

Runtime caches are stored under:

```text
~/.cache/cosmic-widget-applet/
```

Cached battery readings are provisional at startup and are replaced by a live
reading or the normal unavailable state.

## Development

```bash
cargo fmt --all -- --check
cargo test --all-targets
cargo clippy --all-targets
```

The production overlay is implemented by the `cosmic-widget-iced` Cargo target
and installed as `cosmic-widget`. The older Cairo target remains in the source
tree only as migration compatibility code.

See [Architecture](ARCHITECTURE.md) for the process model, data flow, and source
layout.

## License

MPL-2.0

Weather icons are from
[Weather Icons](https://github.com/erikflowers/weather-icons) under the SIL OFL
1.1.
