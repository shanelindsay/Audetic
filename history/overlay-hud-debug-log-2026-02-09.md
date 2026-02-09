# Overlay HUD Debug Log (2026-02-09)

## Goal
Make the overlay behave like a lightweight HUD:
- Show while recording/processing
- Hide when idle
- Avoid focus stealing
- Keep tray click as stable toggle

## What Was Changed
1. Tray toggle path
- Reworked tray click and menu toggle handlers.
- Replaced problematic tray HTTP call path that caused runtime panics.

2. Overlay visibility state machine (multiple iterations)
- Added phase-based show/hide logic (`recording/processing` vs `idle`).
- Added hide delay logic.
- Added startup/first-frame visibility guards.
- Added hard-hide attempts (`Visible(false)` and minimize toggles).
- Added fallback status sync polling from `/status`.

3. Overlay lifecycle
- Tried service-based overlay lifecycle (`audetic-overlay.service`) to remove spawn races.
- Decoupled tray from overlay spawning during this attempt.

## Observed Issues
1. Inconsistent compositor/window behaviour
- Overlay sometimes stayed visible in idle despite phase changes.
- Overlay sometimes became permanently hidden after aggressive minimize/hide commands.
- Startup behaviour could show window unexpectedly before first toggle in some runs.

2. State desync symptoms
- Service `/status` reported `idle`, but visible state could still drift.
- SSE stream and window state did not always align in a stable way across iterations.

3. Reliability regression risk
- Repeated visibility experiments increased variability in behaviour, reducing confidence.

## Decision Taken
Reverted to stable baseline behaviour for now.
- Reverted `src/bin/audetic-overlay.rs` to committed state.
- Removed repo unit file `audetic-overlay.service` from working tree.
- Disabled and removed user unit `~/.config/systemd/user/audetic-overlay.service`.
- Restarted `audetic.service` and `audetic-tray.service`.

## Current State After Revert
- `audetic.service`: active
- `audetic-tray.service`: active
- `audetic-overlay.service`: not found/disabled

## Recommendation For Next Attempt
If retrying HUD auto-hide later:
1. Add explicit structured visibility logs (phase, computed visible, viewport command issued).
2. Keep one single source of truth for phase (prefer `/status` only, not mixed SSE + local heuristics).
3. Avoid minimize commands; use only visibility toggles.
4. Ship in small steps with one behaviour change per commit.

## Overlay Code Note
- Main overlay implementation file: `src/bin/audetic-overlay.rs`.
- Tray integration entrypoint: `src/bin/audetic-tray.rs`.
- At the end of this session, overlay/HUD experiments were reverted and only baseline overlay code remains in `src/bin/audetic-overlay.rs`.
- If we retry HUD mode later, start from current baseline and re-implement in small commits (do not reapply old minimize/hard-hide logic).
