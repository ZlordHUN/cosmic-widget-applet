# Quick Start Guide

## Building

```bash
# Build all three binaries
cargo build --release

# Or build individually
cargo build --release --bin cosmic-monitor-applet
cargo build --release --bin cosmic-monitor-widget
cargo build --release --bin cosmic-monitor-settings
```

This creates three binaries:
- `target/release/cosmic-monitor-applet` - Panel applet
- `target/release/cosmic-monitor-widget` - Borderless floating widget (layer-shell)
- `target/release/cosmic-monitor-settings` - Configuration window

## Installing

```bash
# Install all binaries
sudo install -Dm755 target/release/cosmic-monitor-applet /usr/bin/cosmic-monitor-applet
sudo install -Dm755 target/release/cosmic-monitor-widget /usr/bin/cosmic-monitor-widget
sudo install -Dm755 target/release/cosmic-monitor-settings /usr/local/bin/cosmic-monitor-settings

# Install desktop files
sudo install -Dm644 resources/app.desktop /usr/share/applications/com.github.zoliviragh.CosmicMonitor.desktop
sudo install -Dm644 resources/settings.desktop /usr/share/applications/com.github.zoliviragh.CosmicMonitor.Settings.desktop
```

## Running

### Panel Applet
Add the applet to your COSMIC panel through the panel configuration. The applet provides a menu with:
- **Toggle Widget** - Show/hide the monitoring widget
- **Settings** - Open configuration window
- **About** - App information

### Widget (Borderless Overlay)
The widget is controlled via the panel applet menu. It can also be launched directly:
```bash
cosmic-monitor-widget &
```

The widget:
- Renders as a borderless overlay (no window decorations)
- Uses Wayland layer-shell protocol with fully transparent background
- Displays large clock with date (Conky-style with text outlines)
- Shows CPU and RAM with icons and gradient progress bars
- Shows real-time system statistics (CPU, memory, network placeholders)
- Position is fixed at startup (set via settings)

### Settings
Open via the applet menu or launch directly:
```bash
cosmic-monitor-settings
```

Settings include:
- **Monitoring Options**: Toggle CPU, memory, GPU, network, disk monitoring
- **Storage Display**: Toggle storage/disk usage monitoring
- **Battery Display**: Toggle battery section and enable Solaar integration
- **Temperature Display**: Toggle CPU and GPU temperature displays, switch between circular gauges and text
- **Widget Display**: Toggle clock and date displays, 12/24-hour time format
- **Weather Display**: Configure OpenWeatherMap API key and location
- **Display Options**: Percentages toggle and update interval
- **Layout Order**: Customize section ordering (Utilization, Temperatures, Storage, Weather)
- **Widget Position**: Enter exact X, Y coordinates
- **Apply Position**: Restart widget to apply new position

## Configuration

Settings are stored via cosmic-config at:
```
~/.config/cosmic/com.github.zoliviragh.CosmicMonitor/v1/config
```

Cache is stored at:
```
~/.cache/cosmic-monitor-applet/widget_cache.json
```

The cache stores:
- Disk names and mount points for instant display on startup
- Battery device names and kinds for instant display on startup
- Updated after first successful data fetch

Configuration fields:
- `show_cpu`, `show_memory`, `show_gpu`, `show_network`, `show_disk` - Boolean toggles for system stats
- `show_storage` - Toggle storage/disk usage monitoring
- `show_battery` - Toggle battery section display
- `enable_solaar_integration` - Enable Solaar for Logitech device battery monitoring
- `show_cpu_temp`, `show_gpu_temp` - Toggle temperature displays
- `use_circular_temp_display` - Switch between circular gauges and text for temperatures
- `show_clock`, `show_date` - Toggle clock and date displays
- `use_24hour_time` - 12/24-hour time format
- `show_weather` - Toggle weather display
- `weather_api_key`, `weather_location` - OpenWeatherMap configuration
- `update_interval_ms` - Update frequency (100-10000)
- `show_percentages` - Display percentage values
- `section_order` - Customizable ordering of widget sections
- `widget_x`, `widget_y` - Widget position coordinates (pixels from top-left)
- `widget_autostart` - Auto-start widget on login
- `widget_movable` - Internal flag for drag mode

## Positioning the Widget

Since the widget uses layer-shell and can't be dragged:

1. Open Settings from the panel applet
2. Enter desired X and Y coordinates in pixels
3. Click **Apply Position**
4. Widget will restart at the new location

**Tip**: Start with values like X=100, Y=100 and adjust from there.

## Auto-start Widget

The widget can auto-start automatically when the applet loads. This is controlled by the `widget_autostart` setting (enabled by default).

To disable auto-start, you would need to manually edit the config file and set `widget_autostart = false`.

Alternatively, to have the widget start with the system independently:

1. Add to COSMIC startup applications:
   ```bash
   cosmic-monitor-widget
   ```

2. Or create a systemd user service (optional)

## Battery Monitoring Setup

To enable battery monitoring for Logitech wireless devices:

1. Install Solaar if not already installed:
   ```bash
   sudo apt install solaar  # Debian/Ubuntu
   sudo dnf install solaar  # Fedora
   ```

2. Open Settings from the applet menu
3. Navigate to the Battery section
4. Enable "Show Battery Section"
5. Enable "Enable Solaar Integration"

The widget will display battery status for all detected Logitech wireless devices with color-coded icons and percentages.

## Troubleshooting

### Widget not appearing
- Check if it's running: `ps aux | grep cosmic-monitor-widget`
- Try launching from terminal to see errors: `cosmic-monitor-widget`
- Make sure it's installed to `/usr/bin/`

### Widget positioned off-screen
- Open settings and enter coordinates like X=50, Y=50
- Click Apply Position to restart widget

### Settings not saving
- Check permissions on `~/.config/cosmic/`
- Verify cosmic-config is working

### Widget shows in wrong position
- Layer-shell anchors to TOP-LEFT corner
- Position must be positive values (negative values don't work correctly)
- Click Apply Position after changing coordinates

### Statistics showing zero
- Wait one update interval for first measurement
- Check that sysinfo has permissions (usually not needed)

### Battery section not showing devices
- Make sure Solaar is installed: `which solaar`
- Check Solaar can detect devices: `solaar show`
- Verify both toggles are enabled in Settings (Show Battery Section and Enable Solaar Integration)
- Wait 30 seconds for first battery data fetch

### Widget startup slow
- First startup loads fresh data which takes a few seconds
- After first run, cache is created and subsequent startups are instant
- Cache location: `~/.cache/cosmic-monitor-applet/widget_cache.json`

## Development

```bash
# Run applet in development
cargo run --bin cosmic-monitor-applet

# Run widget with debug output
cargo run --bin cosmic-monitor-widget 2>&1

# Run settings
cargo run --bin cosmic-monitor-settings

# Watch for changes and rebuild
cargo watch -x 'build --release'
```

Debug output shows:
- Widget startup position
- Layer surface margin values
- Settings button clicks and position changes
