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
- Uses Wayland layer-shell protocol for true transparency
- Position is fixed at startup (set via settings)
- Displays real-time system statistics

### Settings
Open via the applet menu or launch directly:
```bash
cosmic-monitor-settings
```

Settings include:
- **Monitoring Options**: Toggle CPU, memory, network, disk monitoring
- **Display Options**: Percentages and graphs
- **Update Interval**: 100-10000ms
- **Widget Position**: Enter exact X, Y coordinates
- **Apply Position**: Restart widget to apply new position

## Configuration

Settings are stored via cosmic-config at:
```
~/.config/cosmic/com.github.zoliviragh.CosmicMonitor/v1/config
```

Configuration fields:
- `show_cpu`, `show_memory`, `show_network`, `show_disk` - Boolean toggles
- `update_interval_ms` - Update frequency (100-10000)
- `show_percentages`, `show_graphs` - Display options
- `widget_x`, `widget_y` - Widget position coordinates (pixels from top-left)
- `widget_movable` - Internal flag for drag mode

## Positioning the Widget

Since the widget uses layer-shell and can't be dragged:

1. Open Settings from the panel applet
2. Enter desired X and Y coordinates in pixels
3. Click **Apply Position**
4. Widget will restart at the new location

**Tip**: Start with values like X=100, Y=100 and adjust from there.

## Auto-start Widget

To have the widget start automatically:

1. Add to COSMIC startup applications:
   ```bash
   cosmic-monitor-widget
   ```

2. Or create a systemd user service (optional)

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
