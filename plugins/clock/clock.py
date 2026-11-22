#!/usr/bin/env python3
"""dool-plugin-clock: an analogue clock rendered by py-analogue-clock."""

import os
import re
import sys
from datetime import datetime

here = os.path.dirname(os.path.realpath(__file__))
sys.path.insert(0, here)  # installed: dool_plugin.py sits next to this file
sys.path.insert(0, os.path.join(here, "..", "..", "plugin-sdk-python"))  # in the repository

import dool_plugin  # noqa: E402
from analogue_clock import AnalogueClock  # noqa: E402

DEFAULT_CONFIG = """\
plugin = "clock"
# Face SVG with ids hour-hand, minute-hand and optionally second-hand.
# Relative paths are relative to this file; "" = the library's default face.
face = ""
# "auto" = 1 if the face has a second hand, else 60; or a number of seconds.
update_interval_seconds = "auto"
# IANA time zone, e.g. "Africa/Johannesburg"; "" = local time.
timezone = ""
# strftime format of the hover tooltip; "" = no tooltip.
tooltip_format = "%A, %d %B %Y"

[position]
bar = "main"
section = "start"
order = 0
"""


def bundled_face():
    """The face shipped with the plugin, if installed (share/…/plugins/clock/faces/bar.svg)."""
    for base in (os.path.join(here, "..", "..", "share", "days-of-our-lives", "plugins", "clock"), here):
        path = os.path.join(base, "faces", "bar.svg")
        if os.path.exists(path):
            return path
    return None


def with_viewbox(svg):
    """Make sure the face scales exactly: add a viewBox from width/height if missing."""
    head = svg[: svg.find(">") + 1]
    if "viewBox" in head:
        return svg
    w = re.search(r'\bwidth="([\d.]+)', head)
    h = re.search(r'\bheight="([\d.]+)', head)
    if not (w and h):
        return svg
    return svg.replace("<svg", f'<svg viewBox="0 0 {w.group(1)} {h.group(1)}"', 1)


class Clock(dool_plugin.Plugin):
    def __init__(self, config, frame, post):
        unknown = set(config) - {"face", "update_interval_seconds", "timezone", "tooltip_format"}
        if unknown:
            dool_plugin.log("warn", f"unknown config keys ignored: {sorted(unknown)}")
        self.tz = None
        if config.get("timezone"):
            from zoneinfo import ZoneInfo

            self.tz = ZoneInfo(config["timezone"])  # raises on an unknown zone → config error
        self.tooltip_format = config.get("tooltip_format", "%A, %d %B %Y")
        self.clock = self.load_face(config.get("face", ""), frame)
        has_seconds = 'id="second-hand"' in self.clock.svg or "id='second-hand'" in self.clock.svg
        interval = config.get("update_interval_seconds", "auto")
        if interval == "auto":
            interval = 1 if has_seconds else 60
        if not isinstance(interval, (int, float)) or interval <= 0:
            raise ValueError(f'update_interval_seconds: expected "auto" or a positive number, got {interval!r}')
        self.now = self.read_clock()
        dool_plugin.every(interval, post, lambda: "tick")

    def load_face(self, face, frame):
        path = dool_plugin.resolve_path(frame, face) if face else bundled_face()
        if path:
            try:
                with open(path) as f:
                    return AnalogueClock(svg=f.read())
            except (OSError, ValueError) as e:
                dool_plugin.log("error", f"face {path}: {e}; using the default face")
        return AnalogueClock()

    def read_clock(self):
        return datetime.now(self.tz)

    def update(self, msg, frame):
        self.now = self.read_clock()
        return True

    def mouse(self, ev, frame, out):
        if self.tooltip_format and ev["type"] in ("mouseenter", "mousemove"):
            out.tooltip(self.read_clock().strftime(self.tooltip_format))
        return False

    def render(self, frame):
        return with_viewbox(self.clock.generate(self.now.time().replace(microsecond=0)))


if __name__ == "__main__":
    dool_plugin.run(
        Clock,
        {"name": "clock", "description": "analogue clock (py-analogue-clock)", "version": "0.1.0"},
        DEFAULT_CONFIG,
    )
