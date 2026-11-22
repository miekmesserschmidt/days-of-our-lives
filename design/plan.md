# days-of-our-lives — implementation plan

Source: [design.md](design.md), [plugins.md](plugins.md).

Guiding rule from the design: **super simple to understand.** Every choice below
favours few moving parts, plain text formats and small files over cleverness.

---

## 1. Architecture in one paragraph

`days-of-our-lives` is a Rust program built on **gpui** (Zed's GPU-accelerated UI
framework, used from crates.io) that opens one or more **bar windows** on Hyprland —
ordinary Wayland windows that Hyprland floats, pins to every workspace, places and
reserves screen space for, driven by the bar over Hyprland's IPC. Each bar is placed by
**display** (monitor) and **screen edge** (top, bottom, left or right); left/right
bars are vertical. A bar does not know how to draw anything itself.
It finds **plugins** — ordinary executables named `dool-plugin-<name>` on
`$PATH` — starts one process per plugin instance per bar, and talks to each
over stdin/stdout using one JSON object per line. Plugins answer with **SVG
frames**; the bar renders each SVG (with `resvg`) into that plugin's slot.
All mouse input on a slot (move, down, up, click, wheel, enter/leave) is forwarded
to the plugin as JSON, in the plugin's own SVG coordinates, and a plugin can ask the
bar to show a tooltip. That is the whole system.

```
┌────────────── bar (gpui window, placed by Hyprland rules + IPC) ─────────┐
│ [clock slot]   [calendar slot ...............]   [workspaces] [windows] │
└──────┬──────────────────┬─────────────────────────────┬────────────┬─────┘
       │ stdin: init/resize/style/mouse… (JSON lines)   │            │
       ▼ stdout: svg / tooltip / log                    ▼            ▼
 dool-plugin-clock   dool-plugin-calendar   dool-plugin-workspaces  dool-plugin-windows
   (Python)              (Rust)                  (Rust)                (Rust)
```

Why this design:

- **Discoverable when installed:** installing a plugin = putting an executable on
  `$PATH` (works naturally with `nix profile install`, distro packages, `cargo install`).
- **Language-agnostic:** the analog clock plugin can use the Python
  `py-analogue-clock` library directly, no FFI.
- **One rendering path:** everything is SVG, so the bar has exactly one drawing routine.
- **Crash isolation:** a broken plugin cannot take the bar down; it is just restarted.

---

## 2. Repository layout (Cargo workspace)

```
Cargo.toml                    # workspace
crates/
  bar/                        # binary: days-of-our-lives
    src/main.rs               # CLI + startup
    src/config.rs             # load + merge TOML (global, user, plugins.d)
    src/discovery.rs          # find dool-plugin-* on $PATH
    src/plugin_host.rs        # spawn process, read/write JSON lines, restart
    src/bars.rs               # one Bar per (bar config × matching display), hotplug
    src/bar_view.rs           # gpui window + Render impl: sections, slots
    src/input.rs              # mouse events -> protocol messages (SVG units, targets, click detection)
    src/tooltip.rs            # tooltip window (a small placed bar-like window)
    src/hypr_place.rs         # Hyprland window rules, window geometry, reserved space
    src/style.rs              # CSS subset: parse, cascade, -> gpui StyleRefinement, live reload
    src/render.rs             # SVG -> gpui RenderImage via resvg/tiny-skia; fit transform, hit_test
  plugin-sdk/                 # crate `dool-plugin-sdk`, used by the bar AND the Rust plugins
    src/protocol.rs           # serde message types — the single definition of the protocol
    src/run.rs                # Plugin trait + run() event loop, --describe/--default-config
    src/svg.rs                # tiny SVG string builder + escaping
    src/hypr.rs               # Hyprland IPC: request socket + event socket (feature "hypr")
    src/tick.rs               # wall-clock aligned timers (every second / minute)
  plugin-sdk-python/
    dool_plugin.py            # one-file Python equivalent of run() (for the clock & 3rd parties)
plugins/
  clock/                      # Python: dool-plugin-clock
  calendar/                   # Rust:   dool-plugin-calendar
  workspaces/                 # Rust:   dool-plugin-workspaces
  windows/                    # Rust:   dool-plugin-windows
  examples/hello.sh           # 10-line shell plugin used in docs and tests
  examples/mouse-echo         # debug plugin: logs every mouse message, draws id'd hit areas
examples/
  etc/days-of-our-lives/config.toml
  etc/days-of-our-lives/style.css
  etc/days-of-our-lives/plugins.d/*.toml
flake.nix                     # builds bar + all plugins, devShell, nixosModules.default
nix/module.nix                # NixOS module (7.2)
systemd/days-of-our-lives.service   # user unit (7.3)
README.md
```

Crates (keep the list short): `gpui`, `resvg`, `image`, `simplecss`,
`csscolorparser`, `async-channel`, `serde`, `serde_json`, `toml`, `anyhow`, `dirs`,
`log`. Plugins add only what they need (see each section).

**gpui dependency:** the **crates.io** release only — no git dependency on the Zed
repository:

```toml
[workspace.dependencies]
gpui = { version = "=0.2.2", default-features = false, features = ["wayland", "font-kit"] }
```

gpui 0.2.2 has **no** layer-shell support, no parent-anchored popups, no display
observer, and ignores `display_id` on Wayland. The plan therefore uses only basic
gpui features and lets **Hyprland** do window placement (section 5, `hypr_place.rs`).
Verified in the 0.2.2 sources: `Application::new().run(..)`,
`WindowBackgroundAppearance::Transparent`, `WindowOptions::app_id`,
`TitlebarOptions::title` / `Window::set_window_title`, `Window::resize`,
`Window::scale_factor`, `Window::drop_image`, `ImageSource::Render(Arc<RenderImage>)`,
`div` mouse handlers (`on_hover`, `on_mouse_move`, `on_any_mouse_down`, `on_mouse_up`,
`on_mouse_up_out`, `on_scroll_wheel`) and `cx.background_executor()`. Upgrading to a
newer crates.io gpui is a deliberate, separate step. Only the bar depends on gpui;
plugins don't.

---

## 3. Configuration

### 3.1 Files and precedence

| What              | Global (system)                                   | User override                                      |
|-------------------|---------------------------------------------------|----------------------------------------------------|
| Bar settings      | `/etc/days-of-our-lives/config.toml`              | `~/.config/days-of-our-lives/config.toml`          |
| Styling (CSS)     | `/etc/days-of-our-lives/style.css`                | `~/.config/days-of-our-lives/style.css`            |
| Plugin instances  | `/etc/days-of-our-lives/plugins.d/<instance>.toml`| `~/.config/days-of-our-lives/plugins.d/<instance>.toml` |

**Split of responsibilities** (one place for each kind of setting):

- **TOML = structure**: which bars exist, on which display/edge, how much screen
  space they reserve (outer margins), which plugins sit on which bar, in what order,
  plus plugin behaviour (ICS URLs, grid sizes, …).
- **CSS = looks**: thickness, sizes, transparency, colours, spacing, inner
  margins/padding, borders, rounded corners, fonts, hover effects (see 3.4).

Rules (implemented in `config.rs`, ~100 lines):

1. Load global file, load user file, **deep-merge** tables (user wins; arrays
   and scalars are replaced, tables are merged key-by-key).
2. Plugin instances are keyed by file name (`clock.toml` → instance `clock`).
   A user file with the same name is deep-merged over the global one; a new
   name adds an instance. `enabled = false` disables a global instance.
3. Missing files are fine; invalid TOML is a clear error naming the file.
4. `--config-dir <path>` CLI flag replaces the user dir (handy for testing).
5. `$XDG_CONFIG_HOME` is respected for the user dir.

### 3.2 Bar config (`config.toml`)

Any number of bars can be defined. Bars are **named tables** (`[bars.<name>]`)
rather than an array, so a user file can override a single key of a global bar
(`[bars.main] edge = "bottom"`) through the normal deep-merge. A `[bar_defaults]`
table supplies values shared by all bars.

```toml
plugin_path = []          # extra dirs searched before $PATH
log_level = "info"        # debug | info | warn | error (see 7.4)
hypr_rules = "auto"       # "auto": register the bar's Hyprland window rules over IPC at startup
                          # "manual": you paste `days-of-our-lives hypr-rules` output into hyprland.conf

[bar_defaults]
exclusive = true          # reserve space so windows don't overlap the bar
tooltip_delay_ms = 400    # hover time before a plugin's tooltip appears (4.8)
margin = 0                # px between bar and screen edges, CSS order:
                          #   6 | [6, 12] (vertical, horizontal) | [top, right, bottom, left]
gap = 0                   # extra px reserved between the bar and windows

[bars.main]
output = "DP-1"           # display: connector name, "*" = a copy on every display,
                          # "" = the compositor's default/focused display
edge = "top"              # top | bottom | left | right

[bars.side]
output = "*"              # one copy of this bar on every connected display
edge = "left"             # vertical bar
margin = [8, 0, 8, 8]     # floating: 8px from top, bottom and left screen edges
gap = 4                   # windows start 4px right of the bar

[bars.laptop]
output = "eDP-1"
edge = "bottom"
enabled = false           # disable a bar defined globally
```

- Bar thickness, background, transparency etc. are **not** in TOML — they are CSS
  (e.g. `bar#side { width: 64px; }`), see 3.4.
- **Reserved space** (all in logical px, scaled for HiDPI by the compositor):

  ```
  screen edge ─┬─ margin[edge]      empty, reserved
               ├─ thickness (CSS)   the bar
               ├─ gap               empty, reserved
  windows ─────┘
  ```

  - The bar computes its own window rectangle from the monitor's logical bounds
    (Hyprland `j/monitors`): `margin` is in CSS order top/right/bottom/left. The
    margin on the bar's edge moves it away from the screen edge; margins on the two
    perpendicular edges shorten the bar (e.g. `[0, 200, 0, 200]` on a top bar → a
    centred bar 400px shorter than the screen). The margin on the opposite edge has
    no effect.
  - Reserved space on that edge = `margin[edge] + thickness + gap`. It is applied
    with Hyprland's monitor `addreserved` (`hypr_place.rs`, section 5), so tiled windows start at
    `margin + thickness + gap` from the screen edge.
  - `exclusive = false` reserves nothing (the bar floats over windows); `margin`
    still positions it and `gap` is ignored.
  - Like all TOML settings, `margin`/`gap` are read at startup; thickness changes
    from CSS still apply live.
- `edge` decides the **orientation**: `top`/`bottom` → horizontal,
  `left`/`right` → vertical.
- The same `output` may host several bars on different edges. Bars on the same
  output + edge are stacked in config order (the first is closest to the screen
  edge), and their reserved space adds up.
- `output = "*"` creates one bar copy per monitor, and follows hotplug (a copy is
  created when a monitor appears and destroyed when it disappears).
- An `output` that is not connected simply produces no bar (logged at info level),
  so one config works for docked and undocked setups.
- Zero-config default: if no `[bars]` table exists, a single bar named `main` is
  created on the default output, top edge, using `[bar_defaults]`.

### 3.3 Plugin instance config (`plugins.d/<instance>.toml`)

Every plugin file has a common `[position]` block read by the **bar**; all
other keys are passed to the **plugin** untouched.

```toml
plugin = "clock"        # runs dool-plugin-clock
enabled = true

[position]
bar = "main"            # bar name, or a list: ["main", "side"]
section = "start"       # start | center | end  (left→right, or top→bottom on vertical bars)
order = 10              # sort key inside the section

# ---- plugin specific keys below ----
face = "/etc/days-of-our-lives/clock-face.svg"
update_interval_seconds = 1
```

Slot size, margins, padding, background and "take the remaining space"
(`flex-grow: 1`) are styled in CSS via the slot's id `#clock` (= instance name)
or class `.plugin-clock` (= plugin name).

Each plugin instance runs **one process per bar copy it appears on** (e.g. a
workspaces instance on `bars.side` with `output = "*"` runs once per monitor).
That keeps plugins single-slot and stateless about other bars.

### 3.4 Styling (`style.css`)

gpui has no CSS engine — elements are styled from Rust (`div().bg(..).p_2()`),
which fills a `gpui::StyleRefinement`. The bar therefore adds a **small CSS layer**
(`style.rs`, ~250 lines): parse a CSS subset, match selectors against the bar's
element tree, cascade, and convert the winning declarations into a
`StyleRefinement` per element. gpui lays out with flexbox (taffy), so sizes,
margins, padding and `gap` behave like web CSS.

- Parsing + selector matching: `simplecss` (tiny, the parser resvg uses).
  Colours: `csscolorparser` (names, `#rgb[a]`, `#rrggbb[aa]`, `rgb[a]()`, `hsl[a]()`).
- Load order: **built-in default stylesheet → `/etc/.../style.css` → user
  `style.css`**. Cascade = specificity, then source order (later file wins), so a
  user file only needs the rules it changes.
- **Live reload:** a gpui background timer checks the files' mtimes every second;
  on change → re-parse, recompute styles, `cx.notify()` every bar, resize the
  bar windows and reserved space if thickness changed, and send `style` to plugins (see 4.5).
- Errors (bad value, unknown property) are logged with file, selector and property;
  everything else in the file still applies. `check-config` reports them too.

**Element tree and selectors:**

```
bar#<bar>.<edge>.<orientation>.output-<connector>    e.g. bar#main.top.horizontal.output-DP-1
├─ section.start
│   └─ slot#<instance>.plugin-<plugin>                e.g. slot#clock.plugin-clock
├─ section.center
└─ section.end

tooltip#<instance>.plugin-<plugin>                     separate popup window (4.8)
```

State: `.crashed` on a slot whose plugin died, `.missing` when the plugin isn't
installed (see 4.4); `:hover` on `bar` and `slot`
(mapped to gpui's `.hover(|s| …)`). `check-config` warns if a bar and a plugin
instance share a name (their `#id`s would collide).

Supported selectors: type (`bar`, `section`, `slot`, `tooltip`), `#id`, `.class`, `*`,
compound (`slot.plugin-clock`), descendant (`bar#side slot`), child (`>`), `:hover`.

**Supported properties** (documented in the README):

| Group                    | Properties                                                              |
|--------------------------|-------------------------------------------------------------------------|
| Size                     | `width`, `height`, `min-/max-width`, `min-/max-height` (`px`, `%`)      |
| Box                      | `margin`, `padding` (1–4 values and `-top/-right/-bottom/-left`), `gap` |
| Flex                     | `flex-grow`, `flex-shrink`                                              |
| Background, transparency | `background`/`background-color` (any colour incl. alpha), `opacity`     |
| Border                   | `border-width`, `border-color`, `border` (`1px solid <colour>`), `border-radius` (1–4 values) |
| Shadow                   | `box-shadow` (`x y blur spread colour`, comma-separated list)           |
| Text (inherited)         | `color`, `font-family`, `font-size`, `font-weight` — passed to plugins  |

Not supported (keeps `style.rs` small): transitions/animations, gradients,
`position`/`display`, `!important`, `@media`/`@import`, units other than `px`/`%`.

Built-in default stylesheet (compiled in with `include_str!`, also shipped as a
commented example in `examples/etc/.../style.css`):

```css
bar {
  background-color: rgba(30, 30, 46, 0.80);   /* transparency */
  color: #cdd6f4;
  font-family: "Inter"; font-size: 11px;
  padding: 4px 6px;
}
bar.horizontal { height: 32px; }
bar.vertical   { width: 32px; }
bar.horizontal slot { width: 32px; }           /* default slot size along the bar */
bar.vertical   slot { height: 32px; }
section { gap: 8px; }
slot { border-radius: 6px; }
slot:hover { background-color: rgba(255, 255, 255, 0.08); }
slot.crashed { background-color: rgba(243, 139, 168, 0.30); }
tooltip {
  background-color: rgba(17, 17, 27, 0.95);
  color: #cdd6f4;
  font-family: "Inter"; font-size: 11px;
  padding: 4px 8px;
  border: 1px solid #45475a;
  border-radius: 6px;
  max-width: 320px;
}
```

Example user overrides:

```css
bar#side   { width: 64px; border-radius: 12px; opacity: 0.9; }  /* rounded; floats via TOML margin */
bar.output-eDP-1 { height: 28px; }                                         /* per display */
#clock     { width: 40px; }
#calendar  { flex-grow: 1; }                                               /* take the remaining space */
```

Notes:

- The bar window uses `WindowBackgroundAppearance::Transparent` (and Hyprland rules
  disable blur, dimming, borders and inactive opacity for it, see `hypr_place.rs`); what you
  see is the `bar` element, so `rgba` backgrounds, `opacity` and rounded corners
  give real transparency. Combined with the TOML `margin` (3.2) this gives a
  "floating" bar.
- **Thickness** = the bar's `height` (horizontal) / `width` (vertical). It sets the
  window's size and the reserved space; on reload the bar re-places its windows and
  updates `addreserved` (`hypr_place.rs`, section 5).
  The bar always spans its edge (minus the TOML margins), so `width` of a horizontal
  bar (and `height` of a vertical one) is ignored.
- `margin` on the `bar` element is ignored with a warning — the space around the bar
  is configured only in TOML (`margin`, `gap`), so there's one place for it.
  `margin` on sections and slots works normally.
- Section layout is fixed in code, not CSS: `start` and `end` take equal
  `flex-grow: 1` with `flex-basis: 0`, `center` is `flex-none` — so the centre
  section stays truly centred.
- Slot background/border/padding are drawn by gpui; the plugin's SVG is drawn inside
  the slot's content box on top of it.

---

## 4. Plugin architecture

### 4.1 The idea

The bar only arranges and paints slots. **All functionality lives in plugins.** A
plugin is a small program that owns one rectangular slot on one bar and does three
things:

1. keeps whatever state it needs (time, calendar events, Hyprland workspaces, …),
2. draws that state as an **SVG picture** whenever it changes,
3. reacts to **mouse input** on its slot (move, down, up, click, wheel, enter/leave)
   — by itself, e.g. telling Hyprland to switch workspace, redrawing a hover
   highlight, or asking the bar to show a tooltip. The bar never performs actions
   on a plugin's behalf.

So the bar is a dumb "picture frame + input forwarder", and the plugin is a tiny,
independent program that you can run and test without the bar (it just reads
JSON on stdin and prints JSON on stdout).

```
            days-of-our-lives (one process)                       plugin processes
 ┌────────────────────────────────────────────────────┐
 │ config.rs ─► bars.rs ─► BarView (gpui window)      │
 │                           │ slot "clock"    ◄──────┼── svg ─────────────┐
 │                           │ slot "calendar" ◄──────┼── svg / tooltip ───┼──┐
 │ plugin_host.rs  ──────────┴── mouse/resize/style ──┼──────────────────► │  │
 └────────────────────────────────────────────────────┘                    │  │
                                              dool-plugin-clock ───────────┘  dool-plugin-calendar
                                              (thread: ticker)                (threads: ICS fetcher, minute ticker)
```

Terminology:

| Term                | Meaning                                                                 |
|---------------------|-------------------------------------------------------------------------|
| **plugin**          | an installed executable `dool-plugin-<name>`, e.g. `dool-plugin-clock`   |
| **instance**        | one `plugins.d/<instance>.toml` file using a plugin (two clocks = two instances) |
| **slot**            | the rectangle an instance draws into on one bar copy                    |
| **plugin process**  | one running process per *(instance × bar copy)*; it serves exactly one slot |

One process per slot keeps every plugin trivially simple: it never needs to know
about other bars, displays or instances. The cost (e.g. two workspace grids = two
small processes each listening to Hyprland) is negligible.

### 4.2 Discovery and installation

- **Installing a plugin = putting `dool-plugin-<name>` on `$PATH`** (or in a
  directory listed in `plugin_path`). Nix, distro packages, `cargo install`,
  `pipx install` or a hand-written script in `~/.local/bin` all work.
- Search order: `plugin_path` entries, then `$PATH`; the first match wins (like a shell).
- Every plugin must answer two flags without starting up:
  - `--describe` → one JSON line: `{"name":"clock","description":"Analogue clock","protocol":1,"version":"0.1.0"}`
  - `--default-config` → a commented example TOML for an instance (incl. `[position]`).
- Optional shared data (e.g. default clock faces) goes in
  `<prefix>/share/days-of-our-lives/plugins/<name>/`; the plugin finds it relative to
  its own executable (`../share/...`), no bar involvement.
- Bar commands built on this:
  - `days-of-our-lives plugins list` — name, version, path, description of every
    installed plugin, and which instances use it.
  - `days-of-our-lives plugins init <name> [instance]` — writes `--default-config`
    to `~/.config/days-of-our-lives/plugins.d/<instance>.toml` (refuses to overwrite).
  - `days-of-our-lives check-config` — also reports instances whose plugin is not
    installed or speaks a newer protocol.

### 4.3 From TOML to a running process

For each enabled instance and each bar copy it is placed on:

1. The merged instance TOML (global + user, see 3.1) is split: `plugin`, `enabled`
   and `[position]` are used by the bar; **everything else becomes `init.config`**
   (TOML → JSON, so plugins only ever parse JSON).
2. The bar spawns the executable with:
   - stdin/stdout piped, stderr piped (each line logged as `[<instance>@<bar>/<output>] …`),
   - working directory = `$HOME`,
   - environment = the bar's environment (so `HYPRLAND_INSTANCE_SIGNATURE`,
     `XDG_RUNTIME_DIR`, `XDG_DATA_DIRS` … are available) plus
     `DOOL_PROTOCOL=1`, `DOOL_INSTANCE`, `DOOL_BAR`, `DOOL_OUTPUT`.
3. The bar sends `init` as the first line, then `resize`/`style` and mouse messages
   as they happen.

### 4.4 Lifecycle

```
bar                                   plugin
 │  spawn                               │
 │──── init {config, size, style} ─────►│  parse config, start own threads
 │◄──────────────── svg ────────────────│  first frame (bar shows empty slot until then)
 │◄──────────────── svg ────────────────│  … whenever state changes
 │──── mousemove {x, y, targets} ──────►│  hover: maybe redraw highlight
 │◄──────────────── tooltip ────────────│  … and/or ask for a tooltip
 │──── mousedown / mouseup / click ────►│  act (e.g. Hyprland dispatch), maybe new svg
 │──── mouseleave ─────────────────────►│  clear hover state (bar hides tooltip itself)
 │──── resize / style ─────────────────►│  re-render at new size / colours
 │  bar exits or slot removed:          │
 │──── close stdin ────────────────────►│  plugin exits on EOF
 │  after 2 s still alive → SIGTERM, +2 s → SIGKILL
```

- **Crash / exit** while the slot still exists: the slot gets CSS class `.crashed`
  (last frame stays visible), and the plugin is restarted with backoff 1, 2, 4 … 30 s.
  The backoff resets after 60 s of healthy running.
- **Not installed**: the slot gets class `.missing` and stays empty; one warning is logged.
- **Bad config**: the plugin logs the reason (`log` message, level `error`) and exits
  non-zero; the bar shows it as crashed. `check-config` cannot validate
  plugin-specific keys, so plugins should give precise error messages
  (`calendars[1].color: invalid colour "#12"`).
- Plugins must exit when stdin reaches EOF — this is how they are stopped cleanly
  and why orphaned plugins never linger.

### 4.5 Protocol (v1)

Newline-delimited JSON, one object per line, UTF-8. Unknown message types and fields
are ignored on both sides (forward compatible). The serde types live in
`dool-plugin-sdk::protocol` and are used by both the bar and the Rust plugins, so
the protocol is defined in exactly one place.

**Bar → plugin (stdin)**

| Message  | Fields | When |
|----------|--------|------|
| `init`   | `protocol`, `instance`, `bar`, `output`, `edge`, `orientation`, `width`, `height`, `scale`, `style`, `config`, `config_dir` | first line, once |
| `resize` | `width`, `height`, `scale` | slot content box or display scale changed |
| `style`  | `color`, `font_family`, `font_size`, `font_weight` | CSS (re)loaded |
| `mouseenter` | pointer fields (below) | pointer entered the slot |
| `mousemove`  | pointer fields | pointer moved inside the slot (coalesced, ≤ 1 per frame) |
| `mousedown`  | pointer fields + `button` | a button was pressed inside the slot |
| `mouseup`    | pointer fields + `button` | a button was released (also outside the slot if it was pressed inside) |
| `click`      | pointer fields + `button` | after `mouseup`, if down and up happened in the slot with < 4 px movement |
| `mouseleave` | — | pointer left the slot (or the slot/bar went away) |
| `wheel`      | pointer fields + `dx`, `dy` (lines; touchpads accumulate to whole lines) | wheel/touchpad scroll over the slot |

**Pointer fields** (on every mouse message except `mouseleave`):

| Field | Meaning |
|-------|---------|
| `x`, `y` | position in the **SVG user units of the frame currently shown** (its `viewBox` coordinates) — i.e. the coordinates the plugin drew in. The bar undoes the fit/letterbox transform and HiDPI scaling. |
| `inside` | `false` when the pointer is in the slot but outside the drawn viewBox (letterbox area) |
| `targets` | `id`s of SVG elements under the pointer, innermost first, e.g. `["ev-3", "lane-1"]`; `[]` if none |
| `buttons` | bitmask of held buttons (1 left, 2 right, 4 middle — like the DOM) |
| `button` | for down/up/click: 1 left, 2 middle, 3 right, 8/9 back/forward |
| `mods` | held modifiers: subset of `["shift","ctrl","alt","super"]` |

```json
{"type":"init","protocol":1,"instance":"clock","bar":"main","output":"DP-1","edge":"top","orientation":"horizontal","width":40,"height":32,"scale":1.5,"style":{"color":"#cdd6f4","font_family":"Inter","font_size":11,"font_weight":400},"config":{"face":"faces/plain.svg","update_interval_seconds":"auto"},"config_dir":"/home/miek/.config/days-of-our-lives/plugins.d"}
{"type":"resize","width":40,"height":32,"scale":1.5}
{"type":"mousemove","x":212.5,"y":14.0,"inside":true,"targets":["ev-3"],"buttons":0,"mods":[]}
{"type":"click","x":212.5,"y":14.0,"inside":true,"targets":["ev-3"],"buttons":0,"button":1,"mods":["ctrl"]}
{"type":"wheel","x":20.0,"y":10.0,"inside":true,"targets":[],"buttons":0,"mods":[],"dx":0,"dy":-1}
{"type":"mouseleave"}
```

**Plugin → bar (stdout)**

| Message | Fields | Effect |
|---------|--------|--------|
| `svg`   | `svg` (complete SVG document as a string) | replaces the slot's picture |
| `tooltip` | `text` (string, `\n` for line breaks, or `null` to hide), optional `anchor` `{x, y, w, h}` in SVG user units | shows/updates/hides the slot's tooltip (4.8) |
| `log`   | `level` (`debug`/`info`/`warn`/`error`), `message` | written to the bar's log |

```json
{"type":"svg","svg":"<svg xmlns=\"http://www.w3.org/2000/svg\" viewBox=\"0 0 40 32\">…</svg>"}
{"type":"tooltip","text":"Standup\n09:00–09:15 · work","anchor":{"x":180,"y":2,"w":60,"h":28}}
{"type":"tooltip","text":null}
{"type":"log","level":"warn","message":"ics fetch failed: timeout"}
```

### 4.6 Drawing contract

- Plugins work in **logical pixels**: draw for `width × height` and emit
  `viewBox="0 0 width height"`. The bar rasterises at `× scale` for HiDPI, so text
  and lines stay crisp without the plugin doing anything.
- If the SVG's aspect ratio differs from the slot, it is fitted and its own
  `preserveAspectRatio` decides alignment (default: centred) — so fixed-size
  artwork like a clock face can be sent unchanged.
- The background is transparent: slot background, border and rounded corners come
  from CSS. Plugins draw only their content.
- Text: system fonts (loaded once by the bar via `fontdb`). Plugins should use
  `style.font_family`/`font_size` and `style.color` as defaults so they follow the
  theme; plugin colour options override.
- Images: `<image href="/absolute/path.png|.svg">` is allowed (local files only, no
  network). Used for application icons.
- The bar always paints the **latest** frame only: if a plugin sends frames faster
  than they are painted, intermediate frames are dropped. A plugin should still
  avoid sending identical frames (the SDKs skip duplicates automatically).
- Limits: a line over 8 MiB or an SVG that fails to parse is logged and ignored;
  the previous frame stays.

### 4.7 Mouse input contract

- **All** pointer activity over a slot is forwarded (table in 4.5). The bar has no
  built-in actions; plugins turn input into effects themselves (Hyprland dispatch,
  `xdg-open`, redraw with a hover highlight, show a tooltip, …).
- **Coordinates are SVG user units, not percentages or screen pixels.** The bar
  knows the transform it used to fit the frame's `viewBox` into the slot (scale +
  letterbox offset) and applies its inverse, so `x`/`y` are directly comparable with
  the coordinates the plugin wrote into its SVG — whatever the slot size, display
  scale or `preserveAspectRatio`. Why not percentages: hit-testing would need a
  conversion back to drawing coordinates, and with letterboxing a percentage of the
  slot isn't a percentage of the drawing. A plugin that wants 0..1 values divides by
  its own viewBox size.
- **Hit-testing by id (`targets`)**: the bar already holds the parsed SVG (usvg
  tree), so it reports which elements with an `id` are under the pointer, innermost
  first (test against each node's absolute bounding box; a `<g id=…>` counts when any
  of its children is hit). usvg drops fully invisible shapes while parsing, so a pure
  hit area needs a tiny fill — `fill="#000" fill-opacity="0.01"`; the SDK's
  `svg::hit_area(id, x, y, w, h)` writes exactly that. Most plugins therefore need **no geometry code**: give the
  clickable/hoverable things ids (`ws-3`, `win-0x55d0`, `ev-7`) and look at
  `targets[0]` or search `targets` for a prefix.
- **Ordering**: `mouseenter` → `mousemove`* → (`mousedown` → `mousemove`* → `mouseup`
  → `click`?)* → `mouseleave`. A press that starts in the slot keeps reporting
  `mousemove` and the final `mouseup` even if the pointer leaves the slot (simple drag
  support); `mouseleave` follows the `mouseup` in that case.
- **Rate**: `mousemove` is coalesced to at most one per rendered frame and only sent
  when the position or `targets` changed, so plugins never get flooded.
- **Stale targets**: `targets` always refers to the frame currently on screen; if a
  plugin sends a new frame, the bar re-hit-tests at the last pointer position and
  sends a `mousemove` if `targets` changed (keeps hover state correct when content
  moves under a still pointer, e.g. the calendar re-laying out).
- CSS `slot:hover` still works independently for purely visual feedback.

### 4.8 Tooltips

A plugin shows a tooltip by sending `tooltip` with text (typically from its
`mousemove` handler); `text: null` hides it.

- Rendering: the tooltip is a small separate gpui window (same app id as the bar,
  title `dool-tooltip:<bar>@<output>`), handled by the same Hyprland rules as bars:
  floating, pinned, never focused, no border/animation (`hypr_place.rs`, section 5). It can therefore
  extend beyond the bar.
- Placement: the bar knows its own window rectangle (it placed it), so it computes
  the tooltip's absolute position itself: next to `anchor` (mapped from SVG units to
  screen coordinates), or next to the pointer if no anchor is given; on the side away
  from the bar's edge (below a top bar, right of a left bar, …) with a 4 px offset,
  then clamped to the monitor's bounds. Moving/resizing an open tooltip is one
  Hyprland dispatch; it is only closed when hidden.
- Timing: shown after `tooltip_delay_ms` (bar TOML, default 400 ms) of the same text
  being requested; switching between texts while one is visible is immediate (like
  native tooltips). Hidden on `text: null`, on `mouseleave`, on `mousedown`, and when
  the plugin crashes/restarts.
- Looks: CSS on a `tooltip` element — `tooltip { … }`, or per plugin
  `tooltip.plugin-calendar { … }` (the popup's root gets the same `plugin-<name>`
  class and the instance as id: `tooltip#calendar`). Text only in v1 (plain text,
  `\n` line breaks, no markup).
- One tooltip per bar at a time; a new request from another slot replaces it.

### 4.9 Versioning

`protocol` is a single integer. The bar sends its version in `init`; plugins state
theirs in `--describe`. The bar refuses to start a plugin with a **higher** protocol
number (logged, slot `.missing`). Additions that old peers can ignore (new optional
fields/messages) do not bump the number; only breaking changes do.

### 4.10 Writing a plugin

Smallest possible plugin, in shell — useful for docs and as an integration-test
fixture (`plugins/examples/hello.sh`, installed as `dool-plugin-hello`):

```sh
#!/bin/sh
case "$1" in
  --describe) echo '{"name":"hello","description":"says hi","protocol":1}'; exit;;
  --default-config) printf 'plugin = "hello"\n[position]\nbar = "main"\n'; exit;;
esac
while read -r line; do          # exits on EOF
  echo '{"type":"svg","svg":"<svg xmlns=\"http://www.w3.org/2000/svg\" viewBox=\"0 0 60 20\"><text x=\"2\" y=\"15\" fill=\"white\">hi</text></svg>"}'
done
```

In Rust, a plugin implements one trait from the SDK (6.0) and calls `run()`; in
Python it subclasses `dool_plugin.Plugin` (6.0). Both give the same structure:
**config → state → `render(state, frame) -> svg`**, with events updating state.

---

## 5. The bar (`crates/bar`)

1. **main.rs** — parse CLI (`run` default, `plugins list`, `plugins init`,
   `check-config`, `--config-dir`, `--log-level`). Set up the logger (7.4), take the
   single-instance lock (7.3), load config + CSS, discover plugins, then
   `gpui::Application::new().run(|cx| bars::start(cfg, cx))`. Subcommand
   `hypr-rules` prints the window rules for `hypr_rules = "manual"`; `cleanup`
   resets the reserved space (used by the systemd unit's `ExecStopPost`).
2. **discovery.rs** — iterate `plugin_path` then `$PATH`, collect executable files
   matching `dool-plugin-*`; first hit wins (like a shell). Returns `name → path`.
3. **bars.rs** — turns bar configs into live bars:
   - Monitors come from Hyprland (`j/monitors` via `dool_plugin_sdk::hypr`): connector
     name, logical bounds, scale. gpui's display list is not needed at all.
   - For every enabled `[bars.<name>]`: `output = "X"` → one bar on monitor X if
     present; `"*"` → one bar per monitor; `""` → one bar on the focused monitor.
   - Hyprland events (`monitoraddedv2`, `monitorremovedv2`) create/destroy bar copies
     (and their plugin processes) on hotplug. A bar copy is identified by
     `(bar name, connector)`, so the diff is a simple set comparison.
     `configreloaded` (e.g. `hyprctl reload`, which drops IPC-set rules and
     reserved space) makes the bar re-apply rules, geometry and reserved space.
4. **bar_view.rs** — one gpui window + one `BarView` entity per bar copy:
   - `cx.open_window(WindowOptions { kind: WindowKind::Normal, app_id: Some("days-of-our-lives"), titlebar: Some(TitlebarOptions { title: Some("dool:<bar>@<output>"), .. }), window_background: Transparent, window_bounds: <computed size>, focus: false, is_resizable: false, .. }, ..)`.
     It draws **nothing until placed** (see `hypr_place.rs` below), so it never flashes at Hyprland's
     default floating position.
   - Size/position come from `hypr_place.rs`: thickness from CSS (3.4), margins and
     gap from TOML (3.2), monitor bounds from Hyprland.
   - `impl Render for BarView`: a root `div()` (the `bar` element) that is
     `flex_row()` or `flex_col()` according to the edge — the only
     orientation-dependent code — containing three section `div`s and, inside them,
     one slot `div` per plugin instance sorted by `order`. Each element gets its
     computed `StyleRefinement` (and hover refinement) from `style.rs`.
   - A slot contains `img(ImageSource::Render(frame)).size_full()` with the latest
     plugin frame.
   - Input (`input.rs`, see 4.7): each slot `div` registers `.on_hover(..)`
     (enter/leave), `.on_mouse_move(..)`, `.on_any_mouse_down(..)`,
     `.on_mouse_up(..)` + `.on_mouse_up_out(..)` (release after dragging out),
     and `.on_scroll_wheel(..)`. Event positions are window coordinates; each slot
     records its bounds with a `canvas()` child during prepaint, and `render.rs`
     provides the frame's fit transform, so `window → slot → SVG user units` is two
     subtractions and a division. `input.rs` also owns the small per-slot state
     machine: pressed button + press position (for `click` = < 4 px movement),
     drag capture, `mousemove` coalescing (one per frame, only on change) and
     `targets` lookup via `render::hit_test`. The recorded bounds also detect slot
     size changes → `resize`.
   - Tooltips (`tooltip.rs`, see 4.8): one optional `TooltipView` per bar, a second
     `WindowKind::Normal` window (title `dool-tooltip:<bar>@<output>`, size = measured
     text + CSS padding) placed by `hypr_place.rs` like the bar. Its root element is styled by `style.rs` as `tooltip#<instance>.plugin-<plugin>`.
     The delay timer, replace/hide rules and closing on `mouseleave`/`mousedown`/crash
     live here (~120 lines).
5. **hypr_place.rs** (~150 lines) — everything that makes a normal window behave
   like a bar, all over Hyprland IPC (`dool_plugin_sdk::hypr`):
   - **Window rules** (registered once at startup with `keyword` when
     `hypr_rules = "auto"`, or printed by `hypr-rules`), matching app id
     `days-of-our-lives`: float, pin (visible on every workspace), no focus / no
     initial focus, no border, no shadow, no blur, no dim, no animations, rounding 0,
     opacity forced to 1 (so inactive-window opacity rules don't fade the bar — CSS
     controls transparency).
   - **Placement**: on `openwindow>>ADDRESS,WORKSPACE,CLASS,TITLE` for one of our
     titles, send one batch: `movewindowpixel exact X Y,address:ADDRESS` +
     `resizewindowpixel exact W H,address:ADDRESS` with the rectangle from 3.2
     (global layout coordinates, so it lands on the right monitor). Then mark the
     window "placed" so it starts drawing. Repeated on CSS thickness change, hotplug
     and `configreloaded`.
   - **Reserved space**: per monitor, sum the bars' reserved space per edge and set
     `keyword monitor <name>,addreserved,<top>,<bottom>,<left>,<right>`; reset to
     zeros on exit (`cleanup`).
   - Pure functions for the maths (`bar_rect(monitor, edge, margin, thickness,
     stack_offset)`, `reserved(monitor, bars)`, `tooltip_rect(..)`) and for the
     command strings, so it is unit-tested without Hyprland. All Hyprland syntax
     lives in this one file (the targeted Hyprland version is stated in the README).
6. **style.rs** — CSS subset from 3.4: `load(paths) -> StyleSheet`,
   `compute(&StyleSheet, &ElementInfo) -> (StyleRefinement, hover: StyleRefinement, TextStyle)`,
   where `ElementInfo` (name, id, classes, parent) implements `simplecss::Element`.
   Pure functions, unit-tested without a window. Also owns the 1 s mtime-poll reload
   and sends the `style` message (inherited `color`/`font-*` of each slot) to plugins.
7. **plugin_host.rs** — implements 4.3/4.4, one `PluginProcess` per slot:
   - Spawn `std::process::Command` with piped stdin/stdout/stderr, `DOOL_*` env vars,
     cwd `$HOME`; write `init`.
   - Stdout reader thread: parse lines with `dool_plugin_sdk::protocol`, keep only the
     newest `svg` in a `Mutex<Option<String>>` slot ("latest frame wins") and wake the
     UI through an `async_channel`; a `cx.spawn` task then calls
     `bar.update(cx, |bar, cx| { bar.set_svg(slot, svg); cx.notify() })`.
     `tooltip` messages are forwarded the same way to `tooltip.rs` (latest wins too).
     `log` messages go straight to the bar's logger.
   - Stderr reader thread: forward lines to the log, prefixed `[instance@bar/output]`.
   - Writes (`resize`/`style`/mouse messages) are done from the main thread (small
     messages, non-blocking enough).
   - Exit/crash → `.crashed` + restart with backoff via
     `cx.background_executor().timer(..)`; plugin not found → `.missing`.
   - Stop = close stdin, then SIGTERM after 2 s, SIGKILL after 4 s (on bar shutdown,
     hotplug removal, or slot removal).
8. **render.rs** — parse SVG with `usvg` (file loading enabled, system fonts
   loaded once via `fontdb`), render to a `tiny_skia::Pixmap` at slot content size ×
   `window.scale_factor()` (HiDPI), convert premultiplied RGBA → straight-alpha BGRA
   (swap R/B and un-premultiply — a 10-line loop; gpui's own helper is crate-private
   in 0.2.2), wrap as `Arc<RenderImage>`
   (`RenderImage::new([image::Frame::new(buf)])`). The previous frame is released with
   `window.drop_image(old)` so a once-a-second clock doesn't fill the GPU atlas.
   Rendering runs on the main thread (bar SVGs are small); it can move to
   `cx.background_executor().spawn(..)` if profiling ever says so.
   For input, the current frame keeps its `usvg::Tree` and its **fit transform**
   (scale + offset used to place the viewBox in the content box):
   - `to_svg_units(slot_point) -> (x, y, inside)` — inverse of the fit transform.
   - `hit_test(&tree, x, y) -> Vec<String>` — depth-first walk, collect `id`s of nodes
     whose `abs_bounding_box()` contains the point, innermost first. Bar SVGs have tens
     of nodes, so a linear walk per `mousemove` is fine.

---

## 6. Plugins

### 6.0 Plugin SDKs

The SDKs exist so every plugin has the **same shape** and contains only its own
logic. Using them is optional — the protocol (4.5) is the real contract.

**Shape of every plugin** (Elm-like, easy to read and test):

```
config (TOML→JSON) ──► start() ──► State ──► render(State, Frame) ──► SVG
                                     ▲
     own threads (timers, IPC) ──────┤ update(msg)          → "redraw?"
     bar mouse messages ─────────────┘ mouse(ev, out)       → "redraw?" (+ out.tooltip(..))
```

#### Rust: `dool-plugin-sdk`

```rust
pub trait Plugin: Sized {
    type Config: serde::de::DeserializeOwned;      // the plugin's own TOML keys
    type Msg: Send + 'static;                       // messages from the plugin's own threads

    /// Parse nothing here — `config` is already typed. Spawn timers / IPC threads
    /// that send `Self::Msg` through `tx`.
    fn start(config: Self::Config, init: &Init, tx: Tx<Self::Msg>) -> anyhow::Result<Self>;
    fn update(&mut self, msg: Self::Msg) -> bool;                       // true = redraw
    /// All mouse input. Return true to redraw (e.g. hover highlight changed).
    fn mouse(&mut self, _ev: &Mouse, _f: &Frame, _out: &mut Out) -> bool { false }
    /// Pure: no I/O, no clock reads. Everything it needs is in `self` and `frame`.
    fn render(&self, frame: &Frame) -> String;
}

pub enum Mouse {
    Enter(Pointer), Move(Pointer), Leave,
    Down(Pointer, Button), Up(Pointer, Button), Click(Pointer, Button),
    Wheel(Pointer, f32 /*dx*/, f32 /*dy*/),
}
/// x, y (SVG user units), inside, targets (innermost first), buttons, mods.
pub struct Pointer { .. }
impl Pointer {
    /// First target starting with `prefix`, without it: `target("ws-")` → `Some("3")`.
    pub fn target(&self, prefix: &str) -> Option<&str>;
}
/// Side effects of a mouse handler.
pub struct Out { .. }
impl Out {
    pub fn tooltip(&mut self, text: impl Into<String>, anchor: Option<Rect>);
    pub fn hide_tooltip(&mut self);
}

/// Size and look of the slot: width, height, scale, orientation, edge, output, style.
pub struct Frame { .. }

pub fn run<P: Plugin>(describe: Describe, default_config: &str) -> !;
```

`run()` is the whole runtime (~180 lines):

1. Handles `--describe` / `--default-config`.
2. Reads `init`; deserialises `config` into `P::Config` with `serde_path_to_error`, so
   a bad key produces `calendars[1].color: invalid colour "#12"` (logged at `error`,
   exit code 2).
3. Calls `P::start`, then loops over **one channel** that receives both bar messages
   (from a stdin thread) and the plugin's own `Msg`s. `resize`/`style` update the
   `Frame` and redraw; `mouse`/`update` redraw when they return `true`.
4. Redraw = `render()` → skip if identical to the last frame → `svg` message.
   Tooltips likewise: `tooltip` is only sent when text/anchor actually changed, and
   the SDK forgets the current tooltip on `Leave` (the bar already hid it).
5. Stdin EOF → exit 0. A panic is logged as an `error` before exiting.

Helper modules:

| Module  | Contents |
|---------|----------|
| `protocol` | serde types for every message (shared with the bar) |
| `svg`   | `Svg::new(w, h).rect(..).line(..).text(..).image(..).group(id, ..).finish()`, `hit_area(id, x, y, w, h)` (near-invisible rect, see 4.7), `esc()`, colour parsing |
| `tick`  | `every_second(tx, f)`, `every_minute(tx, f)`, `at_midnight(tx, f)` — threads that sleep until the next wall-clock boundary (no drift) |
| `hypr`  | Hyprland IPC without `hyprctl`: `query::<Vec<Client>>("clients")` over `$XDG_RUNTIME_DIR/hypr/$HYPRLAND_INSTANCE_SIGNATURE/.socket.sock` (`j/…` requests), `dispatch("workspace 3")`, `events(tx, f)` — a thread reading `.socket2.sock` lines `EVENT>>DATA` and reconnecting if Hyprland restarts. Typed `Client`, `Workspace`, `Monitor` (+ `Monitor::logical_bounds()`: Hyprland reports monitor size in physical pixels, so divide by `scale` and swap for rotated transforms). Behind cargo feature `hypr`. |
| `log`   | `log::info!/warn!/error!` → `log` messages |

#### Python: `dool_plugin.py`

One file (~80 lines, standard library only) with the same model, for the clock
plugin and third-party Python plugins:

```python
class Plugin:
    def __init__(self, config: dict, init: dict, post): ...   # post(msg) from own threads
    def update(self, msg) -> bool: return True
    def mouse(self, ev: dict, frame: dict, out) -> bool: return False   # ev = raw protocol message
    def render(self, frame: dict) -> str: ...
# out.tooltip(text, anchor=None) / out.hide_tooltip()

dool_plugin.run(MyPlugin, describe={...}, default_config="...")
```

### 6.1 Analog clock — `dool-plugin-clock` (Python)

**What it shows:** an analogue clock face rendered by
[py-analogue-clock](https://github.com/miekmesserschmidt/py-analogue-clock) —
either the library's default face or your own SVG face (any SVG with elements
`hour-hand`, `minute-hand`, optional `second-hand`, optional `transform-center*`).

**Config:**

| Key | Default | Meaning |
|-----|---------|---------|
| `face` | `""` | path to a face SVG; relative paths are resolved against `config_dir`; `""` = library default face |
| `update_interval_seconds` | `"auto"` | `"auto"` = 1 if the face has a `second-hand`, else 60; or a number |
| `timezone` | `""` | IANA zone (e.g. `"Africa/Johannesburg"`) via `zoneinfo`; `""` = local time |
| `tooltip_format` | `"%A, %d %B %Y"` | `strftime` format shown on hover; `""` = no tooltip |

```toml
plugin = "clock"
face = "faces/station.svg"
[position]
bar = "main"
section = "start"
order = 0
```
CSS: `#clock { width: 32px; }` — the face is fitted into the slot keeping its aspect
ratio, so on a vertical bar give it a `height` instead.

**How it works:**

- `__init__`: read the face file once, build `AnalogueClock(svg=face)` (or
  `AnalogueClock()`), detect `id="second-hand"` for `"auto"`, start a ticker thread
  that sleeps until the next whole second/minute and `post("tick")`.
- `render`: `clock.generate(now.time())` and return it unchanged — the bar fits it
  (4.6), so no re-wrapping is needed. If the face has no `viewBox`, add one from its
  `width`/`height` so scaling is exact.
- Mouse: hovering shows the date as a tooltip (`tooltip_format`, anchored to the
  whole face); `mouseleave` is handled by the bar. No click actions.
- Errors: unreadable/invalid face → `log error` with the path, fall back to the
  default face (the bar keeps working).
- Packaging: Python ≥ 3.14 (required by py-analogue-clock); the Nix package is a
  Python env with py-analogue-clock (as in `hypr-analogue`) plus `dool_plugin.py`.
- Tests (pytest): feed `init` with a fixed `timezone` and a monkeypatched clock;
  assert a valid SVG with the hands rotated as expected; `auto` interval detection;
  relative `face` path resolution; fallback on a broken face.

### 6.2 Day calendar — `dool-plugin-calendar` (Rust)

**What it shows:** today's events from one or more ICS feeds on a timeline covering
the **full 24 hours** (00:00 → 24:00, not configurable), each feed in its own
colour, with the elapsed part of the day shaded like a progress bar and a **now
line** running across the events.

Crates: `ureq` (HTTP), `icalendar` (parse), `rrule` (recurrences), `chrono`, `chrono-tz`.

**Config:**

| Key | Default | Meaning |
|-----|---------|---------|
| `orientation` | `"auto"` | `horizontal` \| `vertical` \| `auto` (= follow the bar) |
| `refresh_minutes` | `10` | how often feeds are re-downloaded |
| `show_all_day` | `true` | thin strip for all-day events |
| `hour_labels` | `true` | hour ticks with labels (every 1/3/6 h depending on available length) |
| `now_line_color` | `"#ff5555"` | colour of the now line |
| `now_line_width` | `2` | px |
| `past_shade` | `"#00000066"` | overlay on `[00:00, now]` (the "progress bar") |
| `text_color` | `""` | `""` = `style.color` from CSS |
| `tooltip` | `true` | show a tooltip with the event's name when hovering an event |
| `tooltip_details` | `true` | add time range, calendar name and location (if any) below the name |
| `hover_outline` | `""` | outline colour of the hovered event; `""` = `style.color` |
| `[[calendars]]` | — | one table per feed: `url` (https:// or file://) **or** `url_file` (a file containing a secret URL), `color`, optional `name` |

```toml
plugin = "calendar"
orientation = "auto"
[[calendars]]
name = "work"
url_file = "~/.config/days-of-our-lives/secrets/work-ics-url"   # private feed URL kept out of config
color = "#89b4fa"
[[calendars]]
name = "personal"
url = "https://example.com/personal.ics"
color = "#a6e3a1"
[position]
bar = "main"
section = "center"
```
CSS: `#calendar { flex-grow: 1; }` to take the remaining space.

**State and threads:**

- One **fetcher thread** per feed: download every `refresh_minutes` →
  `Msg::Fetched(i, Result<String>)`. Successful downloads are cached in
  `~/.cache/days-of-our-lives/calendar/<instance>-<i>.ics`; the cache is loaded at
  start so events appear instantly and survive being offline. Failures keep the last
  good data and log a `warn` (once per error kind, not every retry).
- `tick::every_minute` → `Msg::Tick` (moves the now line);
  `tick::at_midnight` → re-expand events for the new day.
- `expand(feeds, today) -> Vec<Event { start, end, title, location, calendar, color, all_day }>`: pure
  function; handles `RRULE`, `RDATE`, `EXDATE`, `RECURRENCE-ID` overrides, `TZID`
  (via `chrono-tz`), floating times (= local), all-day events, events crossing
  midnight (clipped to today), skips `STATUS:CANCELLED`.

**Layout and rendering** (pure, both orientations):

- Layout is computed once in abstract coordinates — `t` (time axis, 0..length) and
  `lane` (cross axis, 0..breadth) — and mapped to SVG at the very end:
  - **horizontal**: time runs left → right; `x = t`, `y = lane`.
  - **vertical**: time runs top → bottom (like a day planner); `x = lane`, `y = t`.
  - Only `map(t, lane) -> (x, y)` and label placement differ, so both layouts share
    all logic. `auto` uses `init.orientation`; setting it explicitly allows e.g. a
    vertical timeline in a large slot.
- Overlapping events go into lanes (greedy: sort by start, put each event in the
  first free lane); lanes share the breadth equally. All-day events: thin strip at
  the start of the cross axis.
- Drawn back to front: hour ticks + labels (skipped when there's no room), event
  rectangles in their feed colour with clipped titles (text always horizontal),
  past shade over `[00:00, now]`, now line across all lanes (vertical line in
  horizontal mode, horizontal line in vertical mode). With a fixed 24 h axis the now
  line is always inside the timeline and `t = length × minutes_since_midnight / 1440`.
  DST days (23 h / 25 h) still use a 24 h axis; times are placed by local wall-clock
  time, so the skipped/repeated hour simply looks empty/overlapping.
- Every event rectangle (incl. all-day ones) carries `id="ev-<n>"` (`n` = index into
  today's event list), so the bar's `targets` tell the plugin which event is under
  the pointer — no geometry code in the plugin. The hovered event gets a
  `hover_outline` stroke.

**Input — hover tooltip:**

- `Enter`/`Move`: `n = pointer.target("ev-")`.
  - If `n` changed: remember `hovered = n`, redraw (outline), and
    `out.tooltip(text, Some(rect_of(n)))` — anchored to the event's rectangle, so the
    tooltip sits next to the event (below it on a top bar, beside it on a side bar).
  - If no event is under the pointer: `hovered = None`, redraw, `out.hide_tooltip()`.
- `Leave`: `hovered = None`, redraw (the bar has already hidden the tooltip).
- Tooltip text: the event **name** (`SUMMARY`, "(no title)" if empty); with
  `tooltip_details`, a second line `09:00–09:15 · work` (or `all day · work`) and a
  third line with `LOCATION` when present.
- When the layout changes under a still pointer (minute tick, new feed data), the
  bar re-hit-tests and sends a fresh `mousemove` (4.7), so the tooltip follows.
- Clicks and wheel: no action (confirmed for v1). Today only; no browsing to other days.

**Tests:** fixture `.ics` files for recurrence/exception/timezone/all-day/midnight
cases → `expand`; lane assignment; `map(t, lane)` for both orientations; SVG
snapshots at a fixed "now" (incl. `ev-<n>` ids and the hover outline); mouse handling:
`Move` over `ev-2` → tooltip with name/time/calendar anchored at event 2's rect,
`Move` over empty space → hide, repeated `Move` over the same event → nothing sent.

### 6.3 Virtual desktop switcher — `dool-plugin-workspaces` (Rust)

**What it shows:** a `columns × rows` grid of Hyprland workspaces; inside each cell,
the windows on that workspace as tiny rectangles at their real relative positions.
Clicking a cell switches to that workspace; hovering shows a tooltip.

Uses `dool-plugin-sdk` with feature `hypr` (no `hyprctl` process, no extra crate).

**Config:**

| Key | Default | Meaning |
|-----|---------|---------|
| `columns`, `rows` | `3`, `2` | grid size; shows workspaces `first .. first + columns*rows - 1` |
| `first` | `1` | id of the top-left workspace |
| `this_output_only` | `false` | only count windows/active state for this bar's display |
| `show_windows` | `true` | draw window rectangles |
| `show_numbers` | `false` | workspace number in each cell |
| `active_color` | `"#cba6f7"` | outline/fill of the active workspace |
| `workspace_color` | `"#313244"` | cell background |
| `window_color` | `""` | `""` = `style.color` |
| `focused_window_color` | `"#f5c2e7"` | the focused window's rectangle |
| `hover_color` | `"#45475a"` | background of the workspace cell under the pointer |
| `tooltip` | `true` | hover tooltip: window class + title over a window rect, otherwise the workspace name and window count |
| `poll_seconds` | `2` | safety-net refresh (see below); `0` = off |

```toml
plugin = "workspaces"
columns = 3
rows = 2
[position]
bar = "main"
section = "end"
```
CSS: `#workspaces { width: 120px; }`. For a vertical side bar use e.g. `columns = 1, rows = 6`.

**State and threads:**

- State = `monitors`, `workspaces`, `clients`, `active workspace` from `j/monitors`,
  `j/workspaces`, `j/clients`. Clients with app id `days-of-our-lives` (the bar's own
  bar and tooltip windows, see `hypr_place.rs`) are filtered out — a shared `hypr::Client::is_dool()`.
- `hypr::events` thread → `Msg::Changed` on `workspacev2`, `createworkspacev2`,
  `destroyworkspacev2`, `moveworkspacev2`, `focusedmonv2`, `openwindow`,
  `closewindow`, `movewindowv2`, `changefloatingmode`, `fullscreen`,
  `activewindowv2`, `monitoradded/removedv2`. Changes are debounced (50 ms) into one
  re-query of all three lists.
- Hyprland emits no event when a tiled window is resized or re-laid-out, so a cheap
  `poll_seconds` timer re-queries too; the SDK's duplicate-frame check makes this free
  when nothing changed.

**Rendering** (pure):

- Cells keep the **monitor's aspect ratio** (from `Monitor::logical_bounds()` of the
  monitor the workspace lives on, or the bar's monitor for empty workspaces) and the
  whole grid is fitted and centred in the slot.
- Each window: `client.at`/`client.size` minus the monitor origin, scaled into the
  cell; floating windows are drawn on top of tiled ones; special workspaces
  (negative ids) are ignored.
- Active workspace (per display if `this_output_only`) gets `active_color`; the
  hovered cell gets `hover_color`.
- Ids for hit-testing: each cell is `<g id="ws-<id>">` containing a
  `hit_area` for the whole cell; each window rect is `id="win-<address>"`. So a
  pointer over a window reports `targets = ["win-0x55d0…", "ws-3"]`.

**Input** (all via `targets`, no coordinate maths):

- `Move`: `ws = target("ws-")` → if changed, redraw with the hover highlight; tooltip
  from `target("win-")` (class — title) or the cell (`Workspace 3 · 2 windows`).
- `Click` left → `hypr::dispatch("workspace <ws>")`. Clicks in the gaps between
  cells hit no `ws-` target and do nothing.
- `Wheel` → `workspace m+1` / `m-1` (next/previous on this monitor).
- `Leave` → clear hover state, redraw.

**Tests:** rendered ids (`ws-*`, `win-*`) and hover highlight; mouse handling from
synthetic `targets` (click → dispatch string, click in a gap → nothing, tooltip
text); window rect scaling with offset/scaled/rotated monitors;
event-line parsing; SVG snapshots from JSON fixtures.

### 6.4 Running applications on current workspace — `dool-plugin-windows` (Rust)

**What it shows:** one application icon per window on the currently active
workspace (of this bar's display), laid out in a `columns × rows` grid.

Uses `dool-plugin-sdk` (feature `hypr`) and `freedesktop-icons` for icon-theme lookup.

**Config:**

| Key | Default | Meaning |
|-----|---------|---------|
| `columns`, `rows` | `4`, `1` | grid size; at most `columns*rows` icons — if there are more windows the last cell shows `+N` |
| `order` | `"position"` | `position` (left→right, top→bottom on screen) \| `focus` (most recent first) \| `class` |
| `icon_theme` | `""` | `""` = the system/default theme |
| `highlight_focused` | `true` | outline the focused window's icon |
| `highlight_color` | `""` | `""` = `style.color` |
| `hover_color` | `"#45475a"` | background of the icon under the pointer |
| `tooltip` | `true` | hover tooltip with the window title (and class); over `+N`, the titles of the hidden windows |
| `middle_click_closes` | `false` | middle click closes the window |

```toml
plugin = "windows"
columns = 4
rows = 1
[position]
bar = "main"
section = "end"
```
CSS: `#windows { width: 128px; }` (4 × 32px icons). On a vertical bar use `columns = 1, rows = N`.

**State and threads:**

- Windows = `j/clients` where `workspace.id` = the `activeWorkspace.id` of this bar's
  monitor (`j/monitors`, matched by `init.output`; falls back to the focused monitor),
  excluding the bar's own windows (`Client::is_dool()`, as in 6.3). The bar's windows
  are pinned, so without this filter they would appear on every workspace.
- Same `hypr::events` + debounce approach as 6.3 (window open/close/move, workspace
  change, focus change), plus `windowtitlev2` so tooltips show current titles.

**Icon resolution** (`icons.rs`, cached per window class):

1. Find a `.desktop` file in `$XDG_DATA_HOME/applications` and every
   `$XDG_DATA_DIRS/*/applications` (covers Nix profiles and Flatpak exports) whose
   file name matches the window `class` case-insensitively, or whose
   `StartupWMClass=` equals it. A ~30-line INI reader is enough (`Icon=` and
   `StartupWMClass=` under `[Desktop Entry]`).
2. `Icon=` is an absolute path → use it; otherwise look it up with
   `freedesktop-icons` (theme from config, size ≈ cell size × scale, SVG preferred).
3. Fallback: a rounded square with the first letter of the class, drawn in SVG.

**Rendering:** grid of cells; each icon is an `<image href="/path/to/icon.svg|png">`
fitted into its cell with a small padding; focused window outlined, hovered icon on
`hover_color`. Each cell is `<g id="win-<address>">` with a `hit_area`; the overflow
cell is `id="more"`.

**Input** (via `targets`):

- `Move`: `addr = target("win-")` → if changed, redraw hover and
  `out.tooltip("<title>\n<class>", Some(cell_rect))`; over `more` → tooltip listing
  the hidden windows' titles; nothing → `hide_tooltip()`.
- `Click` left → `hypr::dispatch("focuswindow address:<addr>")`.
- `Click` middle (if `middle_click_closes`) → `closewindow address:<addr>`.
- `Leave` → clear hover, redraw.

**Tests:** desktop-file matching (exact, case-insensitive, `StartupWMClass`) with a
fake `XDG_DATA_DIRS`; ordering modes; overflow `+N` and its tooltip; mouse handling
from synthetic `targets` (focus/close dispatch strings, tooltip text); SVG snapshots.

---

## 7. Packaging, installation and startup

### 7.1 Nix packages

`flake.nix` (same style as `hypr-analogue`):

- `packages.bar`, `packages.plugin-{clock,calendar,workspaces,windows}`,
  `packages.default` = `symlinkJoin` of everything (so all plugins land on `$PATH`).
- Clock plugin: python env with `py-analogue-clock` (flake input, same `postPatch`
  trick as hypr-analogue).
- Example configs installed to `$out/share/days-of-our-lives/examples/`.
- Bar package: `rustPlatform.buildRustPackage` with a plain `cargoLock.lockFile` (all
  dependencies from crates.io); runtime libs wrapped into `LD_LIBRARY_PATH`
  (`wayland`, `libxkbcommon`, `vulkan-loader`, `fontconfig`, `freetype`).
- `devShells.default` with cargo, rustc, rust-analyzer, pkg-config, the libs above,
  python + py-analogue-clock.

### 7.2 NixOS module (global config in `/etc`)

`nixosModules.default` exposes `programs.days-of-our-lives`:

```nix
programs.days-of-our-lives = {
  enable = true;
  package = days-of-our-lives.packages.${system}.default;   # bar + all plugins
  settings = {                         # -> /etc/days-of-our-lives/config.toml
    bars.main = { output = "DP-1"; edge = "top"; };
  };
  style = ''                           # -> /etc/days-of-our-lives/style.css
    bar { background-color: rgba(30, 30, 46, 0.8); }
  '';
  plugins = {                          # -> /etc/days-of-our-lives/plugins.d/<name>.toml
    clock    = { plugin = "clock"; position = { bar = "main"; section = "start"; }; };
    calendar = { plugin = "calendar"; calendars = [ { url = "…"; color = "#89b4fa"; } ]; };
  };
  systemd.enable = true;               # install + enable the user service (7.3)
};
```

- TOML is generated with `pkgs.formats.toml` and placed via `environment.etc`.
- The package is added to `environment.systemPackages`, and the generated
  `config.toml` sets `plugin_path = [ "${package}/bin" ]`, so plugins are found even
  under systemd's minimal `PATH`.
- Users still override everything in `~/.config/days-of-our-lives/` (3.1).
- No home-manager module in v1.

### 7.3 Starting the bar

Both ways are documented; the systemd unit is shipped in the package
(`share/systemd/user/days-of-our-lives.service`) and enabled by the NixOS module:

- **systemd user service** (recommended — restarts on crash):
  `PartOf=graphical-session.target`, `After=graphical-session.target`,
  `WantedBy=graphical-session.target`, `Restart=on-failure`, `RestartSec=2`,
  `ExecStopPost=days-of-our-lives cleanup` (removes the reserved space if the bar
  died without cleaning up).
  Hyprland's environment (`WAYLAND_DISPLAY`, `HYPRLAND_INSTANCE_SIGNATURE`) must be
  imported into the user manager — automatic with UWSM; otherwise add
  `exec-once = dbus-update-activation-environment --systemd --all` to `hyprland.conf`.
- **Hyprland `exec-once = days-of-our-lives`** — simplest; no automatic restart.

The bar refuses to start a second copy for the same Wayland display (lock file
`$XDG_RUNTIME_DIR/days-of-our-lives-$WAYLAND_DISPLAY.lock`), so using both by
accident is harmless.

### 7.4 Logging

- Bar and plugin messages go to **stderr** (→ journal under systemd) **and** to
  `$XDG_STATE_HOME/days-of-our-lives/bar.log` (default
  `~/.local/state/days-of-our-lives/bar.log`).
- One line per message: `2026-11-22T16:55:42 WARN [calendar@main/DP-1] ics fetch failed: timeout`.
  Plugin `log` messages and plugin stderr lines are prefixed with `[instance@bar/output]`.
- Level from `log_level = "info"` in `config.toml` (`debug` \| `info` \| `warn` \|
  `error`), overridable with `--log-level`.
- The file is rotated at 5 MiB (one previous file kept: `bar.log.1`).
- Implementation: the `log` crate facade + a ~50-line logger writing to both sinks
  (no `tracing`, no extra framework).

---

## 8. Milestones (each ends with something runnable)

| # | Milestone | Done when |
|---|-----------|-----------|
| 1 | Workspace skeleton, gpui 0.2.2 from crates.io, flake devShell, CI-able `cargo build/test/clippy`; **placement spike**: a transparent gpui window turned into a bar by `hypr_place.rs` (rules, exact move/resize, `addreserved`) | the window sits on the right edge of the right monitor, on every workspace, never takes focus, still receives pointer events, tiled windows avoid it, no flash at open |
| 2 | Config loading + merging + `check-config` | unit tests for merge/precedence pass |
| 3 | Bar window with start/center/end slots drawing a static SVG as `RenderImage`; all four edges, margins, stacking | horizontal and vertical bars visible on Hyprland, reserve space, survive `hyprctl reload` |
| 4 | CSS layer (`style.rs`): parse, cascade, → `StyleRefinement`, hover, live reload | transparency, thickness, rounded floating bar change on save without restart |
| 5 | Multiple bars: `[bars.<name>]`, per-output placement, `output = "*"`, hotplug, reserved-space `margin`/`gap` | two bars on different edges/displays; unplugging a monitor removes its copy; windows start at `margin + thickness + gap` |
| 6 | Plugin host (4.3–4.4: spawn, env, `init`, latest-frame-wins, stop on EOF, `.crashed`/`.missing`, backoff) + `plugin-sdk::protocol` + discovery + `plugins list/init` | `plugins/examples/hello.sh` renders; killing it shows `.crashed` then restarts |
| 6b | Mouse input (4.7: all events, SVG units, `targets`, click/drag rules, coalescing) + tooltips (4.8: placed tooltip window, delay, CSS) | a debug plugin (`plugins/examples/mouse-echo`) logs every event with correct SVG coordinates in a letterboxed slot and shows a tooltip outside the bar |
| 7 | `plugin-sdk` (`run`, `svg`, `tick`, `hypr`) + **workspaces** plugin | grid updates live, clicking switches workspace |
| 8 | **windows** plugin | icons of active workspace, grid size respected, click focuses |
| 9 | `dool_plugin.py` + **clock** plugin (Python) | custom face renders, ticks every second |
| 10 | **calendar** plugin | events from multiple ICS feeds in colours, now-line moves, horizontal + vertical layouts, offline cache, hover tooltip with event name |
| 11 | Nix packages, NixOS module, systemd user unit, logging to file, example configs + `style.css`, README (incl. CSS selector + protocol reference) | `nixos-rebuild` with the module shows the full bar(s) after login; `systemctl --user restart days-of-our-lives` works; log file written |

## 9. Testing

- **Unit tests** (`cargo test`): config deep-merge & precedence (incl. overriding one
  key of a global `[bars.x]`), `margin` shorthand parsing (1/2/4 values),
  `hypr_place.rs` maths and commands (bar/tooltip rectangles and reserved space from
  monitor fixtures — offset, scaled and rotated monitors, stacking on one edge,
  tooltip clamping — and the generated Hyprland rule/dispatch strings), CSS (selector matching/specificity/source order,
  `:hover`, inheritance of `color`/`font-*`, value parsing of shorthands, colours and
  bad input), bar-copy expansion (`output` ×
  monitors → set of `(bar, connector)`, and the hotplug diff), plugin discovery on a
  temp `$PATH`, protocol (de)serialisation, SVG → BGRA conversion, input mapping
  (`to_svg_units` round-trips the fit transform incl. letterboxing and HiDPI;
  `hit_test` returns innermost-first ids; click threshold, drag-out `mouseup`,
  `mousemove` coalescing and re-hit-test after a new frame), tooltip rules (delay,
  immediate switch, hide on leave/down/crash), and each plugin's
  own tests listed in 6.1–6.4 (pure `expand`/layout/`render` functions with fixtures
  and SVG snapshots).
- **SDK tests**: `run()` driven with an in-memory stdin/stdout — init → frame, duplicate
  frames suppressed, bad config → precise error + exit 2, EOF → exit 0.
- **Integration test**: the bar's plugin host against `hello.sh` and a
  deliberately crashing script (restart/backoff, `.missing` for a non-existent plugin).
- **Clock plugin**: pytest (see 6.1).
- **Manual**: run under Hyprland with `--config-dir examples/...`; verify
  placement on all four edges and on two monitors, hotplug, `hyprctl reload`
  (rules/reserved space re-applied), reserved space reset on exit, bar never
  focused or faded by inactive-opacity rules, HiDPI scaling, CSS live reload
  (thickness + reserved space follow), GPU memory
  stays flat with a 1 s clock (old frames dropped), plugin crash/restart (kill a plugin PID).

## 10. Decisions and risks

**Confirmed** (2026-11-22):

- **Plugin mechanism** — external executables + JSON/SVG over stdio (rather than
  compiled-in Rust modules or `dlopen` shared libraries).
- **Compositor** — Hyprland only for v1.
- **One plugin process per bar copy**.
- **Executable prefix** — `dool-plugin-`.
- **GUI library** — gpui from **crates.io only** (`=0.2.2`); no git dependency on the
  Zed repository, and no layer shell. Bars and tooltips are normal windows that
  Hyprland floats, pins and places, with space reserved via `addreserved` (`hypr_place.rs`, section 5).
- **Clock** — a slot inside a bar only (positioned with the common `[position]`
  block + CSS); written in Python using py-analogue-clock.
- **Workspaces grid** — a fixed numbered set `first..first+columns*rows-1`,
  including empty workspaces. No drag-to-move windows.
- **Running apps** — one icon per window (no grouping).
- **Calendar** — full 24 h axis; secret ICS URLs only (no Basic auth/OAuth); no click
  action; today only (no browsing days).
- **Config reload** — TOML is read at startup only; CSS reloads live.
- **Tooltips** — plain text with `\n` line breaks.
- **Deployment** — NixOS module writes `/etc`; systemd user unit shipped and
  `exec-once` documented; logs to stderr and a log file.
- **Same plugin on several bars** — one instance file may list several bars
  (`bar = ["main", "side"]`) with identical settings; different settings need
  separate instance files. Each bar copy gets its own plugin process.
- **Own windows** — the workspaces and windows plugins hide the bar's own windows.

**Risks** (accepted, watched by milestone 1):

1. **Bars without layer shell** — compared with a layer surface: other *floating*
   windows can be raised above the bar (tiled windows can't overlap it because of
   the reserved space; fullscreen windows cover it, which is wanted); the bar's
   windows appear in Hyprland's client list, so other tools (e.g. a window switcher)
   may list them; behaviour depends on Hyprland rule/dispatch syntax, which changes
   between Hyprland versions — all of it lives in `hypr_place.rs`, and the README
   states the Hyprland version it targets. `nofocus` windows must still receive
   pointer events; this is the first thing milestone 1 checks.
2. **gpui 0.2.2** — an older snapshot (Oct 2025) of a fast-moving library. gpui usage is
   kept to `bar_view.rs`, `bars.rs`, `input.rs`, `tooltip.rs`, `render.rs` and the
   `StyleRefinement` conversion in `style.rs`, so a later upgrade touches few files.
3. **"CSS" is our own subset**, not a full engine — the README lists exactly what's
   supported.

No open questions remain.
