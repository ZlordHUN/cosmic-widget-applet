#!/usr/bin/env python3
"""Bake vendored Meteocons SMIL animations into PNG atlases for native Iced playback.

Requires Chromium, Pillow, and the Python websockets package. This is a developer
asset-generation tool; building and running the widget do not require them.
"""

import argparse
import asyncio
import base64
import hashlib
import io
import json
import math
from pathlib import Path
import subprocess
import tempfile
import xml.etree.ElementTree as ET

from PIL import Image
import websockets

ROOT = Path(__file__).resolve().parents[1]
ASSETS = ROOT / "assets/weather/meteocons"
FRAME_SIZE = 128
DEFAULT_FRAME_MS = 50
UPSTREAM = "fdd19749b9389f0b4e00d55584b083a7ab2deded"
# Duration, frame interval, and minimal duration substitutions for bounded loops.
# The eight identical sun rays repeat visually once every eighth revolution.
ICONS = {
    "clear-day": (5625, 45, {}),
    "clear-night": (10000, 50, {}),
    "partly-cloudy-day": (5625, 45, {}),
    "partly-cloudy-night": (10000, 50, {}),
    "overcast": (7000, 50, {}),
    "rain": (700, 50, {}),
    "partly-cloudy-day-rain": (7000, 50, {"45s": "56s"}),
    "partly-cloudy-night-rain": (9800, 50, {"10s": "9.8s"}),
    "thunderstorms": (2000, 50, {}),
    "snow": (8000, 50, {"9s": "8s"}),
    "mist": (5000, 50, {}),
    "not-available": (50, 50, {}),
}


class DevTools:
    def __init__(self, connection):
        self.connection = connection
        self.counter = 0
        self.session = None

    async def call(self, method, params=None, browser=False):
        self.counter += 1
        request = {"id": self.counter, "method": method, "params": params or {}}
        if self.session and not browser:
            request["sessionId"] = self.session
        await self.connection.send(json.dumps(request))
        while True:
            response = json.loads(await asyncio.wait_for(self.connection.recv(), 30))
            if response.get("id") == self.counter:
                if "error" in response:
                    raise RuntimeError(response["error"])
                result = response.get("result", {})
                if "exceptionDetails" in result:
                    raise RuntimeError(result["exceptionDetails"])
                return result


def svg_document(name, duration_changes):
    ET.register_namespace("", "http://www.w3.org/2000/svg")
    ET.register_namespace("xlink", "http://www.w3.org/1999/xlink")
    svg = ET.parse(ASSETS / "source" / f"{name}.svg").getroot()
    svg.set("width", str(FRAME_SIZE))
    svg.set("height", str(FRAME_SIZE))
    for node in svg.iter():
        duration = node.get("dur")
        if duration in duration_changes:
            node.set("dur", duration_changes[duration])
    return (
        '<!doctype html><html><head><style>html,body{margin:0;background:transparent;}'
        'svg{display:block}</style></head><body>'
        + ET.tostring(svg, encoding="unicode")
        + "</body></html>"
    )


async def render(chromium):
    with tempfile.TemporaryDirectory(prefix="meteocons-render-") as profile:
        # A separate temporary profile never opens the user's browser session.
        process = subprocess.Popen([
            chromium, "--headless", "--disable-gpu", "--no-first-run",
            "--no-default-browser-check", "--disable-background-networking",
            "--hide-scrollbars", "--remote-debugging-port=0",
            f"--user-data-dir={profile}", "about:blank",
        ], stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL)
        try:
            port_file = Path(profile) / "DevToolsActivePort"
            for _ in range(100):
                if port_file.exists():
                    break
                if process.poll() is not None:
                    raise RuntimeError("Chromium exited before its debugging endpoint was ready")
                await asyncio.sleep(0.1)
            else:
                raise TimeoutError("Chromium did not start within 10 seconds")
            port, browser_path = port_file.read_text().splitlines()[:2]
            async with websockets.connect(f"ws://127.0.0.1:{port}{browser_path}", max_size=16*1024*1024) as connection:
                cdp = DevTools(connection)
                target = await cdp.call("Target.createTarget", {"url": "about:blank"}, browser=True)
                session = await cdp.call("Target.attachToTarget", {"targetId": target["targetId"], "flatten": True}, browser=True)
                cdp.session = session["sessionId"]
                await cdp.call("Page.enable")
                await cdp.call("Emulation.setDeviceMetricsOverride", {
                    "width": FRAME_SIZE, "height": FRAME_SIZE, "deviceScaleFactor": 1, "mobile": False,
                })
                await cdp.call("Emulation.setDefaultBackgroundColorOverride", {"color": {"r": 0, "g": 0, "b": 0, "a": 0}})
                frame_id = (await cdp.call("Page.getFrameTree"))["frameTree"]["frame"]["id"]
                manifest = {"upstream_revision": UPSTREAM, "frame_size": FRAME_SIZE, "frame_ms": DEFAULT_FRAME_MS, "icons": {}}
                for name, (loop_ms, frame_ms, retiming) in ICONS.items():
                    frames = loop_ms // frame_ms
                    columns = math.ceil(math.sqrt(frames))
                    rows = math.ceil(frames / columns)
                    assert max(columns, rows) * FRAME_SIZE <= 2048
                    atlas = Image.new("RGBA", (columns * FRAME_SIZE, rows * FRAME_SIZE))
                    await cdp.call("Page.setDocumentContent", {"frameId": frame_id, "html": svg_document(name, retiming)})
                    source = ET.parse(ASSETS / "source" / f"{name}.svg").getroot()
                    expected_links = sum("{http://www.w3.org/1999/xlink}href" in node.attrib for node in source.iter())
                    actual_links = await cdp.call("Runtime.evaluate", {"expression": "Array.from(document.querySelector('svg').querySelectorAll('*')).filter(node => node.hasAttributeNS('http://www.w3.org/1999/xlink', 'href')).length"})
                    assert actual_links["result"]["value"] == expected_links, f"{name}: SVG gradient references were lost"
                    await cdp.call("Runtime.evaluate", {"expression": "document.querySelector('svg').pauseAnimations()"})
                    for index in range(frames):
                        # Negative begin offsets in the originals preserve their phase at t=0.
                        await cdp.call("Runtime.evaluate", {"expression": f"document.querySelector('svg').setCurrentTime({index * frame_ms / 1000})"})
                        screenshot = await cdp.call("Page.captureScreenshot", {"format": "png", "captureBeyondViewport": False, "clip": {"x": 0, "y": 0, "width": FRAME_SIZE, "height": FRAME_SIZE, "scale": 1}})
                        image = Image.open(io.BytesIO(base64.b64decode(screenshot["data"]))).convert("RGBA")
                        if name == "not-available":
                            # The upstream dark N/A lettering needs contrast on the overlay.
                            alpha = image.getchannel("A")
                            image = Image.new("RGBA", image.size, (230, 239, 252, 0))
                            image.putalpha(alpha)
                        atlas.paste(image, ((index % columns) * FRAME_SIZE, (index // columns) * FRAME_SIZE))
                    atlas.save(ASSETS / f"{name}.png", optimize=True)
                    manifest["icons"][name] = {
                        "columns": columns, "rows": rows, "frames": frames, "frame_ms": frame_ms,
                        "source_sha256": hashlib.sha256((ASSETS / "source" / f"{name}.svg").read_bytes()).hexdigest(),
                        "duration_changes": retiming,
                    }
                    print(f"Rendered {name}: {frames} frames, {atlas.width}x{atlas.height}", flush=True)
                (ASSETS / "manifest.json").write_text(json.dumps(manifest, indent=2) + "\n")
        finally:
            process.terminate()
            try:
                process.wait(timeout=5)
            except subprocess.TimeoutExpired:
                process.kill()
                process.wait()


if __name__ == "__main__":
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--chromium", required=True, help="Path to a Chromium executable")
    args = parser.parse_args()
    asyncio.run(render(args.chromium))
