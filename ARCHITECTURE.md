# Cosmic Monitor Architecture

## Overview

This project implements a Conky-style system monitor for COSMIC desktop with three separate binaries:

1. **cosmic-monitor-applet** - Panel applet providing menu interface
2. **cosmic-monitor-widget** - Borderless floating widget using Wayland layer-shell
3. **cosmic-monitor-settings** - Configuration window

## Why Three Binaries?

- **Applet**: Integrates with COSMIC panel for easy access
- **Widget**: Runs independently as a borderless overlay (like Conky)
- **Settings**: Separate configuration UI (launched on-demand)

This separation allows the widget to run continuously while settings/applet can start/stop independently.

## Component Architecture

### 1. Panel Applet (`src/app.rs`, `src/main.rs`)

**Purpose**: Provide quick access menu in COSMIC panel

**Implementation**:
- Uses `libcosmic::Application` with panel applet mode
- Minimal UI - just a menu with 3 items
- Manages widget process lifecycle (spawn/kill)
- Launches settings window

**Key Features**:
- Toggle Widget: Spawns or kills `cosmic-monitor-widget` process
- Settings: Launches `cosmic-monitor-settings`
- About: Shows app information

**File**: `src/main.rs` (entry point), `src/app.rs` (logic)

### 2. Widget (`src/widget_main.rs`)

**Purpose**: Borderless floating overlay displaying system stats

**Why Not libcosmic?**
- COSMIC compositor (`cosmic-comp`) adds mandatory 10px `RESIZE_BORDER` to all windows
- No way to disable borders with libcosmic/client-side decorations
- Layer-shell protocol bypasses window management entirely

**Implementation**:
- Direct Wayland client (no libcosmic Application)
- Uses `smithay-client-toolkit` 0.19.2 for layer-shell protocol
- Custom rendering with Cairo/Pango (transparent background, text outlines)
- Clock display with chrono (HH:MM:SS + full date)
- System monitoring via `sysinfo` crate

**Architecture**:
```
MonitorWidget struct
├── Wayland state
│   ├── registry_state
│   ├── compositor_state
│   ├── layer_shell (wlr-layer-shell)
│   ├── seat_state
│   └── output_state
├── Rendering
│   ├── shm_state (shared memory)
│   ├── slot_pool (double buffering)
│   └── layer_surface (the actual surface)
└── Application state
    ├── config (Arc<Config>)
    ├── sys (System from sysinfo)
    ├── last_update (for timing)
    └── last_config_check (for polling)
```

**Layer Surface Configuration**:
- **Layer**: `TOP` (above normal windows)
- **Anchor**: `TOP | LEFT` (positioned from top-left corner)
- **Size**: 350x300 pixels (configurable via constants)
- **Margins**: `(widget_y, 0, 0, widget_x)` - positions the widget
- **Exclusive Zone**: -1 (doesn't reserve space)
- **Keyboard Interactivity**: None (click-through)
- **Background**: Fully transparent using Cairo `Operator::Source`

**Rendering Pipeline**:
1. Request buffer from shared memory pool
2. Create Cairo surface from buffer
3. Set transparent background with `Operator::Source`
4. Render clock with text outlines (stroke + fill)
5. Render CPU/RAM icons using Cairo paths
6. Render progress bars with gradient fills (green/yellow/red based on usage)
7. Render system metrics with Cairo/Pango
8. Flush Cairo surface
9. Attach buffer to Wayland surface
10. Damage and commit surface

**Config Watching**:
- Polls config file every 500ms
- Detects changes and redraws
- Does NOT update margins (requires restart)

**Clock Display**:
- Uses `chrono::Local` for current date/time
- Large HH:MM display (Ubuntu Bold 48)
- Smaller :SS display (Ubuntu Bold 28)
- Full date below clock (Ubuntu 16)
- White text with black outlines (Conky-style)
- Can be individually toggled (show_clock, show_date)

**Visual Indicators**:
- CPU icon: Chip representation with pins
- RAM icon: Memory stick with notch and chips
- GPU icon: Graphics card with fan and PCIe connector
- Progress bars: 200px wide with gradient fills
  - Green gradient (< 50% usage)
  - Yellow gradient (50-80% usage)
  - Red gradient (> 80% usage)
- Layout: Icon + Label + Bar + Percentage (if enabled)

**System Monitoring**:
- Uses `sysinfo::System` for CPU, memory, disk
- CPU: Global CPU percentage
- Memory: Used/Total bytes + percentage
- GPU: Placeholder at 0% (needs nvtop/radeontop integration)
- Network: Placeholder (needs implementation)
- Disk: Placeholder (needs implementation)

### 3. Settings (`src/settings.rs`, `src/settings_main.rs`)

**Purpose**: Configuration UI for the widget

**Implementation**:
- Uses `libcosmic::Application` (normal windowed mode)
- Reads/writes via `cosmic-config`
- Text inputs for precise positioning
- Apply Position button restarts widget

**UI Structure**:
```
Settings Window (Scrollable)
├── Monitoring Options
│   ├── Show CPU (toggle)
│   ├── Show Memory (toggle)
│   ├── Show GPU (toggle)
│   ├── Show Network (toggle)
│   └── Show Disk (toggle)
├── Widget Display
│   ├── Show Clock (toggle)
│   └── Show Date (toggle)
├── Display Options
│   ├── Show Percentages (toggle)
│   └── Update Interval (text input)
└── Widget Position
    ├── X Position (text input)
    ├── Y Position (text input)
    └── Apply Position (button → restart widget)
```

**Apply Position Logic**:
```rust
1. pkill -f cosmic-monitor-widget
2. sleep(300ms)
3. spawn /usr/bin/cosmic-monitor-widget
```

Why restart? Layer-shell margins are set at surface creation and cannot be changed at runtime.

## Configuration Flow

```
Settings Window → cosmic-config → Disk
                       ↑            ↓
                       └── Widget polls config
                                    ↓
                            Reads at startup
                            Watches for changes
```

**Config Structure** (`src/config.rs`):
```rust
pub struct Config {
    show_cpu: bool,
    show_memory: bool,
    show_network: bool,
    show_disk: bool,
    update_interval_ms: u64,
    show_percentages: bool,
    show_graphs: bool,
    widget_x: i32,         // X position from left
    widget_y: i32,         // Y position from top
    widget_movable: bool,  // Internal (for future drag mode)
}
```

**Storage**: `~/.config/cosmic/com.github.zoliviragh.CosmicMonitor/v1/config`

## Technical Challenges & Solutions

### Challenge 1: Borderless Windows

**Problem**: COSMIC compositor adds 10px `RESIZE_BORDER` to all windows
**Attempted Solutions**:
- ❌ `window_decorations(false)` - Still has border
- ❌ CSD with `set_client_side(true)` - Still has border
- ❌ Override redirect - Not available in Wayland
- ❌ Subcompositor subsurfaces - Too complex, still managed

**Solution**: Wayland layer-shell protocol
- Bypasses window management completely
- Used by bars, overlays, screen lockers
- No borders, no titlebar, no resize handles
- Compositor treats it as a layer, not a window

### Challenge 2: Widget Positioning

**Problem**: Layer-shell surfaces can't be dragged
**Reason**: Position is set via margins at creation time, not movable

**Solution**: 
- Settings window provides text inputs for X/Y coordinates
- "Apply Position" button kills and respawns widget
- New widget instance reads updated config

**Alternative Considered**:
- Interactive dragging mode (tried, failed)
- Layer-shell doesn't support grab/move operations
- Would need to recreate surface on every pixel moved (terrible)

### Challenge 3: Config Synchronization

**Problem**: Multiple processes need shared config
**Solution**: cosmic-config with polling
- Settings writes atomically
- Widget polls every 500ms
- Applet doesn't need config (just spawns processes)

## Dependencies

### Core Dependencies
- `libcosmic` (git) - For applet and settings UI
- `smithay-client-toolkit` 0.19.2 - Layer-shell protocol
- `wayland-client` 0.31 - Wayland core protocol
- `wayland-protocols` 0.32 - Protocol definitions

### Rendering
- `cairo-rs` 0.20.1 - 2D graphics with transparency
- `pango` 0.20.1 - Text layout
- `pangocairo` 0.20.1 - Cairo/Pango integration
- `chrono` 0.4 - Date/time formatting for clock display

### System Monitoring
- `sysinfo` 0.32 - CPU, memory, disk stats

### Configuration
- `cosmic-config` (from libcosmic) - Settings persistence

## Build Targets

```toml
[[bin]]
name = "cosmic-monitor-applet"
path = "src/main.rs"

[[bin]]
name = "cosmic-monitor-widget"
path = "src/widget_main.rs"

[[bin]]
name = "cosmic-monitor-settings"
path = "src/settings_main.rs"
```

## Future Enhancements

### Planned
- [ ] Actual network statistics (rx/tx bytes per second)
- [ ] Actual disk I/O statistics
- [ ] Graph visualizations (line graphs for trends)
- [ ] Customizable colors/themes
- [ ] Multiple widget instances with different configs
- [ ] Click actions (e.g., click to open system monitor)

### Under Consideration
- [ ] Different anchor positions (top-right, bottom-left, etc.)
- [ ] Transparency/opacity controls
- [ ] Font/size customization
- [ ] Animated updates (smooth transitions)

### Not Feasible
- ❌ Interactive dragging (layer-shell limitation)
- ❌ COSMIC theming integration (layer-shell is separate from COSMIC window management)
- ❌ Dynamic repositioning without restart (would need to destroy/recreate surface)

## Development Notes

### Debugging Widget
```bash
# Run with stderr output
cosmic-monitor-widget 2>&1

# Shows:
# - Widget starting with position: X=?, Y=?
# - Setting layer surface margins: top=?, left=?
```

### Debugging Settings
```bash
# Run from terminal to see button clicks
cosmic-monitor-settings 2>&1

# Shows:
# - ApplyPosition clicked! Current position: X=?, Y=?
# - pkill status: ?
# - Widget spawned with PID: ?
```

### Testing Config Changes
```bash
# Watch config file
watch -n 0.5 cat ~/.config/cosmic/com.github.zoliviragh.CosmicMonitor/v1/config

# Modify settings and see updates in real-time
```

## Files Reference

- `src/main.rs` - Applet entry point
- `src/app.rs` - Applet application logic
- `src/settings_main.rs` - Settings entry point
- `src/settings.rs` - Settings application logic
- `src/widget_main.rs` - Widget (layer-shell implementation)
- `src/config.rs` - Shared configuration structure
- `src/i18n.rs` - Localization support
- `i18n/en/cosmic_monitor_applet.ftl` - English translations
- `resources/app.desktop` - Applet desktop file
- `resources/settings.desktop` - Settings desktop file
