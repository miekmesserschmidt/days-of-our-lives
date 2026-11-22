"""One-file Python SDK for days-of-our-lives plugins (standard library only).

Same model as the Rust SDK: subclass `Plugin`, implement `render(frame) -> str`, and
optionally `update(msg)` (messages you `post()` from your own threads) and
`mouse(ev, frame, out)`. Return True from them to redraw. Then call `run(...)`.
"""

import json
import os
import queue
import sys
import threading
import time
import traceback

PROTOCOL = 1
_out_lock = threading.Lock()


def send(msg):
    line = json.dumps(msg, separators=(",", ":")) + "\n"
    with _out_lock:
        sys.stdout.write(line)
        sys.stdout.flush()


def log(level, message):
    send({"type": "log", "level": level, "message": str(message)})


class Out:
    """Side effects of a mouse handler."""

    def __init__(self):
        self.tooltip_request = None  # None = no change, () = hide, (text, anchor) = show

    def tooltip(self, text, anchor=None):
        self.tooltip_request = (text, anchor)

    def hide_tooltip(self):
        self.tooltip_request = ()


class Plugin:
    def __init__(self, config, frame, post):
        self.config = config

    def update(self, msg, frame):
        return True

    def mouse(self, ev, frame, out):
        return False

    def render(self, frame):
        raise NotImplementedError


def every(seconds, post, make_msg):
    """Post `make_msg()` on every multiple of `seconds` of wall-clock time (no drift)."""

    def loop():
        while True:
            time.sleep(seconds - (time.time() % seconds) + 0.005)
            post(make_msg())

    threading.Thread(target=loop, daemon=True).start()


def resolve_path(frame, path):
    """`~` → $HOME; relative paths are relative to the instance's config directory."""
    path = os.path.expanduser(path)
    return path if os.path.isabs(path) else os.path.join(frame["config_dir"], path)


def _frame_from_init(init):
    keys = ("instance", "bar", "output", "edge", "orientation", "width", "height", "scale", "style", "config_dir")
    return {k: init.get(k) for k in keys}


def run(plugin_class, describe, default_config):
    if len(sys.argv) > 1 and sys.argv[1] == "--describe":
        print(json.dumps({"protocol": PROTOCOL, **describe}))
        sys.exit(0)
    if len(sys.argv) > 1 and sys.argv[1] == "--default-config":
        sys.stdout.write(default_config)
        sys.exit(0)

    first = sys.stdin.readline()
    if not first:
        sys.exit(0)
    init = json.loads(first)
    if init.get("type") != "init":
        log("error", f"expected init, got {first.strip()}")
        sys.exit(2)
    frame = _frame_from_init(init)
    events = queue.Queue()
    try:
        plugin = plugin_class(init.get("config") or {}, frame, lambda msg: events.put(("own", msg)))
    except Exception as e:  # config errors etc.
        log("error", f"{type(e).__name__}: {e}")
        sys.exit(2)

    def read_stdin():
        for line in sys.stdin:
            try:
                events.put(("bar", json.loads(line)))
            except ValueError as e:
                log("warn", f"bad message from bar: {e}")
        events.put(("eof", None))

    threading.Thread(target=read_stdin, daemon=True).start()

    last_svg = None
    last_tooltip = None

    def redraw():
        nonlocal last_svg
        svg = plugin.render(frame)
        if svg != last_svg:
            send({"type": "svg", "svg": svg})
            last_svg = svg

    try:
        redraw()
        while True:
            kind, msg = events.get()
            if kind == "eof":
                break
            if kind == "own":
                if plugin.update(msg, frame):
                    redraw()
                continue
            t = msg.get("type")
            if t == "resize":
                frame.update(width=msg["width"], height=msg["height"], scale=msg["scale"])
                redraw()
            elif t == "style":
                frame["style"] = {k: v for k, v in msg.items() if k != "type"}
                redraw()
            elif t in ("mouseenter", "mousemove", "mouseleave", "mousedown", "mouseup", "click", "wheel"):
                if t == "mouseleave":
                    last_tooltip = None  # the bar hid it already
                out = Out()
                changed = plugin.mouse(msg, frame, out)
                req = out.tooltip_request
                if req is not None and req != (last_tooltip or ()):
                    if req == ():
                        send({"type": "tooltip", "text": None})
                        last_tooltip = None
                    else:
                        m = {"type": "tooltip", "text": req[0]}
                        if req[1] is not None:
                            m["anchor"] = req[1]
                        send(m)
                        last_tooltip = req
                if changed:
                    redraw()
    except Exception:
        log("error", traceback.format_exc())
        sys.exit(1)
    sys.exit(0)
