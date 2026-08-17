---
name: navigating-as-an-agent
description: >-
  Drive CrossPoint Reader (crossreader) in the eBook firmware simulator over
  this repository's MCP Session tools. Use when testing CrossPoint Reader in
  the simulator via MCP, injecting keys or touch, observing logs, taking
  snapshots, starting or shutting down instances, or confirming UI state.
---

# Navigating as an agent

The firmware UI is the interface. Drive it with Session tools on the
`crosspoint-simulator` MCP server. Do not invent sleeps as a substitute
for knowing the UI finished.

After each new screen or missed tap, add the working panel hit (and
what failed) to **Device profiles** or **Touch** in this file so the
next session does not rediscover it. Label every numeric hit with
`boardId`, theme, and orientation. Those pixels are **not** portable
across boards, themes, firmware builds, or settings (optional rows,
`touchReaderControls`, recents). Recompute from `Register` and the
theme grid; treat listed coordinates as one confirmed session, not a
universal map.

## Loop

1. `inject_*` to act. Pick the inject from `Register` (see **Device
   profiles**): tap when `capTouch` is true; keys when it is not.
2. `observe` immediately to know the action landed.
3. `request_snapshot` from time to time to confirm you are on the screen
   you think you are: after a new activity, after a few list moves,
   before a tap or a book open, and whenever the highlight or path
   might have drifted. Use the snapshot to read labels, not as a wait
   and not as the source of panel coordinates (snapshots are often
   described rotated).

`get_instance` carries `lastHeartbeat.framebufferGeneration` (and
`headless`) and `Register`. `boardId` is `x4`, `x3`, `x4_pro`,
`sticky`, or `paper_mono`. `capFrontlight` is any frontlight style
(including Paper Mono PMIC), not PWM-only. Proto3 JSON omits false
bools: `capTouch` / `capHome` / `capFrontlight` absent means false.
Several instances may be up at once; always name `instance_id`.

## Device profiles

Board identity is compile-time (`-DFREEINK_DEVICE_*`). `start_instance`
cannot change it. Read `Register` after connect and drive that surface.

| `boardId` | Panel | Touch | Home key | Frontlight | Efficient drive |
| --- | --- | --- | --- | --- | --- |
| `x4` | 800×480 | no | no | no | Keys only. `DOWN`/`UP` + `ENTER` on lists; `LEFT`/`RIGHT` in the reader. `inject_touch` / `inject_home` return `no_touch` / `no_home`. |
| `x3` | 792×528 | no | no | no | Same as X4 (keys). Geometry differs; do not reuse X4 tap math. |
| `x4_pro` | 800×480 | yes (GT911) | yes | PWM warm | **Tap** the control (`inject_touch` kind 3). Do not walk menus with `UP`/`DOWN`/`ENTER`. `inject_home` jumps to Home from Settings or a nested activity. Default reader page turns are **tap zones**, not swipe. |
| `sticky` | 800×480 | yes | no | no | Tap like X4 Pro. No Home key — use `BACK` or the reader-menu Go Home row. |
| `paper_mono` | 800×480 | yes (FT5x06) | no | PMIC | Tap like X4 Pro. `capFrontlight` is true. No Home key. |

Efficiency:

- **No touch (X4 / X3):** one `DOWN` per row, then `ENTER`. Snapshot
  before assuming Browse Files is still the default highlight (after a
  book, home confirm is Resume). Reader pages: `LEFT` / `RIGHT`.
- **Touch (X4 Pro / Sticky / Paper Mono):** one tap on the label.
  Compute **panel** pixels from the theme hit grid (below), then
  confirm with `ACT` / `GFX`. A missed tap often produces `InputAck`
  and `InputObserved` but **no** generation bump and no
  `Entering activity`.
- **Home key boards (X4 Pro):** `inject_home` is faster than repeated
  `BACK` when you only need Home. `BACK` from Text Settings returns to
  Settings without an `Entering activity: Settings` line (stack pop).
- **Reader pages (default `touchReaderControls=TOUCH_READER_ON`):** tap
  the **right third** for next, **left third** for previous. A logical
  left-swipe (`inject_swipe` along panel Y) is accepted and observed
  but does **not** turn the page until Settings → Controls sets
  Swipe. Do not use `UP`/`DOWN` for page turns. Button boards still
  use `LEFT`/`RIGHT`.
- **Reader chrome:** on a button board, `ENTER` opens
  `EpubReaderMenu`. On touch, tap the **center third**
  (`isTouchMenuTap`) — panel **(400, 240)** on 800×480 Portrait.
  From that menu, Text Settings is shorter than Home → Settings →
  Reader. Snapshot the menu before tapping: Footnotes and Bookmarks
  rows are optional and shift later hits.

## InputAck is not a paint

Inject waits (default) only mean the edge was accepted onto the
simulator's input queue. The panel updates after firmware handles that
edge.

Useful `observe` lines (need firmware `LOG_LEVEL` ≥ 2 so `LOG_DBG` is
compiled in):

- `ACT` `Entering activity: <Name>` / `Exiting activity: <Name>` /
  `Pushed to activity stack` / `Popped from activity stack` — different
  activity
- `GFX` `Time = N ms from clearScreen to displayBuffer` — a frame was
  submitted
- `ERS` `Rendered page in Nms` and `Progress saved: … page=N` — reader
  page turn finished
- `heartbeat.framebufferGeneration` increased — SDL presented a new
  frame (read `get_instance` if heartbeats are masked out of `observe`)
- After `start_instance`, wait for `ACT Entering activity: Home` before
  driving the home menu (`Register` is earlier, during Boot)
- Opening an EPUB: `EBP Loaded ePub: …` then `ERS Rendered page`
  (first open also logs `EBP Total indexing` and `ERS Cache not found,
  building…` / `SCT Page N processed`)
- `BACK` from the reader: `Exiting activity: EpubReader` then
  `Entering activity: Home` (not the file browser)

`MEM` heap lines are periodic and are not a UI-ready signal.

## Auto-sleep

Firmware auto-sleeps after `SETTINGS.sleepTimeoutMinutes` of no input
(default **10 minutes**; 31 minutes means never). Logs:

- `SLP` `Auto-sleep triggered after N ms of inactivity`
- `MAIN` `Entering deep sleep`

The simulator does not emulate ESP sleep. Deep sleep is a **process
relaunch** plus a synthetic power-button wake. An idle instance you
left running will log those lines and disappear from the Session map
until it redials (new pid). Do not treat that as a proxy crash.

While driving: keep injecting, or send `POWER` after those log lines if
you still need the instance. Prefer shutting down when idle.

`start_instance` defaults to `auto_sleep: false` and seeds
`fs_/.crosspoint/settings.json` with `sleepTimeoutMinutes` 31 (never).
Pass `auto_sleep: true` only when you want the firmware 10-minute idle
sleep. The process default is `--auto-sleep` / `CSM_AUTO_SLEEP`.

## Touch

Hits below were confirmed on **X4 Pro + Lyra + Portrait** (panel
800×480, logical 480×800) against one CrossPoint Reader build. Do not
reuse them on X3 (792×528), a different theme (RoundedRaff / Base),
Landscape, or after firmware layout changes. Prefer theme metrics +
`Register.width`/`height`/`boardId`, then confirm with `ACT` / `GFX`.

Read `Register.capTouch` (and `capHome`) after connect.

`inject_touch` `x`/`y` are **panel pixels** on the framebuffer
(`Register.width` × `Register.height`). They are not SDL window pixels
and not the rotated snapshot description. `kind` 3 is a tap. Observed
`touch.nx` / `ny` are that point normalized to the panel.

Firmware hit-tests in **logical** coordinates
(`GfxRenderer::getScreenWidth/Height`). On X4 / X4 Pro home the usual
orientation is **Portrait**: logical 480×800, panel 800×480.

```
panel_x = logical_y
panel_y = (panel_height - 1) - logical_x
```

Center of a portrait row is about `logical_x = 240` → `panel_y = 239`
on an 800×480 panel.

A tap that is accepted but misses the widget does not change
`framebufferGeneration`. Snapshot after a miss; do not increment a
guess by large steps along the long panel axis (that overshoots the
menu). Use the theme grid, then nudge ~20–40 px if needed.

### Lyra home (X4 Pro, no recents)

Theme: `homeTopPadding=56`, `homeCoverTileHeight=242`,
`homeMenuTopOffset=16`, `menuRowHeight=64`, `menuSpacing=8`.

- `menuTop` = 56+242+16 = **314** (logical y)
- `rowStep` = 64+8 = **72**
- Rows (no OPDS, no Continue Reading): 0 Browse Files, 1 Recent Books,
  2 File Transfer, 3 Settings
- Browse Files (row 0) logical y ≈ 314+36 = **350** → panel **(350, 240)**
  → `Entering activity: FileBrowser`
- Recent Books (row 1) → panel **(422, 240)** →
  `Entering activity: RecentBooks`. First row **(170, 240)** reopens
  the sample EPUB (same first-row hit as FileBrowser).
- File Transfer (row 2) → panel **(494, 240)** →
  `CrossPointWebServer` then `NetworkModeSelection` (Join a Network /
  Calibre Wireless / Create Hotspot). `inject_home` exits both.
- Settings logical y ≈ 314+3×72+32 = **562** → panel **(560, 240)**
- Panel **(700, 240)** is below the menu; miss, no redraw

After a book has been opened, Home shows a cover card (may log
`No known cover image for thumbnail`). Menu row math is unchanged.
`inject_home` from Text Settings that was pushed from the reader
exits TextSettings **and** EpubReader, then Home.

`HomeActivity` uses `rowTouch` on the full width (`xStart=0`,
`xEnd=INT32_MAX`); `panel_y` only needs to land in the row band.

### FileBrowser (X4 Pro, Lyra)

First list row (folder or file) is about **(170, 240)**. That opened
`books` at `/` and `CrossPoint-Reader.epub` under `/books`. Snapshot
descriptions often guess a higher first-row X (~95); prefer **170**.
Wait for `EBP Loaded ePub` then `ERS Rendered page` (first open also
indexes: `ERS Cache not found` / `SCT Page N processed`).
`Entering activity: EpubReader` may already have been drained by the
time those ERS lines arrive.

### Settings (X4 Pro, Lyra)

Tabs are Display, Reader, Controls, System (`UiTabListActivity`).
Font size is not on Display. Reader list hides per-field font size
behind **Text Settings**.

Confirmed panel hits (X4 Pro, Lyra, Portrait 800×480):

- Reader tab (2nd): **(110, 290)**
- Controls tab (3rd): **(110, 200)** — shows `Touch Reader Controls:
  Tap` by default
- Text Settings row (first Reader list row): **(175, 240)** →
  `Entering activity: TextSettings`
- Touch Reader Controls row (Controls, 2nd list row): **(215, 240)**
  opens an option popup (OFF / Tap / Swipe / Inverted Tap). A tap at
  **(615, 240)** did not change the value (still Tap); snapshot X for
  popup options is unreliable. Confirm the value on the list after
  the popup closes.

### Text Settings (X4 Pro)

Preview pane sits **above** the tab bar. Do not reuse Settings-tab Y
for these tabs.

- `tabTop` ≈ 297 (after header + preview). Tabs: Font | Size | Layout | Style
- Size tab: **(317, 350)** (`logical_x` ≈ 129). **(317, 290)** hits
  **Layout** (3rd tab; `logical_x` ≈ 189)
- Size list rows (after tab bar): 12 / 14 / 16 / 18 pt
- **16 pt**: **(525, 240)** — preview becomes `Noto Serif, 16 pt`.
  `FDC Prewarm: … glyphs` is a good size-change signal (not only `GFX`)

BACK from Text Settings returns to Settings (no `Entering activity:
Settings` line; stack pop). `inject_home` from Settings logs
`Exiting activity: Settings` then `Entering activity: Home`.

### Reader (X4 Pro, default tap zones)

`SETTINGS.touchReaderControls` defaults to `TOUCH_READER_ON` (tap),
not `TOUCH_READER_SWIPE`. Outer logical thirds (width/3 of 480 ≈ 160):

- Next page: logical_x ≈ 400 → panel **(400, 80)** —
  `Progress saved: … page=N`
- Previous page: logical_x ≈ 80 → panel **(400, 400)** —
  `Progress saved: … page=N` (confirmed)
- Menu: center third in **both** axes → panel **(400, 240)** →
  `Entering activity: EpubReaderMenu`

A swipe from **(400, 80)** to **(400, 400)** (`duration_ms` 250) is
observed as a trail of `kind` 1 moves and does **not** emit `ERS`.

### EpubReaderMenu (X4 Pro)

Rows are optional: Footnotes if the book has them; Bookmarks only
after at least one bookmark exists. `listRowHeight` is 40 (Lyra), but
confirmed hits below are empirical — snapshot after the row set
changes.

With Footnotes, no Bookmarks (sample EPUB):

- Toggle Bookmark: **(300, 240)** — `ERS Toggle bookmark` then return
  to the reader (menu pops)
- Text Settings: **(350, 240)** → `Entering activity: TextSettings`
- Night mode: **(410, 240)** and **(470, 240)** toggle ON/OFF in place
  (`GFX` only; no activity change)

After adding a bookmark, a Bookmarks row appears above Toggle
Bookmark. **(340, 240)** then opened `EpubReaderBookmarks`. `BACK`
from Bookmarks returns to **EpubReaderMenu**, not the reader.

Do not trust snapshot-described X for these rows; two descriptions
put Text Settings at 470, which is Night mode.

## Keys on a button board

- `ENTER` confirms
- `BACK` leaves or moves focus to a tab bar
- `UP` / `DOWN` move a list
- `LEFT` / `RIGHT` turn reader pages (not menu rows)

On Settings / Text Settings, focus starts on the tab bar; `ENTER`
there advances the tab.

A fresh home (no recents) highlights Browse Files and confirms with
Select. After a book has been opened, home shows a cover card and
confirm becomes Resume — `ENTER` reopens that book. Snapshot before
assuming Browse Files is still the default.

`sample_book: false` leaves no `fs_/books/`; FileBrowser shows
"No files found".

## Do not starve observe

The inbound queue is 32 envelopes and drops overflow. Heartbeats are
frequent. This server drops a heartbeat before a log when the queue is
full, and a non-empty `set_session_view` mask is applied at enqueue so
masked heartbeats never occupy a slot. Still call `observe` right after
inject if you need the completion lines. Prefer `set_session_view`
paths `log`, `input_ack`, `input_observed` while driving, and read
generation from `get_instance`. Heartbeats still arrive on the wire so
`lastHeartbeat` advances even when observe omits them.

`observe` waits when you pass `until_log` (substring of `log.text`)
and/or `until_generation_gt` (succeeds if
`lastHeartbeat.framebufferGeneration` is greater). Either condition
is enough. `wait_ms` is the timeout; omit it to use
`--observe-wait-ms` / `CSM_OBSERVE_WAIT_MS` (default 8000). `wait_ms: 0`
is a one-shot drain. The response includes `matched` and `timedOut`.
Do not busy-loop with sleeps.

## Spawn and rebuild

`start_instance` launches the operator `--simulator` binary; it does
not build firmware. Rebuild the consuming firmware `program` in place,
then start a **new** instance. Do not stop this MCP/Session process to
pick up a new `program`. This agent cannot restart the proxy; queue
follow-up work around a human restart. Operators start the test proxy
with `cargo xtask start-csm-proxy` (HTTP MCP by default).

To switch X4 → X4 Pro without a proxy restart, set
`-UFREEINK_DEVICE_X4` and `-DFREEINK_DEVICE_X4PRO=1` on the same
`[env:simulator]` the proxy already execs, `pio run -e simulator`,
then `start_instance` with a new id. Confirm `Register.boardId` is
`x4_pro` and `capTouch` is true before tapping.

`sample_book` (default true) seeds `fs_/books/CrossPoint-Reader.epub`.
Pass `headless: false` when a human should see the SDL window.
Pass `auto_sleep: true` only when you want the firmware 10-minute
idle sleep; default is off (never-sleep settings) for spawned instances.
