# Architecture

[Overview](README.md) | [Architecture](ARCHITECTURE.md) | [Supported Devices](SUPPORTED_DEVICES.md)

## Runtime Components

COSMIC Widget is split into three installed processes so the panel, overlay,
and settings window can restart independently.

| Component | Cargo target | Installed command | Responsibility |
| --- | --- | --- | --- |
| Panel applet | `cosmic-widget-applet` | `cosmic-widget-applet` | Panel popup, overlay lifecycle, settings launcher |
| Overlay | `cosmic-widget-iced` | `cosmic-widget` | Layer-shell UI and all live monitoring |
| Settings | `cosmic-widget-settings` | `cosmic-widget-settings` | COSMIC configuration UI |

Cargo also contains an older `cosmic-widget` target backed by the original
smithay-client-toolkit, Cairo, and Pango renderer. It is not installed by
`just install`; the Iced target is installed under that public command name.

## Process Flow

```text
COSMIC panel
    |
    +-- cosmic-widget-applet
            |
            +-- starts/stops --> cosmic-widget
            +-- opens --------> cosmic-widget-settings

cosmic-widget-settings
    |
    +-- writes cosmic-config
            |
            +-- watched by the applet and overlay
```

The processes share configuration, but not in-memory state. The overlay owns
the monitors and runtime caches. A process lock prevents duplicate overlay
instances after compositor or panel restarts.

## Overlay

The production overlay is an Iced daemon using libcosmic's single-worker
executor. It creates a Wayland layer-shell surface through Iced's COSMIC
platform integration.

The surface:

- is anchored from the top-left using saved X and Y margins;
- requests compositor blur and rounded corners;
- does not reserve an exclusive desktop area;
- sizes itself from the enabled sections and their current content;
- accepts pointer input for controls, scrolling, expansion, seeking, and edit
  mode;
- exposes a pin while edit mode is active, then persists the pinned position.

`src/iced_widget/mod.rs` owns application state, subscriptions, animation
state, sizing, and input handling. `src/iced_widget/view.rs` builds the visible
section tree. Small custom widgets provide gauges, marquee text, sliding
transitions, and translated content.

System readings are sampled once per second. The UI interpolates utilization
bars and temperature gauges between samples so animation cadence is independent
from hardware polling cadence.

## Monitoring Pipeline

```text
native APIs / D-Bus / local HTTP / sysfs
                    |
            background monitors
                    |
         synchronized snapshots/state
                    |
            Iced update and view
                    |
          Wayland layer surface
```

Slow I/O and device commands stay outside the Iced update path. Persistent
workers and clients are reused where practical instead of creating a process,
thread, D-Bus connection, or HTTP client for each update.

## Data Sources

| Section | Primary implementation |
| --- | --- |
| Utilization | `sysinfo`, Linux sysfs, and NVML for NVIDIA |
| Network | Linux `/proc` and sysfs counters |
| Disk I/O | Linux `/proc/diskstats` and sysfs metadata |
| Temperatures | `sysinfo` hardware sensors and NVML |
| Storage | `sysinfo` filesystem data and `/sys/class/block` model metadata |
| Devices | Linux `power_supply`, native HID++, and native HID reports |
| Weather | Open-Meteo through a persistent `reqwest` client |
| Notifications | Native `zbus` monitoring and COSMIC history reconciliation |
| Media | MPRIS over `zbus`, Cider HTTP, and Emby discovery/API access |

### Devices

`src/widget/battery.rs` coordinates device discovery, cached startup state,
native readers, deduplication, and optional external fallbacks.

- `battery/logitech.rs` discovers Logitech endpoints and delegates HID++
  protocol, receiver, transport, sysfs, and Centurion handling.
- `battery/headsets.rs` contains the explicit native headset registry and
  dispatches to vendor protocol modules.
- `battery/controllers/` contains model-specific controller readers.
- Solaar and HeadsetControl are discovery/fallback paths, not primary polling
  dependencies.

The complete compatibility contract is documented in
[Supported Devices](SUPPORTED_DEVICES.md).

### Notifications

One monitor connection observes FreeDesktop notification calls, replies, and
close signals. A second reusable session-bus connection handles dismissal and
periodic COSMIC history reconciliation.

COSMIC notification history is restored through the optional
`GetNotificationHistory` extension. When that method is unavailable, the
overlay still captures live notifications and uses its session-scoped local
cache. Dismissal uses the standard `CloseNotification` method and verifies the
notification server owner before reusing an ID.

### Media

The media monitor merges several sources into a stable player list:

- MPRIS players discovered and updated over D-Bus;
- Cider through its local HTTP API;
- Emby sessions found from the local client state and queried over HTTP.

Controls are queued to an asynchronous command worker. Artwork is downloaded
asynchronously through a persistent client and retained in a bounded LRU cache
with entry, byte, and pixel limits. YouTube artwork candidates are keyed by
video identity so lower-resolution updates cannot replace better artwork for
the same track.

## Configuration

`src/config.rs` defines the versioned `cosmic-config` entry shared by all three
installed processes. It controls:

- enabled metrics and sections;
- section order;
- temperature presentation;
- time and percentage display;
- weather location;
- notification and media visibility;
- optional Solaar fallback and debug logging;
- overlay position, autostart, and edit mode.

The settings application writes changes directly. Most visual changes apply
live; surface placement is committed when the user pins the overlay or resets
its position.

## Caches

Files under `~/.cache/cosmic-widget-applet/` reduce empty startup states:

| Cache | Contents |
| --- | --- |
| `widget_cache.json` | Storage identities and last confirmed peripheral battery readings |
| `weather.json` | Resolved location and last successful weather response |
| `notifications.json` | Session-scoped notification fallback history |

Artwork is cached only in memory. Cached battery values are rendered as
provisional until the live backend confirms the device and reading.

## Source Map

```text
src/
|- app.rs                    panel applet
|- config.rs                 shared persistent configuration
|- settings.rs               settings application
|- iced_widget/              production overlay UI
|- widget/
|  |- battery.rs             battery monitor coordinator
|  |- battery/               native device protocol modules
|  |- media.rs               multi-source media coordinator
|  |- media/                 Cider and MPRIS backends
|  |- notifications.rs       D-Bus capture/history/dismissal
|  |- utilization.rs         CPU, memory, and GPU utilization
|  |- temperature.rs         hardware temperatures
|  |- network.rs             network throughput
|  |- disk_io.rs             disk throughput
|  |- storage.rs             mounted filesystem usage
|  `- weather.rs             Open-Meteo client and cache
|- iced_widget_main.rs       production overlay entry point
|- main.rs                   panel applet entry point
`- settings_main.rs          settings entry point
```

## Extending the Project

- Add explicit headset USB identities to the appropriate vendor module and the
  registry chain in `battery/headsets.rs`.
- Add a controller-specific reader under `battery/controllers/`.
- Keep hardware I/O off the UI thread and preserve the last confirmed reading
  only across transient failures.
- Update `SUPPORTED_DEVICES.md` whenever the native registry or protocol
  coverage changes.
- Prefer native Rust APIs and persistent connections over command output
  parsing.
