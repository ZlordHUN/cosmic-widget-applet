# COSMIC Player seeking

The widget's 10-second back/forward buttons and timeline use standard MPRIS seek
commands. COSMIC Player 1.10.0 at the revision below reports `CanSeek = false`
and leaves both `Seek` and `SetPosition` unimplemented. This companion patch
connects those commands to the existing GStreamer playback pipeline.

## Build the companion

The patch targets the installed package's upstream revision:

```text
900453787f999ec151b8d58683923e3110b727e1
```

In a separate COSMIC Player checkout at that revision:

```bash
git apply /path/to/cosmic-widget-applet/integrations/cosmic-player/mpris-seeking.patch
cargo test --locked --bin cosmic-player mpris
cargo build --locked --release --bin cosmic-player
sudo install -m0755 target/release/cosmic-player /usr/local/bin/cosmic-player
```

The player requires Rust 1.93 or newer and the GStreamer development libraries
used by its upstream build. Keep the upstream lockfile unchanged.

Restart COSMIC Player when ready to switch builds; installing the companion does
not modify an already running player. `/usr/local/bin` takes precedence over the
packaged `/usr/bin` through `PATH`. Rebase and rebuild this patch when updating
to a newer Player release.

## Behavior

- `CanSeek` reflects GStreamer's time-seeking capability and changes when media
  opens or closes.
- `Seek` accepts signed microsecond offsets and clamps to the current duration.
- `SetPosition` validates the current track ID and ignores negative positions or
  positions beyond the track duration.
- Track IDs change with the media URL. Queued commands recheck that URL in the
  application before touching the pipeline.
- Seeking preserves the current paused/playing state and reports the new position
  through the existing MPRIS state updates. Pending GStreamer transitions are
  respected so a playing seek cannot briefly report a false pause. Failed pipeline
  seeks are logged.

The widget's title cleanup and local video thumbnail generation work independently
of this companion patch. Other MPRIS players use their existing seek support.

The companion source patch retains COSMIC Player's GPL-3.0-only license.
