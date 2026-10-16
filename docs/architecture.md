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
Each visible section sits in a translucent rounded card. The outer inset and
card padding preserve the section content width, and compact gaps replace the
separator lines. Cards and the Clear all pill reveal the widget's compositor
blur. Idle media shows an inset placeholder card with a muted artwork tile.
The overlay background fits the rendered content with a bottom inset matching
its sides. A layout sensor keeps compositor blur and input bounds aligned with
the actual card height, including media changes and notification animations.
Reusable controls in `src/overlay/components/` provide gauges, marquee text,
sliding transitions, and translated content. A device with a live charging
reading shows a vector copy of the COSMIC battery icon whose charge repeatedly
sweeps to full and fades back; cached readings keep the static icon.

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
| AI Usage | Local Codex quota reports, Claude Code account usage, and saved Claude Desktop reports |

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
  Sysfs readings and queued charging reports are published before querying the
  remaining devices, so one slow HID request does not delay other devices.
- `battery/events.rs` listens passively for kernel USB/HID/audio changes during
  the existing workers' sleep periods. Hotplug invalidates device discovery;
  idle polling intervals, worker count, and dependencies are unchanged.
  Events are coalesced and polling deadlines remain bounded. Unsupported event
  sockets fall back to the usual polling schedule.
- Maxwell USB charging/connection state is read from sysfs independently of its
  slower battery-percentage requests and CLI fallbacks. Timestamped observations
  prevent an older in-flight query from undoing a cable transition. The UI reads
  the latest battery snapshot directly instead of waiting for the general system
  sampler's next iteration.
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


Notification app icons and desktop-entry identities survive live capture, history
refreshes, replacements, and the on-disk cache. The overlay resolves installed
icons using that identity (including Discord conversation notifications), keeps
Instagram's embedded icon, and uses a bell for unknown sources. Icon handles are
cached with a fixed entry limit; resolution uses local files and icon themes only.
Transparent margins are removed once per asset so app icons share the same visible
size as Instagram, while rectangular logos keep their proportions.

Notification groups open and close at a fixed pixel speed (188 px per 540 ms),
independent of the number of children. Opening finishes the clipped tail as soon
as the visible segment is revealed; collapsing skips that tail before animating.
Quick reversals preserve the current visible height and speed. Clipping accounts
for partial expansion, preceding groups, taller transfer rows and the current
scroll offset, so large groups close without an offscreen delay.

Media controls and source selectors share the OS accent hover and pressed
colors. In notifications, Clear all, group expand/collapse arrows, and dismiss
buttons use that accent feedback. Notification bodies remain unhighlighted,
while Open controls keep their neutral theme styling. Clear all retains its
frosted resting surface.

### Media

Media thumbnails are center-cropped before their rounded corners are applied,
with small cached textures reused across redraws. Source badges include the app
icon, and transport controls follow the shared OS accent interaction style.
MPRIS application identity distinguishes Zen from its Firefox-compatible bus name;
resolved names are cached until the bus owner changes.

The media monitor merges several sources into a stable player list:

- MPRIS players discovered and updated over D-Bus;
- Cider through its local HTTP API;
- Emby sessions found from the local client state and queried over HTTP.

For local video files played in COSMIC Player, the displayed title separates
series, season/episode, and episode name while removing recognized release tags.
The formatter uses information in the decoded filename; it does not invent
episode metadata. COSMIC Player can advertise an audio stream label such as
"Japan" as the track title. Music and other players retain supplied titles, and
raw metadata remains available for proxy matching. Both regular and duplicated
MPRIS namespaces identify the app as COSMIC Player.

Local video frames use the asynchronous artwork queue and bounded LRU cache.
Optional FFmpeg extracts one frame, with a timeout and size limits, without
controlling the player. Failed extraction keeps the normal artwork fallback.
The full-width transport row includes ten-second backward/forward controls for
seekable media. Relative seeks clamp to the timeline and update the selected
player immediately after queueing so repeated clicks accumulate.
COSMIC Player versions that leave MPRIS seeking unimplemented need the
[`integrations/cosmic-player`](../integrations/cosmic-player/README.md) companion
patch; the widget respects each player's advertised seeking capability.

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

Codex credit reports are parsed separately from percentage windows. Validated
balances retain their decimal precision, and Unlimited is a distinct state.
Credit-only reports can update the balance without replacing quota windows.
Missing or invalid credit data is unavailable rather than an assumed zero.
The balance has its own report timestamp and 15-minute stale threshold; an
expired quota reset does not expire purchased credits. Only the general Codex
account bucket supplies the shared balance; model-specific credit buckets are
not substituted or summed.

Claude saves validated quota reports and retry deadlines in a private, atomically
replaced cache. A SHA-256 fingerprint binds each report to its access token without
storing credentials. Restarting restores matching reports before network requests,
preserving their original age and reset times. Missing or expired credentials clear
the cache; different tokens cannot restore another login's report. Temporary
request failures retry from 30 seconds with backoff, rate limits honor `Retry-After`,
and authentication rejection retries periodically instead of remaining latched for
the entire widget session.

When no available Claude Code report can be loaded, Claude also checks the local
Desktop usage report every 30 seconds. The fallback selects the latest complete
five-hour and seven-day record from a signed-in Desktop session, preserving its
original timestamp. Desktop normally saves usage every 5–15 minutes; rereading
that record does not make it newer. Reports older than 20 minutes are marked
stale, and logged-out sessions are rejected. The accepted history organization
also identifies two exact usage-response entries in Desktop's Chromium HTTP cache.
Bounded reads validate their keys, checksums, response dates, and JSON bodies;
unchanged files reuse parsed metadata. A current complete response supplies both
percentages and real reset timestamps together. A newer history reading falls
back to percentages with “Reset time unavailable”; timestamps are never guessed
or borrowed from another report. Source and report age stay in the tooltip.
Both sources describe the same account-wide Claude allowance. The monitor prefers
available Code account reports, then the current Desktop report.

`src/overlay/sections/ai_usage.rs` displays remaining allowance in equal-width
Codex and Claude cards with horizontal meters. Both cards reserve two quota rows
while loading and grow together for additional reported windows. Their rounded
meters share styling with utilization, storage, media, and transfer progress.
Provider icons, quota labels, and reset times (or their unavailable state) remain
visible; hover details explain the source, update cadence, and original age of
the report. Stale reports stay marked, and expired windows clear their fill while
waiting for a fresh report instead of assuming the allowance has refilled.
A compact ChatGPT credits row below both cards shows the shared Codex/ChatGPT
Work balance without a percentage gauge or invented reset date. The row reserves
space even while unavailable, and its tooltip preserves the complete balance and
original report age.

### Animated Weather Icons

The weather view uses the Meteocons fill artwork in `assets/weather/meteocons/`.
Original SVGs and their license are vendored at a pinned upstream revision.
`tools/render_weather_icons.py` renders their SMIL animations into transparent
PNG atlases; these generated assets are committed and embedded in the binary.
Normal builds and runtime need neither Chromium nor a network connection for icons.

`src/overlay/components/weather_icon.rs` decodes the current condition's atlas once
and caches its individual frames as RGBA image handles. Every frame draws at the
same icon bounds, avoiding software-renderer rounding jumps and large redraw
regions caused by translating a whole atlas. Its own frame clock schedules redraws
without application messages or weather polling. Offscreen/static icons do not
schedule frames, and removing the Weather section removes its animation.

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
