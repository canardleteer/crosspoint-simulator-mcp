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

## Loop

1. `inject_*` to act.
2. `observe` immediately to know the action landed.
3. `request_snapshot` from time to time to confirm you are on the screen
   you think you are: after a new activity, after a few list moves,
   before a tap or a book open, and whenever the highlight or path
   might have drifted. Use the snapshot to read labels and hit targets,
   not as a wait.

`get_instance` carries `lastHeartbeat.framebufferGeneration` (and
`headless`) and `Register`. Proto3 JSON omits false bools: `capTouch` /
`capHome` / `capFrontlight` absent means false. Several instances may
be up at once; always name `instance_id`.

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

## Touch when the board has it

Read `Register.capTouch` (and `capHome`) after connect. When
`capTouch` is true, tap the control you want (`inject_touch` kind 3)
or swipe; do not walk menus with a planned UP/DOWN/ENTER sequence.
Coordinates are **panel pixels** on the framebuffer
(`Register.width` × `Register.height`, 800×480 on X4), not the rotated
SDL window.

The default X4 profile has no touch and no Home (`inject_touch` /
`inject_home` return `no_touch` / `no_home`); then keys are the
Session path. X4 Pro and Sticky have touch. That is a firmware compile
flag (`SIMULATOR_DEVICE_X4_PRO` / `FREEINK_DEVICE_X4PRO`), not an MCP
parameter.

## Keys on a button board

- `ENTER` confirms
- `BACK` leaves or moves focus to a tab bar
- `UP` / `DOWN` move a list
- `LEFT` / `RIGHT` turn reader pages (not menu rows)

In the reader, `ENTER` opens `EpubReaderMenu` (Text Settings, chapters,
Go Home) — shorter than Home → Settings. On Settings / Text Settings,
focus starts on the tab bar; `ENTER` there advances the tab.

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
generation from `get_instance`.

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
follow-up work around a human restart.

`sample_book` (default true) seeds `fs_/books/CrossPoint-Reader.epub`.
Pass `headless: false` when a human should see the SDL window.
Pass `auto_sleep: true` only when you want the firmware 10-minute
idle sleep; default is off (never-sleep settings) for spawned instances.
