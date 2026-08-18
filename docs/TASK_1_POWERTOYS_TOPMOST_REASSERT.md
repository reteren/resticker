# Task 1/3 — PowerToys-port: topmost reassert backstop (round: "PowerToys-port")

## Goal
Port PowerToys "Always On Top" reassert mechanism into `rst-win32` so the
coordinator (parallel task, owns `overlay_manager.rs`) can stop relying on
continuous reactive-correction loops and use one-shot, event-driven topmost
backstop — the proven PowerToys model.

## Verified against real PowerToys source
Read `src/modules/alwaysontop/AlwaysOnTop/AlwaysOnTop.cpp` (main branch). The
coordinator's research is accurate:
- `PinTopmostWindow` = `SetProp(hwnd, WINDOW_IS_PINNED_PROP, 1)` +
  `SetWindowPos(hwnd, HWND_TOPMOST, 0,0,0,0, SWP_NOMOVE|SWP_NOSIZE)` — one call,
  NO resize, NO position change.
- `UnpinTopmostWindow` = `RemoveProp` + `SetWindowPos(HWND_NOTOPMOST, SWP_NOMOVE|SWP_NOSIZE)`.
- Reassert: in `HandleWinHookEvent` case `EVENT_OBJECT_FOCUS`:
  `for each tracked window: if (!IsTopmost(window)) PinTopmostWindow(window);`
  — a pure style check + one-shot correction, no timer.
- Additional re-pin on `EVENT_SYSTEM_MINIMIZEEND` (PowerToys#17332: "in some
  cases topmost flag stops working" after restore).

## pin/unpin already match PowerToys — NO changes needed
`WindowPins::pin` (window_pin.rs:134) already does `SetPropW(marker)` +
`SetWindowPos(HWND_TOPMOST, SWP_NOMOVE|SWP_NOSIZE|SWP_NOACTIVATE|SWP_NOOWNERZORDER)`;
`unpin` (window_pin.rs:175) is the symmetric `HWND_NOTOPMOST` + `RemovePropW`.
The "shrink fullscreen window to 90%" clamp is a SEPARATE coordinator call
(`pin_window` in overlay_manager.rs) — not inside `WindowPins`; the parallel
coordinator task is removing that, not us.

## New method (crates/rst-win32/src/window_pin.rs, after `restore_slot`)
```rust
pub fn reassert_topmost_if_needed(&self, hwnd: HWND) -> bool
```
- If `hwnd` is dead (`IsWindow == false`) → `false` (no-op, no panic — module
  convention "missing elements are not a panic").
- Reads `GWL_EXSTYLE`; if `WS_EX_TOPMOST` already set → `false` (no correction).
- Else re-issues the same one-shot `SetWindowPos(HWND_TOPMOST, SWP_NOMOVE|
  SWP_NOSIZE|SWP_NOACTIVATE|SWP_NOOWNERZORDER)` as `pin`; returns whether the
  corrective call was issued AND succeeded (UIPI-denied → `false`, window left
  as-is).
- Pure primitive: does NOT check the `pinned` book (same convention as
  `enforce_slot`/`move_resize`); the coordinator only calls it for windows it
  considers pinned. Return `bool` is available for redraw/log/border-flash
  (task 3) signals.

## Event decision: REUSE existing EVENT_SYSTEM_FOREGROUND — no new hook
`crates/rst-win32/src/window_tracker.rs` already registers
`SetWinEventHook(EVENT_SYSTEM_FOREGROUND, EVENT_SYSTEM_FOREGROUND, ...)`
(install_hooks, ~line 342) and `classify_event` maps FOREGROUND →
`PendingOp::NeedsFull` → full window snapshot → `WindowEvent::Changed(Vec<WindowInfo>)`
emitted per debounced foreground change. The coordinator already consumes it as
`OverlayMessage::Windows(TrackerWindowEvent::Changed(windows))` and calls
`maintain_pinned_windows` on EVERY such snapshot (overlay_manager.rs:2769-2785).

Why NOT a second `EVENT_OBJECT_FOCUS` (0x8005) hook:
- PowerToys' topmost-loss cases are covered by our EXISTING ranges, which give
  MORE reassert opportunities than PowerToys' bare FOCUS hook: we also emit
  full snapshots on `EVENT_SYSTEM_MINIMIZEEND` (the documented topmost-loss
  quirk, PowerToys#17332), `EVENT_OBJECT_LOCATIONCHANGE` (cached windows),
  `EVENT_OBJECT_SHOW/HIDE`, `EVENT_OBJECT_DESTROY`, plus FOREGROUND.
- The ONLY case FOCUS adds over FOREGROUND is focus moving between child
  controls of the SAME already-foreground window — which cannot clear a
  window's `WS_EX_TOPMOST` style by itself (style is only cleared by
  minimize/restore quirks or explicit app calls, both covered by other events);
  a z-order REORDER without any of those events would be missed by PowerToys
  too (its check is style-based, same as ours).
- Live probe on this machine (Windows 11 24H2, build 26200): minimize/restore
  does NOT clear topmost at all (`[reassert live] ... lost topmost: false`), so
  the documented quirk isn't even reproducible here — the backstop's real value
  is external knockout (other apps raising/clearing), which fires FOREGROUND.
- Cheaper and lower-risk: one hook to reason about, consistent with the
  codebase's stated hook-reuse pattern (module header already notes FOREGROUND
  is "reused" for occlusion + pin-focus-surfacing).

## Where the coordinator wires it (for the parallel task, no re-derivation needed)
Inside `maintain_pinned_windows` (crates/resticker/src/overlay_manager.rs, ~5743),
in the existing `for pinned in &edit.pinned_windows` loop (already runs on every
`OverlayMessage::Windows(Changed)` snapshot, gated off edit-mode): for each
pinned window WITHOUT neighbor rules / move-lock (plain topmost pin), call
```rust
window_pins.reassert_topmost_if_needed(win_hwnd);
```
(i.e. the same loop body where `enforce_slot`/`enforce_move_lock` are called
today — that loop already iterates every pinned window per snapshot). The
`bool` return can drive the border-flash (task 3).

## Tests (all in window_pin.rs)
Deterministic unit tests (no real desktop needed):
- `reassert_is_noop_when_already_topmost`
- `reassert_restores_knocked_out_topmost` (+ second call is no-op)
- `reassert_dead_window_is_noop`
- `reassert_unpinned_window_can_be_restored_as_primitive` (pin book untouched)

Live-desktop `#[ignore]` test (`reassert_topmost_restores_after_external_knockout_live`,
runs with `--ignored`): real window on its own pump thread → pin → no-op when
topmost → simulate external knockout (`SetWindowPos(HWND_NOTOPMOST,...)`) →
backstop detects + restores → minimize/restore probe (logs platform behavior:
`lost topmost: false` on 26200) → unpin clears topmost.

## Verification status
- `cargo test -p rst-win32 --lib`: 171 passed, 1 failed —
  `single_instance::second_acquire_in_same_process_sees_already_running`,
  which FAILS ONLY because a live resticker.exe (PID 40384) is running and holds
  the `resticker_single_instance` mutex — purely environmental, unrelated to
  this change (single_instance.rs untouched; test inherently fails whenever any
  resticker.exe runs). All reassert tests + the ignored live test pass.
- `cargo clippy --workspace --all-targets -- -D warnings`: clean.
- `cargo build --release -p resticker`: compiles (built into an alternate
  CARGO_TARGET_DIR because the running resticker.exe locks target/release;
  same environmental condition).