<div align="center">

# COSMIC Widget

![Icon](resources/icon.svg)

*A Conky-style system monitoring widget for the COSMIC desktop environment*

[![License: MPL 2.0](https://img.shields.io/badge/License-MPL_2.0-brightgreen.svg)](https://opensource.org/licenses/MPL-2.0)

![Widget Screenshot](screenshots/Screenshot_2025-11-25_00-47-55.png)

</div>

---

A borderless floating widget that displays real-time system statistics for the COSMIC desktop environment.

## Features

- **Panel Applet**: Integrates into COSMIC panel with a menu to toggle widget and open settings
- **Borderless Widget**: Floating overlay widget using Wayland layer-shell protocol (no window borders!)
- **Dynamic Sizing**: Widget automatically adjusts height based on enabled features
- **Clock Display**: Large time display with 12/24-hour format toggle and date with Conky-style text outlines (toggleable)
- **Weather Integration**: Real-time weather data with dynamic icons (sun, moon, clouds, rain, snow, fog, thunderstorm) from Open-Meteo API (no API key required!) with day/night variants for all conditions
- **Notification Monitor**: Real-time desktop notification capture via D-Bus with smart grouping by application, expand/collapse groups, and visual containers
- **Temperature Monitoring**: Individual CPU and GPU temperature displays with sensor detection
- **Circular Temperature Gauges**: Color-changing hollow rings for temperature visualization (switchable to text mode)
- **Transparent Background**: Fully transparent widget background for seamless desktop integration
- **Visual Indicators**: CPU, RAM, and GPU icons with gradient progress bars that change color based on usage
- **System Monitoring**: Real-time CPU, memory, GPU (NVIDIA, AMD, Intel auto-detected), storage usage, network, and disk I/O statistics
- **Multi-Vendor GPU Support**: Automatic detection and monitoring for NVIDIA (nvidia-smi), AMD (sysfs/radeontop), and Intel (sysfs/intel_gpu_top) GPUs
- **Storage Monitoring**: Displays disk usage for system drives and external media with intelligent labeling (vendor + model names)
- **Battery Monitoring**: Shows battery status for Logitech wireless devices (via Solaar) and gaming headsets (via HeadsetControl) with color-coded vertical battery icons, connection status, and immediate startup rendering
- **Media Player Integration**: Multi-source media player with support for Cider (Apple Music), browser audio (YouTube thumbnails), and any MPRIS-compatible player; includes album art, playback controls, and pagination dots for switching between active players
- **Persistent Cache**: Remembers drives and peripherals to instantly display placeholders while loading fresh data
- **Customizable Position**: Precise X/Y positioning via settings window
- **Configurable Display**: Toggle individual stats (CPU, RAM, GPU, clock, date, temperatures, notifications), show/hide percentage values
- **Native COSMIC Integration**: Built with libcosmic and follows COSMIC design patterns

## Architecture

This project consists of three separate binaries:

1. **cosmic-widget-applet**: Panel applet that provides the menu interface
2. **cosmic-widget**: Borderless widget using direct Wayland layer-shell
3. **cosmic-widget-settings**: Configuration window for customizing the widget

The widget uses the Wayland layer-shell protocol directly (via smithay-client-toolkit) to bypass COSMIC's window management and achieve true borderless rendering, similar to Conky.

## Building

```bash
# Build all binaries
cargo build --release

# Or build individually
cargo build --release --bin cosmic-widget-applet
cargo build --release --bin cosmic-widget
cargo build --release --bin cosmic-widget-settings
```

## Installation

```bash
# Build all binaries
cargo build --release

# Install using just (recommended)
sudo just install

# Or install manually
sudo install -Dm755 target/release/cosmic-widget-applet /usr/local/bin/cosmic-widget-applet
sudo install -Dm755 target/release/cosmic-widget /usr/local/bin/cosmic-widget
sudo install -Dm755 target/release/cosmic-widget-settings /usr/local/bin/cosmic-widget-settings

# Install desktop files and icon
sudo install -Dm644 resources/app.desktop /usr/local/share/applications/com.github.zoliviragh.CosmicWidget.desktop
sudo install -Dm644 resources/settings.desktop /usr/local/share/applications/com.github.zoliviragh.CosmicWidget.Settings.desktop
sudo install -Dm644 resources/icon.svg /usr/local/share/icons/hicolor/scalable/apps/com.github.zoliviragh.CosmicWidget.svg

# Update desktop database and icon cache
sudo update-desktop-database /usr/local/share/applications
sudo gtk-update-icon-cache -f -t /usr/local/share/icons/hicolor
```

## Usage

1. Add the applet to your COSMIC panel
2. Click the panel icon to toggle the widget on/off
3. Use "Settings" to configure widget position and displayed stats
4. Enter X and Y coordinates for precise positioning
5. Click "Apply Position" to restart the widget at the new location

## Configuration

Settings are stored using cosmic-config at:
```
~/.config/cosmic/com.github.zoliviragh.CosmicWidget/v1/
```

Available options:
- **Monitoring**: Toggle CPU, memory, GPU, network, disk stats individually
- **Storage Display**: Toggle storage/disk usage monitoring with per-drive usage bars
- **Battery Display**: Toggle battery section and enable Solaar integration for Logitech wireless devices
- **Temperature Display**: Toggle CPU and GPU temperature monitoring independently, switch between circular gauges and text display
- **Widget Display**: Toggle clock (12/24-hour format) and date displays independently
- **Weather Display**: Toggle weather information and configure location (no API key needed - uses Open-Meteo)
- **Notification Display**: Toggle notification monitoring with grouped display by application
- **Media Display**: Toggle media player information display with multi-source support (Cider, MPRIS players like browsers, Spotify, etc.)
- **Layout Order**: Customize the order in which sections appear in the widget (Utilization, Temperatures, Storage, Battery, Weather, Notifications, Media)
- **Display Options**: Show/hide percentage values next to progress bars
- **Update Interval**: 100-10000ms refresh rate
- **Widget Position**: Precise X/Y coordinates, auto-start widget on login toggle

## Technical Details

### Why Layer-Shell?

COSMIC's compositor (cosmic-comp) adds a mandatory 10px resize border to all client-side decorated windows. To achieve a truly borderless widget like Conky, we bypass the normal window management entirely using the Wayland layer-shell protocol.

Trade-offs:
- ✅ True borderless rendering
- ✅ Persistent overlay positioning
- ❌ No interactive dragging (position set at startup)
- ❌ No COSMIC theming integration

### Dependencies

- **libcosmic**: For applet and settings UI
- **smithay-client-toolkit**: Direct Wayland layer-shell access
- **cairo-rs/pango**: Custom widget rendering with text outlines
- **chrono**: Date and time formatting
- **sysinfo**: System statistics monitoring
- **busctl**: System tool for D-Bus monitoring (notification capture)
- **solaar**: (Optional) For battery monitoring of Logitech wireless devices
- **headsetcontrol**: (Optional) For battery monitoring of gaming headsets (Audeze, SteelSeries, Logitech, HyperX, etc.)
- **cosmic-config**: Configuration persistence
- **reqwest**: HTTP client for weather API requests
- **serde/serde_json**: JSON parsing for weather data

## Weather Setup

To enable weather display:

1. Open Settings from the applet menu
2. Enable "Show Weather"
3. Enter your location (e.g., "London", "New York", "Berlin")

No API key required! Weather data is provided by [Open-Meteo](https://open-meteo.com/), a free and open-source weather API.

Weather updates every 2 minutes and displays:
- Current temperature
- Weather description
- Location name
- Dynamic icons using [Weather Icons](https://github.com/erikflowers/weather-icons) font with full day/night variants:
  - Clear sky: Sunny (day) / Moon (night)
  - Few clouds: Day cloudy (day) / Night partly cloudy (night)
  - Scattered clouds: Day sunny overcast (day) / Night partly cloudy (night)
  - Rain: Day rain (day) / Night rain (night)
  - Thunderstorm: Day thunderstorm (day) / Night thunderstorm (night)
  - Snow: Day snow (day) / Night snow (night)
  - Fog: Day fog (day) / Night fog (night)

## Battery Monitoring Setup

To enable battery monitoring for wireless peripherals:

### Logitech Devices (via Solaar)

1. Install [Solaar](https://github.com/pwr-Solaar/Solaar) if not already installed:
   ```bash
   sudo apt install solaar  # Debian/Ubuntu
   sudo dnf install solaar  # Fedora
   ```

### Gaming Headsets (via HeadsetControl)

1. Install [HeadsetControl](https://github.com/Sapd/HeadsetControl) if not already installed:
   ```bash
   sudo apt install headsetcontrol  # Debian/Ubuntu
   # Or build from source for latest device support
   ```

### Enable in Settings

1. Open Settings from the applet menu
2. Navigate to the Battery section
3. Enable "Show Battery Section"
4. Enable "Enable Solaar Integration" (monitors both Solaar and HeadsetControl)

The widget will display:
- Device names (e.g., "G309 LIGHTSPEED", "MX Mechanical Mini", "Audeze Maxwell")
- Device type icons based on kind (mouse, keyboard, headset)
- Color-coded vertical battery icons (green > 60%, yellow > 30%, orange > 15%, red ≤ 15%)
- Battery percentage next to each device
- "Disconnected" status for offline devices (e.g., mouse in sleep mode)
- "Connecting..." status while retrieving battery data
- Cached device information for instant display on startup

## Media Player Setup

The widget supports multiple media sources simultaneously with automatic detection and pagination.

### Supported Players

- **Cider** (Apple Music client) - Primary source with full API integration
- **Web Browsers** (Firefox, Chrome, etc.) - Via MPRIS D-Bus protocol
- **Desktop Players** - Spotify, Audacious, VLC, and any MPRIS-compatible player

### Album Art Support

- **Cider**: Full album art from Apple Music
- **YouTube** (in browser): Automatic thumbnail extraction from video URL
- **Other browser media**: Falls back to browser icon (Firefox, Chrome, etc.)
- **Local players**: Supports `mpris:artUrl` metadata and local file:// URLs

### Installing Cider (Optional)

[Cider](https://cider.sh/) is an open-source Apple Music client for Linux.

1. Install Cider from their website or your package manager
2. Sign in with your Apple Music account

### Enabling the Cider API (Optional)

1. Open Cider settings
2. Navigate to the Connectivity section
3. Enable "Enable WebSocket API" or "Enable Remote API"
4. If using API authentication, generate an API token

### Configuring the Widget

1. Open Settings from the applet menu
2. Navigate to the Media Player section
3. Enable "Show Media Player"
4. If using Cider with authentication, enter your API token (leave empty otherwise)

### Multi-Player Navigation

When multiple media sources are active (e.g., Cider and browser audio):
- **Pagination dots** appear at the bottom of the media panel
- **Click a dot** to switch between players
- **Playing sources** are prioritized over paused ones
- The widget remembers your selection when switching

### Playback Controls

The widget provides interactive playback controls:
- **Previous/Next** buttons to skip tracks
- **Play/Pause** button to control playback
- **Progress bar** - click to seek within the track

The widget will display:
- Track title, artist, and album
- Album art (or app icon fallback)
- Progress bar with current time and duration
- Player name (e.g., "Cider", "Firefox", "Spotify")

### Supported Devices

**Logitech** (via Solaar): Any wireless device that Solaar can detect (mice, keyboards, trackballs, etc.)

**Headsets** (via HeadsetControl):
- Audeze Maxwell (PC & Xbox variants)
- SteelSeries Arctis series (7, 9, Nova, Pro Wireless)
- Logitech G series headsets (G533, G733, G935, PRO X, etc.)
- HyperX Cloud series
- Corsair VOID series
- Roccat Elo 7.1 Air
- And many more - see [HeadsetControl device list](https://github.com/Sapd/HeadsetControl#supported-headsets)

### Managing Cached Devices

The Settings app includes a device list in the Battery section where you can remove cached devices you no longer use. Each device has a trash icon button to delete it from the cache.

## Notification Monitoring

The widget can monitor and display desktop notifications in real-time:

### Features

- **Real-time Capture**: Monitors D-Bus for all desktop notifications via `busctl`
- **Smart Grouping**: Automatically groups notifications by application (e.g., all Instagram notifications together)
- **Expand/Collapse**: Click on a group header to toggle between collapsed (▶) and expanded (▼) views
- **Visual Containers**: Each notification group has a semi-transparent background with border for clear separation
- **Recent First**: Groups are sorted by most recent notification
- **Notification Details**: Shows app name, summary, and body text (truncated if too long)
- **Persistent Display**: Keeps up to 5 notifications visible at once
- **Clear All Button**: Red "Clear All" button in the header to dismiss all notifications at once
- **Individual Dismiss**: Each notification and group has an X button to dismiss individually
- **Group Dismiss**: X button on group headers clears all notifications from that application

### Enabling Notifications

1. Open Settings from the applet menu
2. Navigate to the Notifications section
3. Enable "Show Notifications"
4. Notifications will appear automatically as they arrive

### Using Notification Groups

- **Collapsed groups** show: "▶ AppName (count)" with an X button to clear the group
- **Expanded groups** show: "▼ AppName (count)" with individual notification details
- **Left-click** a group header to toggle expand/collapse
- **Click the X button** on a group header to clear all notifications from that app
- **Click the X button** on an individual notification to dismiss just that one
- **Click "Clear All"** button in the header to dismiss all notifications

The grouping feature is especially useful when receiving multiple notifications from the same application, as it keeps the widget compact while still showing all information when needed.

## Cache

The widget caches drive and peripheral information at:
```
~/.cache/cosmic-widget-applet/widget_cache.json
```

This allows the widget to instantly display disk names and battery devices on startup while loading fresh data in the background. Storage drives show empty bars with "Loading..." and battery devices show a "Disconnected" icon until data is refreshed or device comes online.

## Development

See [ARCHITECTURE.md](ARCHITECTURE.md) for detailed technical documentation and [QUICKSTART.md](QUICKSTART.md) for development setup.

## License

MPL-2.0

## Credits

Built for the [COSMIC Desktop](https://github.com/pop-os/cosmic-epoch) by System76.

Weather icons by [Erik Flowers](https://github.com/erikflowers/weather-icons) (SIL OFL 1.1 License).