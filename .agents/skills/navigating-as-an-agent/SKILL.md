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

After each new screen or missed tap, add the working **logical** hit (and
what failed) to **Device profiles** or **Touch** in this file so the
next session does not rediscover it. Label every numeric hit with
`boardId`, theme, and orientation. Those pixels are **not** portable
across boards, themes, firmware builds, or settings (optional rows,
`touchReaderControls`, recents). Recompute from `Register` and the
theme grid; treat listed coordinates as one confirmed session, not a
universal map.

Hits below were confirmed on **X4 Pro + Lyra + Portrait** (logical
480×800, panel 800×480) against CrossPoint Reader `Register.version`
`dev-simulator` (v1.5.0-line firmware). `wait_mode=paint` unless noted.

## Loop

1. `inject_*` to act. Pick the inject from `Register` (see **Device
   profiles**): tap when `capTouch` is true; keys when it is not.
   Touch/swipe `x`/`y` are **logical** pixels by default. `wait_mode`
   defaults to **paint**: the tool returns after `UiResult`
   (`painted`, `generation`, `activity`). A miss is `painted: false`.
2. `observe` only when the next inject depends on a value you do not
   already have (`activity`, `readerPage`, a label). Prefer
   `until_activity`, `until_progress_page`, or `until_generation_gt: 0`
   (bump from the current heartbeat). Do **not** use `until_log` as a
   barrier. After a known hit that painted, fire the next inject.
   `wait_ms` is a miss ceiling (about **400** for a tap). First EPUB
   open, a chapter jump, and a cache rebuild are the exceptions (index;
   a few seconds; raise inject `wait_ms` above the 2s reply timeout).
   Do not use the process default 8000 as a per-tap wait.
3. `request_snapshot` only when you need a **label** for the next tap
   or the path may have drifted. Do not snapshot after a known hit
   that already painted. Snapshots are not a wait and not the source
   of coordinates (they are often described rotated).

## Drive fast

- Use **logical** taps and `wait_mode=paint`. Do not convert through
  panel math. Example (X4 Pro, Lyra, Portrait): Browse Files
  `(240, 350)` then first row `(240, 170)` then the EPUB. The EPUB tap
  already returns `activity: EpubReader` and `painted: true` when the
  reader first paints; wait only if you need a page:
  `until_progress_page` or `until_generation_gt: 0`.
- `inject_batch` for a known chain. Each paint-wait uses `UiResult`.
  Stop on the first rejection. Two next-page taps in one batch both
  painted (pages 1 then 2). Firmware already queues a second page
  turn inside 200ms.
- After paint, read `generation` / `activity` on the inject result.
  Do not `observe` just to learn whether it painted.
- `observe` / `get_instance` `activity` and `readerPage` come from
  `lastHeartbeat` and can lag until the next heartbeat. An
  `until_activity` match can return on a drained `Entering activity:`
  line while the echo still names the previous screen. Trust the
  inject result, or the drained `ACT` line plus `matched`.
- After a **stack pop** (`BACK` from Text Settings; chapter list
  returning to the reader) there is no `Entering activity:` for the
  screen you landed on. `Heartbeat.activity` stays at the last
  **Entering** name. Do not `until_activity` the parent.
- Exclude `MEM` (and `SCT` when not opening a book) via
  `set_session_view` `exclude_log_components` so those lines never
  occupy the queue.
- Keys on a touch board are a fallback, not the fast path.
- First EPUB open is the one slow firmware path (index + cache).
  Resume is fast when the cache is intact. Changing font size can
  invalidate it (`Cache not found` / `Partial cache found`).

`get_instance` carries `lastHeartbeat.framebufferGeneration` (and
`headless`) and `Register`. `boardId` is `x4`, `x3`, `x4_pro`,
`sticky`, or `paper_mono`. `capFrontlight` is any frontlight style
(including Paper Mono PMIC), not PWM-only. Proto3 JSON omits false
bools and zeros: `capTouch` / `capHome` / `capFrontlight` absent
means false; `readerPage` 0 may be omitted. Several instances may
be up at once; always name `instance_id`.

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
- **Touch (X4 Pro / Sticky / Paper Mono):** one tap on the label in
  **logical** pixels (MCP default). Confirm with `UiResult.painted` or
  `until_activity`. A miss is `painted: false` (Ack still happened).
- **Home key boards (X4 Pro):** `inject_home` is faster than repeated
  `BACK` when you only need Home. `inject_home` while already on Home
  is `painted: false`. `BACK` from Text Settings returns to Settings
  without an `Entering activity: Settings` line (stack pop).
  `inject_home` from Text Settings that was pushed from the reader
  exits TextSettings **and** EpubReader, then Home.
- **Reader pages (default `touchReaderControls=TOUCH_READER_ON`):** tap
  the **right third** for next, **left third** for previous. A logical
  swipe is accepted and observed but `painted: false` and does **not**
  turn the page until Settings → Controls sets Swipe. Do not use
  `UP`/`DOWN` for page turns. Button boards still use `LEFT`/`RIGHT`.
- **Reader chrome:** on a button board, `ENTER` opens
  `EpubReaderMenu`. On touch, tap the **center third**
  (`isTouchMenuTap`) — logical **(240, 400)** on 480×800 Portrait.
  From that menu, Text Settings is shorter than Home → Settings →
  Reader. Snapshot the menu before tapping: Footnotes, Bookmarks,
  and Frontlight rows are optional and shift later hits.

## Paint vs ack

Default `wait_mode=paint` waits for `UiResult` on the same corr:
`painted` is true when framebuffer generation increased after the
inject was queued, false after a ~400ms miss. `wait_mode=ack` is the
old `InputAck` (queued, not painted). `wait: false` enqueues only.
The session reply timeout is **2s**; a chapter jump or cache rebuild
that indexes past that returns `timed out waiting for session reply`
even though the tap was applied. Raise `wait_ms`, or `wait: false`
then `until_progress_page` / `until_generation_gt: 0`.

Useful `observe` lines (need firmware `LOG_LEVEL` ≥ 2 so `LOG_DBG` is
compiled in):

- `ACT` `Entering activity: <Name>` / `Exiting activity: <Name>` /
  `Pushed to activity stack` / `Popped from activity stack` — different
  activity. `Exiting` does not clear `Heartbeat.activity`.
- `GFX` `Time = N ms from clearScreen to displayBuffer` — a frame was
  submitted
- `ERS` `Rendered page in Nms` and `Progress saved: … page=N` — reader
  page turn finished
- `heartbeat.framebufferGeneration` increased — SDL presented a new
  frame (read `get_instance` if heartbeats are masked out of `observe`)
- After `start_instance`, wait for `until_activity: Home` before
  driving the home menu (`Register` is earlier, during Boot)
- Opening an EPUB: inject may already report `EpubReader` + painted;
  `EBP Loaded ePub` then `ERS Rendered page` follow (first open also
  logs `EBP Total indexing` and `ERS Cache not found, building…` /
  `SCT Page N processed`)
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

Hits are **logical** unless marked `space=panel`. Do not reuse them on
X3 (792×528), a different theme (RoundedRaff / Base), Landscape, or
after firmware layout changes. Prefer theme metrics +
`Register.width`/`height`/`boardId`, then confirm with `ACT` / `GFX`.

Read `Register.capTouch` (and `capHome`) after connect.

`inject_touch` `x`/`y` default to **logical** pixels (firmware
`GfxRenderer` width × height). Pass `space=panel` for framebuffer
pixels (`Register.width` × `Register.height`). They are not SDL
window pixels and not the rotated snapshot description. `kind` 3 is a
tap. Observed `touch.nx` / `ny` stay panel-normalized.

On X4 / X4 Pro home the usual orientation is **Portrait**: logical
480×800, panel 800×480. Heartbeat.orientation is the GfxRenderer enum
(0=Portrait, 1=LandscapeClockwise, 2=PortraitInverted,
3=LandscapeCounterClockwise). The simulator converts logical→panel.

Portrait conversion if you must reuse an old panel hit with
`space=panel`, or convert it: `logical_x = (panel_height - 1) -
panel_y`, `logical_y = panel_x`. New driving should use logical.

A tap that is accepted but misses the widget returns `painted: false`.
Snapshot after a miss; nudge ~20–40 logical px. Do not re-derive
Portrait panel math.

### Lyra home (X4 Pro, no recents)

Theme: `homeTopPadding=56`, `homeCoverTileHeight=242`,
`homeMenuTopOffset=16`, `menuRowHeight=64`, `menuSpacing=8`.

- `menuTop` = 56+242+16 = **314** (logical y)
- `rowStep` = 64+8 = **72**
- Rows (no OPDS, no Continue Reading): 0 Browse Files, 1 Recent Books,
  2 File Transfer, 3 Settings
- Browse Files (row 0) **(240, 350)** → `FileBrowser`
- Recent Books (row 1) **(240, 422)** → `RecentBooks`. First row
  **(240, 170)** reopens the sample EPUB (same first-row hit as
  FileBrowser).
- File Transfer (row 2) **(240, 494)** → `CrossPointWebServer` then
  `NetworkModeSelection` (Join a Network / Calibre Wireless / Create
  Hotspot). `inject_home` exits both.
- Settings (row 3) **(240, 562)** → `Settings`
- **(240, 700)** is below the menu; `painted: false`, no redraw

After a book has been opened, Home shows a cover card (may log
`No known cover image for thumbnail`). Menu row math is unchanged.
Cover tile is `homeTopPadding`…`+homeCoverTileHeight` (56–298).
Resume tap: **(240, 177)** → `EpubReader`. Faster than Browse Files
when the cache is intact (`ERS Cache found, skipping build`).

`HomeActivity` uses `rowTouch` on the full width; logical x can stay
240.

### FileBrowser (X4 Pro, Lyra)

First list row (folder or file) is **(240, 170)**. That opened `books`
at `/` and `CrossPoint-Reader.epub` under `/books`. Snapshot
descriptions often guess a higher first-row Y; prefer **170**.
First open: inject returns `EpubReader` + painted; `EBP Loaded ePub`
and `ERS Rendered page` follow (`ERS Cache not found` / `SCT Page N
processed` while indexing). `until_activity: EpubReader` may already
be true from the inject.

### Settings (X4 Pro, Lyra)

Tabs are Display, Reader, Controls, System (`UiTabListActivity`).
Font size is not on Display. Reader list hides per-field font size
behind **Text Settings**.

Confirmed logical hits (X4 Pro, Lyra, Portrait 480×800):

- Display tab (1st): **(99, 110)**
- Reader tab (2nd): **(189, 110)**
- Controls tab (3rd): **(279, 110)** — shows `Touch Reader Controls:
  Tap` by default
- System tab (4th): **(369, 110)** — GFX only this session; confirm
  with a snapshot if the next tap needs a System label
- Text Settings row (first Reader list row): **(240, 175)** →
  `TextSettings`
- Touch Reader Controls row (Controls, 2nd list row): **(240, 215)**
  opens an option popup (OFF / Tap / Swipe / Inverted Tap). Snapshot
  popup options; confirm the value on the list after the popup closes.

### Text Settings (X4 Pro)

Preview pane sits **above** the tab bar. Do not reuse Settings-tab Y
for these tabs.

- `tabTop` ≈ 297 (after header + preview). Tabs: Font | Size | Layout | Style
- Size tab: **(129, 317)**
- Size list rows (after tab bar): 12 / 14 / 16 / 18 pt
- **16 pt**: **(240, 525)** — `FDC Prewarm: … glyphs` is a good
  size-change signal (not only `GFX`). This can invalidate the EPUB
  cache; the next open may rebuild.

BACK from Text Settings returns to Settings (no `Entering activity:
Settings` line; stack pop). Inject `activity` may still say
`TextSettings`. `inject_home` from Settings logs `Exiting activity:
Settings` then `Entering activity: Home`.

### Reader (X4 Pro, default tap zones)

`SETTINGS.touchReaderControls` defaults to `TOUCH_READER_ON` (tap),
not `TOUCH_READER_SWIPE`. Outer logical thirds (width/3 of 480 ≈ 160):

- Next page: **(400, 400)** — `Progress saved: … page=N`
- Previous page: **(80, 400)** — `Progress saved: … page=N`
- Menu: center third in **both** axes → **(240, 400)** →
  `EpubReaderMenu`

A swipe from **(400, 400)** to **(80, 400)** (`duration_ms` 250) is
`painted: false` and does **not** emit `ERS`.

Two next-page taps in one `inject_batch` both painted. Firmware
`kMinManualTurnGapMs` is 200; a second tap inside that window is
queued (`pendingManualTurn`), not lost. Some pages log
`GFX !! Outside range` while still saving progress — treat
`Progress saved` as success.

Chapter list (`EpubReaderChapterSelection`): first-row math like
FileBrowser. **(240, 170)** pops the list and starts a jump (may
`SCT` index). That can exceed the 2s paint wait. Returning to the
reader is a stack pop — no `Entering activity: EpubReader`. Wait with
`until_progress_page` or `until_generation_gt: 0`.

### EpubReaderMenu (X4 Pro)

Rows are optional: Footnotes if the book has them; Bookmarks only
after at least one bookmark exists; Frontlight when
`Register.capFrontlight` is true. Snapshot after the row set
changes. Empirical row step this session was about **68** logical y
(not the 40px `listRowHeight`).

This firmware + sample EPUB, no Footnotes, before any bookmark:

- Select Chapter: **(240, 165)** → `EpubReaderChapterSelection`
- Toggle Bookmark: **(240, 233)** — `ERS Toggle bookmark` then return
  to the reader (menu pops)
- Text Settings: **(240, 300)** → `TextSettings`

Visible rows this session (snapshot): Select Chapter, Toggle
Bookmark, Text Settings, Night mode, Frontlight, Look Up, Reading
Orientation, Auto Turn, Go to %. After adding a bookmark, a
Bookmarks row appears and later hits shift ~+68 logical y.
`BACK` from Bookmarks returns to **EpubReaderMenu**, not the reader.

Do not trust snapshot-described X/Y for these rows; descriptions are
often rotated.

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

## Observe and the inbound queue

The inbound queue is 128 envelopes. Eviction drops newest heartbeats
first, then `MEM`/`SCT` logs, then other logs, then oldest.
**Never dropped:** `InputAck`, `UiResult`, `SnapshotFrame` /
`SnapshotError`, and logs with component `ACT` or `ERS`.

A non-empty `set_session_view` mask is applied at enqueue so masked
heartbeats never occupy a slot. Heartbeats still arrive on the wire so
`lastHeartbeat` advances. Prefer paths `log`, `input_ack`,
`input_observed`, `ui_result` while driving, and
`exclude_log_components: ["MEM"]` (add `SCT` when not indexing).

`observe` always echoes `generation`, `activity`, `readerPage`, and
`readerSpine` from `lastHeartbeat`. Those echoes can lag the inject
you just finished (next heartbeat). Until-conditions:
`until_activity` (exact Heartbeat.activity, or `Entering activity:
{name}` in drained logs), `until_progress_page` (Heartbeat.reader_page
or `page=N` on `Progress saved`), `until_generation_gt` (greater than
this; **0 means current generation**), and `until_log` (substring).
Any until is enough. `matched` / `timedOut` appear **only** when an
until-condition was set. `wait_ms` is a miss ceiling (about 400 for a
tap; longer only for first EPUB open, chapter jump, or cache rebuild).
`wait_ms: 0` is a one-shot drain. A match returns on the next 25ms
poll. Do not busy-loop with sleeps.

`until_progress_page` can match a **queued** `Progress saved` from an
earlier tap. Drain (`wait_ms: 0`) or use a page you have not reached
yet.

## Host surface

These Session/MCP fields are the driving API (not a wishlist):

- `inject_*` `wait_mode=paint` (default) → `UiResult` (`painted`,
  `generation`, `activity`). `wait_mode=ack` → `InputAck`.
- Touch/swipe `space=logical` (MCP default) or `space=panel`.
- `inject_batch` sequential steps; stop on first rejection.
- `Heartbeat.activity`, `readerSpine`, `readerPage`, `orientation`.
- `observe` until: `until_activity`, `until_progress_page`,
  `until_generation_gt: 0`, `until_log`.
- `SetSessionView.exclude_log_components`.
- Queue 128; never-drop ACT/ERS/acks/`UiResult`/snapshots.

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
