# Meteocons weather artwork

The SVGs in `source/` are unmodified [Meteocons](https://github.com/basmilius/meteocons)
fill icons by Bas Milius, redistributed under the [MIT license](LICENSE).

- Release: `v2.0.0`
- Commit: `fdd19749b9389f0b4e00d55584b083a7ab2deded`
- Upstream directory: [`production/fill/all/`](https://github.com/basmilius/meteocons/tree/fdd19749b9389f0b4e00d55584b083a7ab2deded/production/fill/all)
- Download URL: `https://raw.githubusercontent.com/basmilius/meteocons/fdd19749b9389f0b4e00d55584b083a7ab2deded/production/fill/all/{name}.svg`

All source SVGs use a `0 0 64 64` view box. Their animations use SVG SMIL;
`not-available.svg` is intentionally static. Native rendering uses generated
animation frames instead of requiring a browser at runtime.

| Source | Original animation durations |
| --- | --- |
| `clear-day.svg` | 45 seconds |
| `clear-night.svg` | 10 seconds |
| `partly-cloudy-day.svg` | 45 seconds |
| `partly-cloudy-night.svg` | 10 seconds |
| `overcast.svg` | 7 seconds |
| `rain.svg` | 0.7 seconds |
| `partly-cloudy-day-rain.svg` | 45 and 0.7 seconds |
| `partly-cloudy-night-rain.svg` | 10 and 0.7 seconds |
| `thunderstorms.svg` | 2 seconds |
| `snow.svg` | 4 and 9 seconds |
| `mist.svg` | 5 seconds |
| `not-available.svg` | Static |

The eight identical sun rays allow a shorter visually repeating cycle than a
full rotation. Any preparation of the generated animation frames leaves these
source SVGs unchanged.

## Generated playback assets

The PNG atlases and `manifest.json` are checked in so ordinary Cargo builds need
no additional tools. Each transparent frame is 128×128 pixels and displays in an
80px slot. Only the active condition's atlas is decoded, once when the condition
changes, into reusable RGBA frame handles. Drawing each frame at a fixed position
avoids subpixel jumps and limits redraws to the icon. Atlas dimensions stay at or
below 2048×2048 pixels. Most animations run at 20 frames per second.

To regenerate using Python 3, Pillow, websockets, and a local Chromium executable:

```sh
python3 tools/render_weather_icons.py --chromium /path/to/chromium
```

Run this from the repository root. Rendering uses an isolated temporary browser
profile and only the vendored SVGs. `manifest.json` records the original file
hashes and the timing adjustments used in the generated assets:

- Sun-only icons repeat after one eighth of the 45-second rotation (125 frames
  at 45ms), preserving their speed through the eight equivalent ray positions.
- Day/rain sun rotation changes from 45 to 56 seconds, aligning the seven-second
  visible loop with ten unchanged rain cycles.
- Night/rain moon motion changes from 10 to 9.8 seconds, aligning with fourteen
  unchanged rain cycles.
- Snowflake rotation changes from 9 to 8 seconds, aligning with two unchanged
  four-second snowfall cycles.

These small timing changes keep loops seamless and bound texture memory. All
other animation timings and all source artwork remain unchanged. The static
`not-available` fallback is rendered in light gray so its N/A lettering remains
legible against the dark overlay. The install recipe includes the upstream MIT
license under `share/licenses/cosmic-widget-applet/meteocons-LICENSE`.
