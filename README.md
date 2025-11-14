# COSMIC Monitor Applet

A Conky-style system monitoring applet for the COSMIC desktop environment, featuring a borderless floating widget that displays real-time system statistics.

## Features

- **Panel Applet**: Integrates into COSMIC panel with a menu to toggle widget and open settings
- **Borderless Widget**: Floating overlay widget using Wayland layer-shell protocol (no window borders!)
- **Clock Display**: Large time (HH:MM:SS) and date display with Conky-style text outlines (toggleable)
- **Transparent Background**: Fully transparent widget background for seamless desktop integration
- **Visual Indicators**: CPU, RAM, and GPU icons with gradient progress bars that change color based on usage
- **System Monitoring**: Real-time CPU, memory, GPU (placeholder), network, and disk I/O statistics
- **Customizable Position**: Precise X/Y positioning via settings window
- **Configurable Display**: Toggle individual stats (CPU, RAM, GPU, clock, date), show/hide percentage values
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
- **Widget Display**: Toggle clock and date displays independently
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

## Development

See [ARCHITECTURE.md](ARCHITECTURE.md) for detailed technical documentation and [QUICKSTART.md](QUICKSTART.md) for development setup.

## License

MPL-2.0

## Credits

Built for the [COSMIC Desktop](https://github.com/pop-os/cosmic-epoch) by System76.