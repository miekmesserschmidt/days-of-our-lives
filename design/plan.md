# days-of-our-lives — implementation plan

Source: [design.md](design.md), [plugins.md](plugins.md).

Guiding rule from the design: **super simple to understand.** Every choice below
favours few moving parts, plain text formats and small files over cleverness.

---

## 1. Architecture in one paragraph

`days-of-our-lives` is a Rust program built on **GTK4** (gtk4-rs) and
**gtk4-layer-shell** that opens one or more `wlr-layer-shell` surfaces (**bars**) on
Hyprland. Each bar is placed by **display** (monitor) and **screen edge** (top,
bottom, left or right); left/right bars are vertical. A bar does not know how to draw
anything itself. It finds **plugins** — ordinary executables named
`dool-plugin-<name>` on `$PATH` — starts one process per plugin instance per bar, and
talks to each over stdin/stdout using one JSON object per line. Plugins answer with
**SVG frames**; the bar renders each SVG (with `resvg`) into that plugin's slot.
All mouse input on a slot (move, down, up, click, wheel, enter/leave) is forwarded
to the plugin as JSON, in the plugin's own SVG coordinates, and a plugin can ask the
bar to show a tooltip. Looks are plain **GTK CSS**. That is the whole system.

```
┌──────────────── bar (GTK4 window on a wlr-layer-shell surface) ──────────┐
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
- **No custom styling engine:** GTK already has CSS (selectors, `:hover`, transparency,
  live reload), and gtk4-layer-shell gives a real bar surface (anchoring, margins,
  reserved space, never focused, not a window in window lists).

---

## 2. Repository layout (Cargo workspace)

```
Cargo.toml                    # workspace
crates/
  bar/                        # binary: days-of-our-lives
    src/main.rs               # CLI, single-instance lock, signals, startup
    src/config.rs             # load + merge TOML (global, user, plugins.d)
    src/discovery.rs          # find dool-plugin-* on plugin_path + $PATH, --describe
    src/bars.rs               # one layer-shell window per (bar config × matching display), hotplug
    src/slot.rs               # one slot: DrawingArea, mouse input, tooltip, plugin supervision
    src/plugin_host.rs        # spawn a plugin process, read/write JSON lines, stop it
    src/render.rs             # SVG → cairo via resvg/tiny-skia; fit transform, hit_test
    src/style.rs              # load the CSS files (live reload), computed style for plugins
    src/default.css           # built-in default stylesheet
    src/logging.rs            # stderr + rotating log file
  plugin-sdk/                 # crate `dool-plugin-sdk`, used by the bar AND the Rust plugins
    src/protocol.rs           # serde message types — the single definition of the protocol
    src/run.rs                # Plugin trait + run() event loop, --describe/--default-config
    src/svg.rs                # tiny SVG string builder, escaping, colour checks, hit_area
    src/tick.rs               # wall-clock aligned timers (every second / minute)
    src/hypr.rs               # Hyprland IPC: request socket + event socket (feature "hypr")
plugin-sdk-python/
  dool_plugin.py              # one-file Python equivalent of run() (for the clock & 3rd parties)
plugins/
  clock/                      # Python: dool-plugin-clock (clock.py, faces/, tests/)
  calendar/                   # Rust:   dool-plugin-calendar (ics.rs, layout.rs, main.rs)
  workspaces/                 # Rust:   dool-plugin-workspaces
  windows/                    # Rust:   dool-plugin-windows (icons.rs, main.rs)
  examples/hello.sh           # 10-line shell plugin used in docs and tests
  examples/mouse-echo         # debug plugin (Python): logs every mouse message, id'd hit areas
examples/etc/days-of-our-lives/
  config.toml  style.css  plugins.d/*.toml
flake.nix                     # packages, devShell, nixosModules.default
nix/packages.nix              # bar + Rust plugins (one cargo build), clock plugin (Python env)
nix/module.nix                # NixOS module (7.2)
systemd/days-of-our-lives.service   # user unit (7.3)
README.md
```

Crates (kept short): `gtk4` (feature `v4_12`), `gtk4-layer-shell`, `resvg`,
`async-channel`, `libc`, `serde`, `serde_json`, `toml`, `anyhow`, `dirs`, `log`,
`chrono`. Plugins add only what they need (calendar: `ureq`, `rrule`, `chrono-tz`;
windows: `freedesktop-icons`). Runtime: GTK ≥ 4.12 and gtk4-layer-shell ≥ 1.0
(developed against GTK 4.22, gtk4-layer-shell 1.3, Hyprland 0.55).

**History:** an earlier version of this plan used gpui (Zed's UI framework). The
crates.io release (0.2.2) has no layer-shell support and no longer builds cleanly
against current crates (an old `xattr` needs a `libc` that newer `mio`/`rustix`
reject), and the layer-shell code only exists in Zed's git repository. GTK4 +
gtk4-layer-shell replaced it: it builds from crates.io, and it also removed the need
for our own CSS engine and for placing normal windows with Hyprland rules.

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
log_level = "info"        # error | warn | info | debug (see 7.4)

[bar_defaults]
exclusive = true          # reserve space so windows don't overlap the bar
layer = "top"             # background | bottom | top | overlay
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
  (e.g. `.bar#side { min-width: 64px; }`), see 3.4.
- **Reserved space** (logical px):

  ```
  screen edge ─┬─ margin[edge]      empty, reserved
               ├─ thickness (CSS)   the bar
               ├─ gap               empty, reserved
  windows ─────┘
  ```

  - `margin` becomes the layer-shell surface margin (top/right/bottom/left). The
    margin on the bar's edge moves it away from the screen edge; margins on the two
    perpendicular edges shorten the bar (e.g. `[0, 200, 0, 200]` on a top bar → a
    centred bar 400px shorter than the screen). The margin on the opposite edge has
    no effect.
  - The exclusive zone is `thickness + gap`; the compositor adds the margin on the
    bar's edge, so tiled windows start at `margin + thickness + gap` (verified on
    Hyprland 0.55: margin 4, bar 36, gap 4 → 44 px reserved).
  - The thickness is the window's real size, so CSS changes apply to the reserved
    space live (an invisible overlay child reports the size, see 5).
  - `exclusive = false` reserves nothing (the bar floats over windows); `margin`
    still positions it and `gap` is ignored.
- `edge` decides the **orientation**: `top`/`bottom` → horizontal,
  `left`/`right` → vertical.
- The same `output` may host several bars on different edges; the compositor stacks
  layer surfaces and their exclusive zones (a left bar starts below a top bar's
  reserved space).
- `output = "*"` creates one bar copy per monitor, and follows hotplug (a copy is
  created when a monitor appears and destroyed when it disappears).
- An `output` that is not connected simply produces no bar (logged at info level),
  so one config works for docked and undocked setups.
- Zero-config default: if no `[bars]` table exists, a single bar named `main` is
  created on the default output, top edge, using `[bar_defaults]`.
- Unknown keys are logged as warnings; type errors name the file and key
  (`plugins.d/cal.toml: position.section: expected start | center | end, got "middle"`).

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
expand = false          # true: take the remaining space along the bar

# ---- plugin specific keys below ----
face = "faces/station.svg"
update_interval_seconds = "auto"
```

Slot size, margins, padding and background are styled in CSS via the slot's id
`#clock` (= instance name) or class `.plugin-clock` (= plugin name). `expand` is in
TOML because GTK CSS has no `flex-grow`.

Each plugin instance runs **one process per bar copy it appears on** (e.g. a
workspaces instance on `bars.side` with `output = "*"` runs once per monitor).
That keeps plugins single-slot and stateless about other bars.

### 3.4 Styling (`style.css`)

Styling is **GTK4 CSS** (`gtk::CssProvider`) — real CSS with selectors, `:hover`,
transitions, `rgba()` transparency and live reload; no custom styling engine.

- Load order: built-in default stylesheet (`crates/bar/src/default.css`, priority
  APPLICATION) → `/etc/days-of-our-lives/style.css` → user `style.css` (priorities
  USER and USER+1). Later files win, so a user file only needs the rules it changes.
- Both files are watched (`gio::FileMonitor`) and **reloaded live** ~100 ms after a
  save; CSS parse errors are logged with file and line and the rest still applies.
  After a reload every plugin gets a `style` message (computed text colour and font).

**Widget tree and selectors:**

```
window.dool-window                                   transparent layer surface
└─ overlay
   ├─ centerbox.bar#<bar>.<edge>.<orientation>.output-<connector>   e.g. .bar#main.top.horizontal.output-DP-1
   │   ├─ box.section.start
   │   │   └─ drawingarea.slot#<instance>.plugin-<plugin>            e.g. .slot#clock.plugin-clock
   │   ├─ box.section.center
   │   └─ box.section.end
   └─ drawingarea (invisible size probe for the exclusive zone)
tooltip                                              GTK's own tooltip popup
```

State classes: `.crashed` on a slot whose plugin died, `.missing` when the plugin
isn't installed (see 4.4); `:hover` works on everything. `check-config` warns when a
bar and a plugin instance share a name (their `#id`s would collide).

**What to style** (everything GTK CSS supports — common properties):

| Want                    | CSS                                                          |
|-------------------------|--------------------------------------------------------------|
| Bar thickness           | `.bar.horizontal { min-height: 32px; }` / `.bar.vertical { min-width: 48px; }` |
| Per-bar / per-display   | `.bar#side { … }`, `.bar.output-eDP-1 { … }`                 |
| Slot size along the bar | `#clock { min-width: 40px; }` (vertical bars: `min-height`)  |
| Transparency            | `background-color: rgba(30,30,46,0.6);` and/or `opacity: 0.9;` |
| Spacing between slots   | `.section { border-spacing: 8px; }`                          |
| Padding / margin        | `padding`, `margin` on `.bar`, `.section`, `.slot`           |
| Rounded bar             | `.bar { border-radius: 12px; }` (+ TOML `margin` to float it) |
| Borders, shadows        | `border`, `box-shadow`                                       |
| Text colour / font      | `color`, `font-family`, `font-size` (passed to plugins, see 4) |
| Hover / animation       | `.slot:hover { background: rgba(255,255,255,.1); transition: 200ms; }` |
| Tooltips                | `tooltip { background-color: …; border-radius: 6px; }`       |

Notes:

- The window background is transparent (`window.dool-window { background: transparent }`
  in the default CSS); what you see is `.bar`.
- GTK CSS has no `width`/`height`; use `min-width`/`min-height`. A slot's natural size
  is 0, so its `min-*` is its size along the bar (unless `expand = true`).
- Slot background/border/padding are drawn by GTK; the plugin's SVG is drawn on top,
  fitted into the slot.

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
 │ config.rs ─► bars.rs ─► layer-shell window (GTK4)  │
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
  knows the transform it used to fit the frame into the slot (scale + letterbox
  offset; the picture is centred, aspect kept) and applies its inverse, so `x`/`y`
  are directly comparable with the coordinates the plugin wrote into its SVG. Why not
  percentages: hit-testing would need a conversion back to drawing coordinates, and
  with letterboxing a percentage of the slot isn't a percentage of the drawing. A
  plugin that wants 0..1 values divides by its own size. (Precisely: units of the
  SVG's `width`/`height`; plugins emit `viewBox` and `width`/`height` equal, as the
  SDK's `Svg` builder does.)
- **Hit-testing by id (`targets`)**: the bar already holds the parsed SVG (usvg
  tree), so it reports which elements **with an `id`** are under the pointer,
  innermost first (test against each node's absolute bounding box). Elements without
  an id never block what is below them, so labels drawn on top of a rectangle don't
  hide it. Children are tested topmost first and the search stops at the first named
  hit. A named `<g id=…>` is hit when any shape inside it contains the point; its id
  follows the ids found inside it (`["win-0x55d0", "ws-3"]`). usvg drops fully
  invisible shapes while parsing, so a pure hit area needs a tiny fill —
  `fill="#000" fill-opacity="0.01"`; the SDK's `svg::hit_area(id, x, y, w, h)` writes
  exactly that. Most plugins therefore need **no geometry code**: give the
  clickable/hoverable things ids (`ws-3`, `win-0x55d0`, `ev-7`) and use
  `pointer.target("ws-")`.
- **Ordering**: `mouseenter` → `mousemove`* → (`mousedown` → `mousemove`* → `mouseup`
  → `click`?)* → `mouseleave`. A press that starts in the slot keeps reporting
  `mousemove` and the final `mouseup` even if the pointer leaves the slot (a GTK drag
  gesture); `mouseleave` follows the `mouseup` in that case.
- **Rate**: `mousemove` is coalesced to at most one per frame (GTK frame clock) and
  only sent when the position or `targets` changed.
- **Stale targets**: `targets` always refers to the frame currently on screen; when a
  plugin sends a new frame the bar re-hit-tests at the last pointer position and
  sends a `mousemove` if anything changed (keeps hover state right when content moves
  under a still pointer, e.g. the calendar re-laying out).
- CSS `.slot:hover` still works independently for purely visual feedback.

### 4.8 Tooltips

A plugin shows a tooltip by sending `tooltip` with text (typically from its
`mousemove` handler); `text: null` hides it.

- Rendering: a native **GTK tooltip** of the slot (`query-tooltip`): an `xdg_popup`
  of the bar's layer surface, so it may extend beyond the bar and the compositor keeps
  it on screen. The bar calls `trigger_tooltip_query()` whenever the plugin changes
  the text, so it updates in place.
- Placement: GTK places the tooltip next to `anchor` (mapped from SVG units to slot
  coordinates and passed as the tooltip's tip area), or next to the pointer.
- Timing and hiding follow GTK's tooltip behaviour (a short hover delay; switching
  between texts is immediate; hidden when the pointer leaves or a button is pressed).
  The bar also drops the tooltip when the plugin crashes or restarts.
- Looks: `tooltip { … }` in CSS. Text only (plain text, `\n` line breaks).

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

1. **main.rs** — CLI (`run` default, `check-config`, `plugins list`,
   `plugins init <plugin> [instance]`, `--config-dir`, `--log-level`, `--help`,
   `--version`). `run`: load config, set up the logger (7.4), take the single-instance
   lock (7.3), block SIGTERM/SIGINT/SIGHUP and wait for them on one `sigwait` thread
   (handed to the GTK main loop to quit cleanly), discover plugins (dropping those
   whose `--describe` reports a newer protocol), then run a `gtk::Application`
   (`NON_UNIQUE` — the lock does that job). `activate` checks
   `gtk4_layer_shell::is_supported()`, starts the bars and the stylesheets, and holds
   the application so it keeps running while no display is connected.
2. **config.rs** — 3.1–3.3: deep merge of `toml::Value`s, bar defaults, instances
   from both `plugins.d` dirs (config dir = the most specific file's directory),
   precise errors. Pure, unit-tested with temp dirs.
3. **discovery.rs** — iterate `plugin_path` then `$PATH`, collect executable files
   matching `dool-plugin-*`; first hit wins (like a shell). `run_flag()` runs
   `--describe` / `--default-config` with a 2 s timeout.
4. **bars.rs** — turns bar configs into live bars:
   - Displays: `gdk::Display::monitors()` (a `ListModel` of `gdk::Monitor` with
     `connector()` names). `wanted(bars, connectors)` is a pure function returning the
     set of `(bar name, connector)` copies; reconciling is a set difference.
   - Hotplug: `items-changed` on the monitor list → reconcile (again after 1 s, since
     connector names can arrive slightly after the monitor).
   - One `gtk::Window` per copy: `init_layer_shell()`, namespace `days-of-our-lives`,
     layer from config, `KeyboardMode::None`, `set_monitor()`, anchors from `edge`
     (top → top+left+right, …), margins from TOML.
   - Content: a `gtk::CenterBox` (the `.bar`) with three `gtk::Box` sections; all
     horizontal or vertical depending on the edge — the only orientation-dependent
     code. Slots are sorted by `(section, order, name)`; `expand` sets
     `hexpand`/`vexpand` on the slot and its section.
   - Reserved space: the CenterBox sits in a `gtk::Overlay` with an invisible,
     non-targetable `DrawingArea` overlay child; its `resize` signal gives the
     window's real thickness → `set_exclusive_zone(thickness + gap)`.
   - Dropping a copy destroys its window and its slots (which stops their plugins).
5. **slot.rs** — one `DrawingArea` per (instance × bar copy):
   - Starts its plugin on the first allocation (so `init` has the real size and the
     computed CSS style), sends `resize` when the content size or the display scale
     (`gdk::Surface::scale()`, fractional) changes.
   - Draws the latest frame (`render.rs`).
   - Input (4.7): `EventControllerMotion` (enter/move/leave), `GestureDrag` with
     `button 0` (press, moves while held, release even outside the slot; `click` when
     released within 4 px in the slot), `EventControllerScroll` (`DISCRETE`, whole
     lines). Moves are coalesced with a one-shot tick callback.
   - Tooltip (4.8): `query-tooltip` returns the plugin's text and anchor.
   - Supervision (4.4): reads `HostEvent`s from the process on a `glib` future,
     handles a whole batch at once and parses only the newest `svg` ("latest frame
     wins"); on exit: `.crashed`, tooltip dropped, restart with backoff 1, 2, 4 … 30 s
     (reset after 60 s of healthy running). A generation counter ignores events of old
     processes.
6. **plugin_host.rs** — one running process: `std::process::Command` with piped
   stdin/stdout/stderr, cwd `$HOME`, `DOOL_*` env vars, and blocked signals unblocked
   again before `exec` (`pre_exec`). Threads: stdout → parsed `PluginMsg`s (lines
   over 8 MiB dropped) into an `async_channel`; stderr → log lines prefixed
   `[instance@bar/output]`; a waiter → `Exited(status)`. Writes happen on the main
   thread; EPIPE just marks stdin closed. `Drop` = close stdin, SIGTERM after 2 s,
   SIGKILL after 4 s.
7. **render.rs** — `usvg::Tree::from_str` (system fonts loaded once), `Fit` (centred,
   aspect kept; `to_svg()`/`to_slot()`), `hit_test()` (4.7), and `paint()`: rasterise
   with `resvg` into a `tiny_skia::Pixmap` at slot size × display scale, convert
   premultiplied RGBA → cairo ARGB32, cache it per (size, scale), and paint it scaled
   by `1/scale` so HiDPI stays crisp.
8. **style.rs** — 3.4: CSS providers, file monitors, parse-error logging, and
   `widget_style()` (computed `color`, font family/size/weight from the widget's Pango
   context) for `init` and `style` messages.
9. **logging.rs** — 7.4.

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
    fn start(config: Self::Config, frame: &Frame, tx: Tx<Self::Msg>) -> anyhow::Result<Self>;
    fn update(&mut self, msg: Self::Msg, frame: &Frame) -> bool;        // true = redraw
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
| `tick`  | `every(period, tx, f)`, `every_second(tx, f)`, `every_minute(tx, f)` — threads that sleep until the next wall-clock boundary (no drift) |
| `hypr`  | Hyprland IPC without `hyprctl`: `query::<Vec<Client>>("clients")` over `$XDG_RUNTIME_DIR/hypr/$HYPRLAND_INSTANCE_SIGNATURE/.socket.sock` (`j/…` requests), `dispatch(r#"hl.dsp.focus({ workspace = "3" })"#)` (Hyprland 0.55 Lua syntax, `lua_str()` quotes values), `watch(names, tx, msg)` — a thread reading `.socket2.sock` lines `EVENT>>DATA`, sending `msg()` once per 50 ms burst of the named events, and reconnecting if Hyprland restarts. Typed `Client`, `Workspace`, `Monitor` (+ `Monitor::logical_bounds()`: Hyprland reports monitor size in physical pixels, so divide by `scale` and swap for rotated transforms). Behind cargo feature `hypr`. |
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
| `face` | `""` | `""` = the bundled face, or a path to a face SVG (relative paths are resolved against `config_dir`) |
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
CSS: `#clock { min-width: 32px; }` — the face is fitted into the slot keeping its
aspect ratio, so on a vertical bar give it a `min-height` instead. With `face = ""`
the plugin uses its bundled `faces/bar.svg` (dark, with a red second hand), falling
back to the library's default face.

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
| `refresh_seconds` | `120` | reload cadence of every feed (`url`, `url_file` or `path`), in seconds; minimum 10 |
| `show_all_day` | `true` | thin strip for all-day events |
| `hour_labels` | `true` | hour ticks with labels (every 1/3/6 h depending on available length) |
| `now_line_color` | `"#ff5555"` | colour of the now line |
| `now_line_width` | `2` | px |
| `past_shade` | `"#00000066"` | overlay on `[00:00, now]` (the "progress bar") |
| `text_color` | `""` | `""` = `style.color` from CSS |
| `tooltip` | `true` | show a tooltip with the event's name when hovering an event |
| `tooltip_details` | `true` | add time range, calendar name and location (if any) below the name |
| `hover_outline` | `""` | outline colour of the hovered event; `""` = `style.color` |
| `[[calendars]]` | — | one table per feed: exactly one of `url` (https://, webcal:// or file://), `url_file` (a file containing a secret URL) or `path` (a local .ics file, relative to the instance's file); `color`; optional `name`; optional `show_text` (default `true`; `false` draws that feed's events without titles, tooltips still work) |

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
expand = true
```
`expand = true` in `[position]` makes it take the remaining space.

**State and threads:**

- One **fetcher thread** per feed: reload every `refresh_seconds` →
  `Msg::Fetched(i, Result<String>)`. Successful downloads are cached in
  `~/.cache/days-of-our-lives/calendar/<instance>-<i>.ics`; the cache is loaded at
  start so events appear instantly and survive being offline. Failures keep the last
  good data and log a `warn` (once per error kind, not every retry).
- `tick::every_minute` → `Msg::Tick` (moves the now line; when the date has changed,
  re-expands events for the new day).
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
  `j/workspaces`, `j/clients` (the bar's layer surfaces are not Hyprland clients, so
  nothing needs filtering).
- `hypr::watch` thread → a message on `workspacev2`, `createworkspacev2`,
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
- `Click` left → `hypr::dispatch("hl.dsp.focus({ workspace = \"<ws>\" })")`. Clicks in the gaps between
  cells hit no `ws-` target and do nothing.
- `Wheel` → focus workspace `m+1` / `m-1` (next/previous on this monitor).
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
  monitor (`j/monitors`, matched by `init.output`; falls back to the focused monitor).
- Same `hypr::watch` + debounce approach as 6.3 (window open/close/move, workspace
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
- `Click` left → `hypr::dispatch("hl.dsp.focus({ window = \"address:<addr>\" })")`.
- `Click` middle (if `middle_click_closes`) → `hl.dsp.window.close("address:<addr>")`.
- `Leave` → clear hover, redraw.

**Tests:** desktop-file matching (exact, case-insensitive, `StartupWMClass`) with a
fake `XDG_DATA_DIRS`; ordering modes; overflow `+N` and its tooltip; mouse handling
from synthetic `targets` (focus/close dispatch strings, tooltip text); SVG snapshots.

---

## 7. Packaging, installation and startup

### 7.1 Nix packages

`flake.nix` + `nix/packages.nix`:

- `packages.bar`: one `rustPlatform.buildRustPackage` for the bar and the three Rust
  plugins (plain `cargoLock.lockFile`, all dependencies from crates.io;
  `wrapGAppsHook4`, `gtk4`, `gtk4-layer-shell`). Installs the systemd unit and the
  examples under `share/`.
- `packages.plugin-clock`: `clock.py` + `dool_plugin.py` + the bundled face, wrapped
  to run with a Python 3.14 env containing py-analogue-clock (flake input, same
  `postPatch` trick as hypr-analogue). Also installs `dool-plugin-mouse-echo`.
- `packages.default` = `symlinkJoin` of both (so all plugins land on `$PATH`).
- Sources are restricted with `lib.fileset` (Cargo files, `crates/`, `plugins/`,
  `plugin-sdk-python/`, `systemd/`, `examples/`), so editing docs or the NixOS module
  doesn't rebuild the packages.
- `devShells.default`: cargo, rustc, clippy, rustfmt, rust-analyzer, pkg-config,
  GTK4, gtk4-layer-shell, Python 3.14 with py-analogue-clock and pytest.

### 7.2 NixOS module (global config in `/etc`)

`nixosModules.default` exposes `programs.days-of-our-lives`:

```nix
programs.days-of-our-lives = {
  enable = true;
  # package = days-of-our-lives.packages.${system}.default;   # bar + all plugins (default)
  settings = {                         # -> /etc/days-of-our-lives/config.toml
    bars.main = { output = "DP-1"; edge = "top"; };
  };
  style = ''                           # -> /etc/days-of-our-lives/style.css
    .bar { background-color: rgba(30, 30, 46, 0.8); }
  '';
  plugins = {                          # -> /etc/days-of-our-lives/plugins.d/<name>.toml
    clock    = { plugin = "clock"; position = { bar = "main"; section = "start"; }; };
    calendar = { plugin = "calendar"; calendars = [ { url = "…"; color = "#89b4fa"; } ]; };
  };
  systemd.enable = true;               # user service started with the graphical session (7.3)
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
  `PartOf=`/`After=`/`Requisite=graphical-session.target`,
  `WantedBy=graphical-session.target`, `Restart=on-failure`, `RestartSec=2`.
  Hyprland's environment (`WAYLAND_DISPLAY`, `HYPRLAND_INSTANCE_SIGNATURE`) must be
  imported into the user manager — automatic with UWSM; otherwise add
  `exec-once = dbus-update-activation-environment --systemd --all` to the Hyprland config.
- **Hyprland `exec-once = days-of-our-lives`** — simplest; no automatic restart.

The bar refuses to start a second copy for the same Wayland display (`flock` on
`$XDG_RUNTIME_DIR/days-of-our-lives-$WAYLAND_DISPLAY.lock`), so using both by
accident is harmless. Reserved space is owned by the layer surfaces, so nothing needs
cleaning up after a crash.

### 7.4 Logging

- Bar and plugin messages go to **stderr** (→ journal under systemd) **and** to
  `$XDG_STATE_HOME/days-of-our-lives/bar.log` (default
  `~/.local/state/days-of-our-lives/bar.log`).
- One line per message: `2026-11-22T16:55:42 WARN  [calendar@main/DP-1] calendars[0]: fetch failed: …`.
  Plugin `log` messages and plugin stderr lines are prefixed with `[instance@bar/output]`.
  Secret calendar URLs are never logged (only scheme and host).
- Level from `log_level = "info"` in `config.toml`, overridable with `--log-level`.
- The file is rotated at 5 MiB (one previous file kept: `bar.log.1`).
- Implementation: the `log` crate facade + a ~50-line logger writing to both sinks.

---

## 8. Milestones (each ends with something runnable)

| # | Milestone | Done when | Status |
|---|-----------|-----------|--------|
| 1 | Workspace skeleton, flake devShell, `cargo build/test/clippy` | builds clean | done |
| 2 | Config loading + merging + `check-config` | unit tests for merge/precedence pass | done |
| 3 | Layer-shell window with start/center/end slots drawing SVG; all four edges | horizontal and vertical bars visible on Hyprland, reserve space | done (verified on Hyprland 0.55.4) |
| 4 | GTK CSS: default/global/user `style.css`, ids/classes, live reload | transparency, thickness, rounded bar from CSS | done |
| 5 | Multiple bars: `[bars.<name>]`, per-output placement, `output = "*"`, hotplug, `margin`/`gap` | two bars on different edges; windows start at `margin + thickness + gap` | done (hotplug not yet exercised with a second monitor) |
| 6 | Plugin host (spawn, env, `init`, latest-frame-wins, stop on EOF, `.crashed`/`.missing`, backoff) + protocol + discovery + `plugins list/init` | `hello.sh` renders | done |
| 6b | Mouse input (all events, SVG units, `targets`, click/drag rules, coalescing) + tooltips | `mouse-echo` logs events with correct SVG coordinates and targets | done (tooltip popup to be checked by hand) |
| 7 | `plugin-sdk` (`run`, `svg`, `tick`, `hypr`) + **workspaces** plugin | grid updates live, clicking switches workspace | done |
| 8 | **windows** plugin | icons of the active workspace, grid size respected, click focuses | done |
| 9 | `dool_plugin.py` + **clock** plugin (Python) | face renders, ticks every second | done |
| 10 | **calendar** plugin | multiple ICS feeds in colours, now line, both orientations, offline cache, hover tooltip | done |
| 11 | Nix packages, NixOS module, systemd unit, logging to file, example configs, README | `nix build` succeeds | done |

## 9. Testing

- **Unit tests** (`cargo test --workspace`, 35 tests):
  - bar: config deep-merge & precedence (overriding one key of a global `[bars.x]`,
    disabling instances), `margin` shorthands, error messages naming file and key,
    bar-copy expansion (`output` × monitors), plugin discovery on temp dirs, the fit
    transform, hit testing (innermost first, unnamed labels don't block), RGBA →
    cairo conversion;
  - SDK: protocol round trips (flattened pointer fields, unknown types/fields
    ignored, `tooltip: null`), `run()` driven in memory (frames on init/resize/click,
    duplicates suppressed, tooltips sent only on change, bad config → exit 2, EOF →
    exit 0), SVG builder/escaping/colour checks, Hyprland monitor bounds (scale,
    rotation), JSON parsing;
  - calendar: unfolding/unescaping, weekly RRULE with EXDATE and a RECURRENCE-ID
    override, all-day/cancelled/midnight-crossing events, TZID conversion, durations,
    lane assignment, orientation mapping, SVG output (ids, shade, now line in both
    orientations), hover tooltip text and anchor, precise config errors;
  - workspaces: grid aspect and window scaling, hover highlight and tooltip text;
  - windows: desktop-file matching (exact, reverse-DNS, `StartupWMClass`), ordering,
    `+N` overflow and its tooltip.
- **Clock plugin**: `pytest plugins/clock/tests` (hands rotated, `auto` interval,
  relative face path, fallback on a broken face, protocol end to end).
- **Manual** (done on Hyprland 0.55.4, single monitor): top and left bars, margins,
  stacking and reserved space; mouse events with SVG coordinates; workspace and window
  dispatches. Still to check by hand: tooltips appearing, CSS live reload, hotplug
  with a second monitor, HiDPI.

## 10. Decisions and risks

**Confirmed** (2026-11-22):

- **Plugin mechanism** — external executables + JSON/SVG over stdio (rather than
  compiled-in Rust modules or `dlopen` shared libraries).
- **Compositor** — Hyprland only for v1 (the bar itself works on any wlr-layer-shell
  compositor; the workspaces/windows plugins and display names need Hyprland).
- **One plugin process per bar copy**.
- **Executable prefix** — `dool-plugin-`.
- **GUI library** — GTK4 (gtk4-rs) + gtk4-layer-shell, with GTK CSS for styling
  (replaces gpui, see 2).
- **Clock** — a slot inside a bar only (positioned with the common `[position]`
  block + CSS); written in Python using py-analogue-clock.
- **Workspaces grid** — a fixed numbered set `first..first+columns*rows-1`,
  including empty workspaces. No drag-to-move windows.
- **Running apps** — one icon per window (no grouping).
- **Calendar** — full 24 h axis; secret ICS URLs only (no Basic auth/OAuth); no click
  action; today only (no browsing days).
- **Config reload** — TOML is read at startup only; CSS reloads live.
- **Tooltips** — plain text with `\n` line breaks, native GTK tooltips.
- **Deployment** — NixOS module writes `/etc`; systemd user unit shipped and
  `exec-once` documented; logs to stderr and a log file.
- **Same plugin on several bars** — one instance file may list several bars
  (`bar = ["main", "side"]`) with identical settings; different settings need
  separate instance files. Each bar copy gets its own plugin process.

**Risks:**

1. **Hyprland IPC syntax** — Hyprland 0.55 moved to Lua: dispatchers are sent as
   `dispatch hl.dsp.focus({ workspace = "3" })`. All of it lives in
   `dool-plugin-sdk::hypr` and the two Hyprland plugins; the README states the
   targeted Hyprland version.
2. **Tooltip delay** is GTK's (not configurable from the bar config).
