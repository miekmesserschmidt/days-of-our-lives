# days-of-our-lives

A simple, plugin based desktop bar for [Hyprland](https://hypr.land), written in Rust
with GTK4 and gtk4-layer-shell.

- **Plugins are programs.** Anything named `dool-plugin-<name>` on your `$PATH` is a
  plugin. It draws an SVG picture and gets mouse events as JSON over stdin/stdout —
  write one in any language.
- **Bars anywhere.** Any number of bars, on any display and any screen edge (left and
  right bars are vertical), with margins and reserved space.
- **TOML for structure, CSS for looks.** Global settings in `/etc/days-of-our-lives/`,
  overridden by `~/.config/days-of-our-lives/`. Styling is real GTK CSS and reloads live.

Included plugins:

| Plugin | What it shows |
|--------|---------------|
| `clock` (Python) | an analogue clock, rendered by [py-analogue-clock](https://github.com/miekmesserschmidt/py-analogue-clock); your own SVG faces work |
| `calendar` | today's events from ICS feeds (one colour per feed) on a 24 h timeline, with a progress shade and a "now" line; hover an event for its name; horizontal or vertical |
| `workspaces` | a grid of Hyprland workspaces with their windows as tiny rectangles; click to switch |
| `windows` | an icon per window on the active workspace; click to focus |

The full design is in [design/plan.md](design/plan.md).

## Install (Nix)

```bash
nix run github:miekmesserschmidt/days-of-our-lives          # try it
nix profile install github:miekmesserschmidt/days-of-our-lives
```

On NixOS, use the module (it writes the global config to `/etc` and starts the bar as a
systemd user service):

```nix
{
  inputs.days-of-our-lives.url = "github:miekmesserschmidt/days-of-our-lives";
  # in your configuration:
  imports = [ inputs.days-of-our-lives.nixosModules.default ];
  programs.days-of-our-lives = {
    enable = true;
    settings.bars.main = { output = "*"; edge = "top"; };
    plugins = {
      clock = { plugin = "clock"; position = { section = "start"; }; };
      workspaces = { plugin = "workspaces"; position = { section = "end"; }; };
    };
    style = ''
      .bar { background-color: rgba(30, 30, 46, 0.8); }
    '';
  };
}
```

## Start

- systemd (restarts on crash): `systemctl --user enable --now days-of-our-lives`
  (the unit is in `share/systemd/user/`). Without UWSM, import Hyprland's environment
  first: `exec-once = dbus-update-activation-environment --systemd --all`.
- or in your Hyprland config: `exec-once = days-of-our-lives`

Only one bar process runs per Wayland display, so doing both is harmless.

## Configure

```text
/etc/days-of-our-lives/config.toml        ~/.config/days-of-our-lives/config.toml
/etc/days-of-our-lives/style.css          ~/.config/days-of-our-lives/style.css
/etc/days-of-our-lives/plugins.d/*.toml   ~/.config/days-of-our-lives/plugins.d/*.toml
```

User files are deep-merged over the global ones (tables key by key; other values are
replaced). A user `plugins.d/clock.toml` overrides the global `clock.toml`;
`enabled = false` switches a global bar or plugin instance off. Examples:

- [examples/etc/days-of-our-lives](examples/etc/days-of-our-lives) — a global config
  with a top bar.
- [examples/left-clock](examples/left-clock) — a 48 px wide bar on the left: clock
  (custom face `plugins.d/clock.svg`), vertical calendar, 4×4 workspace grid and window
  icons. Try it with `days-of-our-lives --config-dir examples/left-clock`.

```toml
# config.toml
[bar_defaults]
margin = 0          # 6 | [vertical, horizontal] | [top, right, bottom, left]
gap = 0             # extra space kept free between bar and windows

[bars.main]
output = "*"        # connector name (hyprctl monitors), "*" = every display
edge = "top"        # top | bottom | left | right

[bars.side]
output = "DP-1"
edge = "left"
margin = [8, 0, 8, 8]
```

```toml
# plugins.d/calendar.toml — every plugin file has a [position] block
plugin = "calendar"
refresh_seconds = 120    # reload cadence of the feeds
[[calendars]]
name = "work"
url_file = "~/.config/days-of-our-lives/secrets/work-ics-url"   # keeps the secret URL out of the config
color = "#89b4fa"
show_text = true         # false: draw this feed's events without titles

[position]
bar = ["main", "side"]   # one bar or several
section = "center"       # start | center | end
order = 0
expand = true            # take the remaining space
```

`days-of-our-lives plugins init calendar` writes a plugin's documented default config
to your config dir. `days-of-our-lives check-config` validates everything and tells
you which plugins are missing. `days-of-our-lives plugins list` shows installed plugins.

### Styling

GTK4 CSS ([reference](https://docs.gtk.org/gtk4/css-properties.html)). Selectors:

```text
.bar#<bar>  .top .bottom .left .right  .horizontal .vertical  .output-<connector>
.section.start  .section.center  .section.end
.slot#<instance>  .plugin-<plugin>  .crashed  .missing  :hover
tooltip
```

```css
.bar { background-color: rgba(30, 30, 46, 0.6); border-radius: 10px; }
.bar.horizontal { min-height: 36px; }       /* thickness */
.bar.vertical   { min-width: 56px; }
#clock { min-width: 36px; }                 /* slot size along the bar */
.slot:hover { background-color: rgba(255, 255, 255, 0.1); }
```

The bar's text colour and font are passed to plugins, so they follow your theme.

## Writing a plugin

A plugin is an executable `dool-plugin-<name>` that:

1. answers `--describe` with one JSON line, e.g.
   `{"name":"hello","description":"says hi","protocol":1,"version":"0.1.0"}`, and
   `--default-config` with an example TOML;
2. reads one JSON message per line from stdin: first `init` (size, scale, orientation,
   computed style, its config), then `resize`, `style` and mouse events
   (`mouseenter`, `mousemove`, `mousedown`, `mouseup`, `click`, `mouseleave`, `wheel`);
3. writes JSON lines to stdout: `{"type":"svg","svg":"<svg …>"}` whenever its picture
   changes, `{"type":"tooltip","text":"…"}` (or `null`), and `{"type":"log",…}`;
4. exits when stdin is closed.

Mouse positions are in your own SVG coordinates, and `targets` lists the `id`s of the
SVG elements under the pointer — give clickable things an id and you need no hit
testing code. See [plugins/examples/hello.sh](plugins/examples/hello.sh) (shell, 10
lines), [plugins/examples/mouse-echo](plugins/examples/mouse-echo) (Python, using
[plugin-sdk-python/dool_plugin.py](plugin-sdk-python/dool_plugin.py)) and the Rust SDK
in [crates/plugin-sdk](crates/plugin-sdk). The full protocol is in section 4 of the plan.

## Develop

```bash
nix develop
cargo build --workspace && cargo test --workspace && cargo clippy --workspace --all-targets
python3 -m pytest plugins/clock/tests
# try it with a throw-away config dir:
cargo run -p days-of-our-lives -- --config-dir /tmp/dool-test --log-level debug
```

Targets Hyprland 0.55 (Lua dispatcher syntax), GTK ≥ 4.12, gtk4-layer-shell ≥ 1.0.
Logs: stderr and `~/.local/state/days-of-our-lives/bar.log`.

## License

MIT
