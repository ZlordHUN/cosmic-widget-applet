# Architecture

[Overview](../README.md) | [Architecture](architecture.md) | [Supported Devices](supported-devices.md)

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

`src/overlay/mod.rs` handles startup, including the instance guard and logging.
`src/overlay/app.rs` owns application state, subscriptions, and input handling.
Animation, surface management, and layout have dedicated modules.
`src/overlay/view.rs` composes the feature views in `src/overlay/sections/`.
Reusable controls in `src/overlay/components/` provide gauges, marquee text,
sliding transitions, and translated content.

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
| AI Usage | Local Codex quota reports and the Claude Code account usage endpoint |

### Devices

`src/monitors/battery/mod.rs` coordinates device discovery, cached startup state,
native readers, deduplication, and optional external fallbacks.

- `battery/logitech/mod.rs` discovers Logitech endpoints and delegates HID++
  protocol, receiver, transport, sysfs, and Centurion handling.
- Logitech polling runs on an independent one-second worker. The UI merges its
  latest snapshot when reading battery state, so slower headset/controller or
  CLI queries cannot delay charging updates or overwrite them with older data.
  Persistent HID++ listeners also retain battery notifications between polls,
  allowing charging changes to update a sleeping device's last reading.
- `battery/headsets/mod.rs` contains the explicit native headset registry and
  dispatches to vendor protocol modules.
- `battery/controllers/` contains model-specific controller readers.
- Solaar and HeadsetControl are discovery/fallback paths, not primary polling
  dependencies.

The complete compatibility contract is documented in
[Supported Devices](supported-devices.md).

### Notifications

One monitor connection observes FreeDesktop notification calls, replies, and
close signals. A second reusable session-bus connection handles dismissal and
periodic COSMIC history reconciliation.

COSMIC notification history is restored through the optional
`GetNotificationHistory` extension. Its backward-compatible
`GetNotificationHistoryV2` variant also retains desktop identity and advertised
action keys. Standalone Discord notifications can therefore invoke their
validated default action through `InvokeNotificationAction`, letting Discord
open the originating server channel, thread, or DM without parsing message
text. When these extensions are unavailable, the overlay still captures live
notifications and uses its session-scoped local cache. Dismissal uses the
standard `CloseNotification` method and verifies the notification server owner
before reusing an ID.

COSMIC Files copy/move notifications carry explicit application identity and
transfer-state hints. Active rows show progress and survive history reconciliation;
terminal updates replace the same daemon owner/ID without changing row identity.
Dismissals remain suppressed across transient progress updates, and sender
disconnects remove abandoned active rows. Active transfers are never restored
from the disk cache. The required Files-side patch and wire contract live under
[`integrations/cosmic-files`](../integrations/cosmic-files/README.md).
The accompanying daemon patch renews replacement timers, allowing completion
popups to expire normally while remaining in notification history.

### Media

The media monitor merges several sources into a stable player list:

- MPRIS players discovered and updated over D-Bus;
- Cider through its local HTTP API;
- Emby sessions found from the local client state and queried over HTTP.

When the displayed source disappears or clears its track metadata, the overlay
retains its card for the same 220 ms left slide used by notification dismissal.
The heading and layout height stay fixed until the slide finishes. The outgoing
card cannot send playback commands; the monitor continues tracking live sources.

Controls are queued to an asynchronous command worker. Artwork is downloaded
asynchronously through a persistent client and retained in a bounded LRU cache
with entry, byte, and pixel limits. YouTube artwork candidates are keyed by
video identity so lower-resolution updates cannot replace better artwork for
the same track. Bandcamp album and track pages supply cover metadata when a
browser omits artwork from MPRIS. These lookups share the asynchronous loader,
cache covers by page URL, and retry temporary failures. Confirmed Bandcamp
covers take precedence over browser placeholder images.
The same response supplies the public track list. For Zen/Firefox playback with
only a page caption, an unambiguous whole-second duration match supplies the
track title, artist, and album before timeline tracking. Explicit track URLs
identify tracks directly. Incomplete track lists, duplicate durations, and
unknown durations keep the original caption; browser-supplied track titles
are preserved. No browser extension is required.

### AI Usage

`src/monitors/ai_usage/` groups the shared quota snapshot types and both providers.
Codex reads bounded tails of local session logs every 30 seconds. Claude checks
its account usage endpoint every five minutes using the existing Claude Code
sign-in, with bounded reads, request timeouts, and retry backoff. Neither monitor
makes inference requests or estimates allowance from token counts.

`src/overlay/sections/ai_usage.rs` displays remaining allowance in compact arches
shared with the temperature gauge component. Provider icons, quota labels, and
reset times remain visible; hover details include the age of the report. Stale
reports stay marked, and expired windows wait for a fresh report instead of
assuming the allowance has refilled.

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
|- lib.rs                    shared application/module graph
|- bin/                      thin launchers, preserving existing target names
|- applet/                   panel popup and overlay lifecycle
|- settings/                 settings controller, page views, preview, cache
|- overlay/
|  |- mod.rs                 production overlay startup
|  |- app.rs                 application state and event handling
|  |- view.rs                composition of enabled sections
|  |- sections/              one module per visible feature
|  |- components/            reusable gauges, text and transition widgets
|  |- stats.rs               background sampling and snapshots
|  `- ...                    animation, surface, layout and state helpers
|- monitors/
|  |- ai_usage/              shared quota data, Codex and Claude providers
|  |- battery/               device coordinator and native vendor protocols
|  |- media/                 player coordinator, MPRIS, Cider and Bandcamp
|  |- notifications/         capture/history/dismissal, downloads and transfers
|  |- cache.rs               persistent discovery/readings cache
|  |- utilization.rs         CPU, memory and GPU usage
|  |- temperature.rs         hardware temperatures
|  |- network.rs             network throughput
|  |- disk_io.rs             disk throughput
|  |- storage.rs             mounted filesystem usage
|  `- weather.rs             Open-Meteo client and cache
|- legacy/                   retained Cairo overlay and drawing helpers
|- runtime/                  process lock and switchable logging
|- config.rs                 shared persistent configuration
`- i18n.rs                   shared Fluent localization
assets/                      embedded symbolic icons and fonts
resources/                   desktop entries, metadata, application icon, udev rules
docs/                        architecture, supported devices, screenshots
integrations/                companion patches and their wire contracts
archive/                     unbuilt prototypes and source backups
```

All targets use `src/lib.rs`, so shared monitors and their tests are compiled as
one module graph. Binaries select an application's `run()` entry point rather
than redeclaring the same source files. The legacy renderer consumes the same
monitors as the production overlay; its Cairo drawing helpers remain under
`src/legacy/` and do not belong in data collectors.

## Extending the Project

- Add explicit headset USB identities to the appropriate vendor module and the
  registry chain in `battery/headsets/mod.rs`.
- Add a controller-specific reader under `battery/controllers/`.
- Keep hardware I/O off the UI thread and preserve the last confirmed reading
  only across transient failures.
- Update `docs/supported-devices.md` whenever the native registry or protocol
  coverage changes.
- Prefer native Rust APIs and persistent connections over command output
  parsing.
