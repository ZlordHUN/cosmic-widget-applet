# COSMIC Monitor Applet

A Conky-style system monitoring applet for the COSMIC desktop environment, featuring a borderless floating widget that displays real-time system statistics.

## Features

- **Panel Applet**: Integrates into COSMIC panel with a menu to toggle widget and open settings
- **Borderless Widget**: Floating overlay widget using Wayland layer-shell protocol (no window borders!)
- **Dynamic Sizing**: Widget automatically adjusts height based on enabled features
- **Clock Display**: Large time display with 12/24-hour format toggle and date with Conky-style text outlines (toggleable)
- **Weather Integration**: Real-time weather data with dynamic icons (sun, moon, clouds, rain, snow, fog, thunderstorm) from OpenWeatherMap API
- **Temperature Monitoring**: Individual CPU and GPU temperature displays with sensor detection
- **Circular Temperature Gauges**: Color-changing hollow rings for temperature visualization (switchable to text mode)
- **Transparent Background**: Fully transparent widget background for seamless desktop integration
- **Visual Indicators**: CPU, RAM, and GPU icons with gradient progress bars that change color based on usage
- **System Monitoring**: Real-time CPU, memory, GPU (NVIDIA via nvidia-smi), network, and disk I/O statistics
- **Customizable Position**: Precise X/Y positioning via settings window
- **Configurable Display**: Toggle individual stats (CPU, RAM, GPU, clock, date, temperatures), show/hide percentage values
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
# Install binaries
sudo install -Dm755 target/release/cosmic-monitor-applet /usr/bin/cosmic-monitor-applet
sudo install -Dm755 target/release/cosmic-monitor-widget /usr/bin/cosmic-monitor-widget
sudo install -Dm755 target/release/cosmic-monitor-settings /usr/local/bin/cosmic-monitor-settings

# Install desktop files
sudo install -Dm644 resources/app.desktop /usr/share/applications/com.github.zoliviragh.CosmicMonitor.desktop
sudo install -Dm644 resources/settings.desktop /usr/share/applications/com.github.zoliviragh.CosmicMonitor.Settings.desktop
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
- **Temperature Display**: Toggle CPU and GPU temperature monitoring independently, switch between circular gauges and text display
- **Widget Display**: Toggle clock (12/24-hour format) and date displays independently
- **Weather Display**: Toggle weather information, configure OpenWeatherMap API key and location
- **Display Options**: Show/hide percentage values next to progress bars
- **Update Interval**: 100-10000ms refresh rate
- **Widget Position**: Precise X/Y coordinates (requires restart to apply)

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
- **cosmic-config**: Configuration persistence
- **reqwest**: HTTP client for weather API requests
- **serde/serde_json**: JSON parsing for weather data

## Weather Setup

To enable weather display:

1. Get a free API key from [OpenWeatherMap](https://openweathermap.org/api)
2. Open Settings from the applet menu
3. Enable "Show Weather"
4. Enter your API key
5. Enter your location (city name, e.g., "London" or "New York")

Weather updates every 10 minutes and displays:
- Current temperature
- Weather description
- Location name
- Dynamic icon based on conditions (clear sky, clouds, rain, snow, fog, thunderstorm) with day/night variants

## Development

See [ARCHITECTURE.md](ARCHITECTURE.md) for detailed technical documentation and [QUICKSTART.md](QUICKSTART.md) for development setup.

## License

MPL-2.0

## Credits

Built for the [COSMIC Desktop](https://github.com/pop-os/cosmic-epoch) by System76.