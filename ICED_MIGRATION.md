# Iced Overlay Migration

The production `cosmic-widget` binary continues to use the existing
smithay-client-toolkit and Cairo renderer. The experimental
`cosmic-widget-iced` binary is the replacement path and intentionally uses the
same single-instance lock so both renderers cannot create overlapping desktop
surfaces.

## Migrated

- Bottom-layer Wayland surface with saved top/left margins
- Explicitly sized preview surface that renders correctly with top-left anchoring
- COSMIC system theme and interface font
- COSMIC spacing, typography, dividers, containers, and progress bars
- Clock and date
- CPU, memory, and GPU utilization
- CPU and GPU temperatures
- Live configuration reload for migrated sections and position
- Background system sampling independent from the Iced UI thread

## Remaining

- Storage
- Battery and Solaar devices
- Weather
- Notifications and notification actions
- MPRIS/Cider media display and controls
- Drag-to-position interaction
- Content-driven surface resizing as sections are enabled and reordered
- Output selection and scale-factor validation
- Theme-change subscription without relying on the periodic UI tick

## Replacement Criteria

The experimental binary can replace `cosmic-widget` after every configured
section has feature parity, interactions have been verified on a restarted
COSMIC compositor, and the layer surface has been checked on mixed-scale and
multi-monitor layouts.
