<div align="center">

# COSMIC Monitor Applet

![Icon](resources/icon.svg)

*A Conky-style system monitoring applet for the COSMIC desktop environment*

[![License: MPL 2.0](https://img.shields.io/badge/License-MPL_2.0-brightgreen.svg)](https://opensource.org/licenses/MPL-2.0)

</div>

---

A borderless floating widget that displays real-time system statistics for the COSMIC desktop environment.

## Features

- **Panel Applet**: Integrates into COSMIC panel with a menu to toggle widget and open settings
- **Borderless Widget**: Floating overlay widget using Wayland layer-shell protocol (no window borders!)
- **Dynamic Sizing**: Widget automatically adjusts height based on enabled features
- **Clock Display**: Large time display with 12/24-hour format toggle and date with Conky-style text outlines (toggleable)
- **Weather Integration**: Real-time weather data with dynamic icons (sun, moon, clouds, rain, snow, fog, thunderstorm) from OpenWeatherMap API with day/night variants for all conditions
- **Notification Monitor**: Real-time desktop notification capture via D-Bus with smart grouping by application, expand/collapse groups, and visual containers
- **Temperature Monitoring**: Individual CPU and GPU temperature displays with sensor detection
- **Circular Temperature Gauges**: Color-changing hollow rings for temperature visualization (switchable to text mode)
- **Transparent Background**: Fully transparent widget background for seamless desktop integration
- **Visual Indicators**: CPU, RAM, and GPU icons with gradient progress bars that change color based on usage
- **System Monitoring**: Real-time CPU, memory, GPU (NVIDIA, AMD, Intel auto-detected), storage usage, network, and disk I/O statistics
- **Multi-Vendor GPU Support**: Automatic detection and monitoring for NVIDIA (nvidia-smi), AMD (sysfs/radeontop), and Intel (sysfs/intel_gpu_top) GPUs
- **Storage Monitoring**: Displays disk usage for system drives and external media with intelligent labeling (vendor + model names)
- **Battery Monitoring**: Shows battery status for Logitech wireless devices (via Solaar) and gaming headsets (via HeadsetControl) with color-coded vertical battery icons, connection status, and immediate startup rendering
- **Persistent Cache**: Remembers drives and peripherals to instantly display placeholders while loading fresh data
- **Customizable Position**: Precise X/Y positioning via settings window
- **Configurable Display**: Toggle individual stats (CPU, RAM, GPU, clock, date, temperatures, notifications), show/hide percentage values
- **Native COSMIC Integration**: Built with libcosmic and follows COSMIC design patterns

## Architecture

This project consists of three separate binaries:

1. **cosmic-monitor-applet**: Panel applet that provides the menu interface
2. **cosmic-monitor-widget**: Borderless widget using direct Wayland layer-shell
3. **cosmic-monitor-settings**: Configuration window for customizing the widget

The widget uses the Wayland layer-shell protocol directly (via smithay-client-toolkit) to bypass COSMIC's window management and achieve true borderless rendering, similar to Conky.

## Building

```bash
# Build all binaries
cargo build --release

# Or build individually
cargo build --release --bin cosmic-monitor-applet
cargo build --release --bin cosmic-monitor-widget
cargo build --release --bin cosmic-monitor-settings
```

## Installation

```bash
# Build all binaries
cargo build --release

# Install using just (recommended)
sudo just install

# Or install manually
sudo install -Dm755 target/release/cosmic-monitor-applet /usr/bin/cosmic-monitor-applet
sudo install -Dm755 target/release/cosmic-monitor-widget /usr/bin/cosmic-monitor-widget
sudo install -Dm755 target/release/cosmic-monitor-settings /usr/local/bin/cosmic-monitor-settings

# Install desktop files and icon
sudo install -Dm644 resources/app.desktop /usr/share/applications/com.github.zoliviragh.CosmicMonitor.desktop
sudo install -Dm644 resources/settings.desktop /usr/share/applications/com.github.zoliviragh.CosmicMonitor.Settings.desktop
sudo install -Dm644 resources/icon.svg /usr/share/icons/hicolor/scalable/apps/com.github.zoliviragh.CosmicMonitor.svg

# Update icon cache
sudo gtk-update-icon-cache -f -t /usr/share/icons/hicolor
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
~/.config/cosmic/com.github.zoliviragh.CosmicMonitor/v1/
```

Available options:
- **Monitoring**: Toggle CPU, memory, GPU, network, disk stats individually
- **Storage Display**: Toggle storage/disk usage monitoring with per-drive usage bars
- **Battery Display**: Toggle battery section and enable Solaar integration for Logitech wireless devices
- **Temperature Display**: Toggle CPU and GPU temperature monitoring independently, switch between circular gauges and text display
- **Widget Display**: Toggle clock (12/24-hour format) and date displays independently
- **Weather Display**: Toggle weather information, configure OpenWeatherMap API key and location (includes day/night icon variants)
- **Notification Display**: Toggle notification monitoring with grouped display by application
- **Layout Order**: Customize the order in which sections appear in the widget (Utilization, Temperatures, Storage, Battery, Weather, Notifications)
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

1. Get a free API key from [OpenWeatherMap](https://openweathermap.org/api)
2. Open Settings from the applet menu
3. Enable "Show Weather"
4. Enter your API key
5. Enter your location (e.g., "London" or "New York")

Weather updates every 10 minutes and displays:
- Current temperature
- Weather description
- Location name
- Dynamic icon based on conditions with full day/night variants:
  - Clear sky: ☀ (day) / 🌙 (night)
  - Few clouds: 🌤 (day) / 🌙☁ (night)
  - Scattered clouds: ☁ (day) / ☁🌙 (night)
  - Rain: 🌦 (day) / 🌧🌙 (night)
  - Thunderstorm: ⛈ (day) / ⛈🌙 (night)
  - Snow: ❄ (day) / ❄🌙 (night)
  - Fog: 🌫 (day) / 🌫🌙 (night)

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
- **Clear Action**: Right-click anywhere in the notifications section to clear all

### Enabling Notifications

1. Open Settings from the applet menu
2. Navigate to the Notifications section
3. Enable "Show Notifications"
4. Notifications will appear automatically as they arrive

### Using Notification Groups

- **Collapsed groups** show: "▶ AppName (count)"
- **Expanded groups** show: "▼ AppName (count)" with individual notification details
- **Left-click** a group header to toggle expand/collapse
- **Right-click** anywhere in the notifications section to clear all notifications

The grouping feature is especially useful when receiving multiple notifications from the same application, as it keeps the widget compact while still showing all information when needed.

## Cache

The widget caches drive and peripheral information at:
```
~/.cache/cosmic-monitor-applet/widget_cache.json
```

This allows the widget to instantly display disk names and battery devices on startup while loading fresh data in the background. Storage drives show empty bars with "Loading..." and battery devices show a "Disconnected" icon until data is refreshed or device comes online.

## Development

See [ARCHITECTURE.md](ARCHITECTURE.md) for detailed technical documentation and [QUICKSTART.md](QUICKSTART.md) for development setup.

## License

MPL-2.0

## Credits

Built for the [COSMIC Desktop](https://github.com/pop-os/cosmic-epoch) by System76.