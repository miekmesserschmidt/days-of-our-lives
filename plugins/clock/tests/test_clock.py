import json
import os
import subprocess
import sys
from datetime import datetime

HERE = os.path.dirname(os.path.abspath(__file__))
CLOCK = os.path.join(HERE, "..", "clock.py")
sys.path.insert(0, os.path.join(HERE, ".."))
import clock  # noqa: E402


def frame(config_dir="/tmp"):
    return {"config_dir": config_dir, "style": {}, "width": 32, "height": 32}


def make(config, config_dir="/tmp"):
    posted = []
    c = clock.Clock(config, frame(config_dir), posted.append)
    return c


def test_hands_rotate_for_the_time():
    c = make({"timezone": "UTC"})
    c.now = datetime(2026, 1, 1, 3, 0, 0)
    svg = c.render(frame())
    assert svg.startswith("<svg") and "viewBox" in svg
    assert "rotate(90" in svg  # hour hand at three o'clock


def test_auto_interval_follows_the_second_hand(tmp_path):
    face = tmp_path / "face.svg"
    face.write_text(
        '<svg xmlns="http://www.w3.org/2000/svg" width="10" height="10">'
        '<line id="hour-hand" x1="5" y1="5" x2="5" y2="2"/><line id="minute-hand" x1="5" y1="5" x2="5" y2="1"/></svg>'
    )
    c = make({"face": "face.svg"}, str(tmp_path))  # relative to config_dir
    assert 'id="second-hand"' not in c.clock.svg
    assert 'viewBox="0 0 10 10"' in c.render(frame())


def test_viewbox_is_added_after_an_xml_declaration():
    svg = '<?xml version="1.0"?>\n<svg\n   width="300"\n   height="300" xmlns="http://www.w3.org/2000/svg"></svg>'
    assert clock.with_viewbox(svg) == '<?xml version="1.0"?>\n<svg viewBox="0 0 300 300"\n   width="300"\n   height="300" xmlns="http://www.w3.org/2000/svg"></svg>'


def test_broken_face_falls_back_to_default(tmp_path, capsys):
    (tmp_path / "bad.svg").write_text("<svg/>")
    c = make({"face": str(tmp_path / "bad.svg")})
    assert "hour-hand" in c.clock.svg
    assert "using the default face" in capsys.readouterr().out


def test_protocol_end_to_end():
    init = {
        "type": "init", "protocol": 1, "instance": "clock", "bar": "main", "output": "", "edge": "top",
        "orientation": "horizontal", "width": 32, "height": 32, "scale": 1, "style": {},
        "config": {"timezone": "UTC"}, "config_dir": "/tmp",
    }
    out = subprocess.run(
        [sys.executable, CLOCK],
        input=json.dumps(init) + "\n" + json.dumps({"type": "mouseenter", "x": 1, "y": 1}) + "\n",
        capture_output=True, text=True, timeout=10,
    ).stdout.splitlines()
    msgs = [json.loads(l) for l in out]
    assert msgs[0]["type"] == "svg" and "hour-hand" in msgs[0]["svg"]
    assert any(m["type"] == "tooltip" and m["text"] for m in msgs)
