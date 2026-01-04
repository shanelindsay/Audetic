# Local changes (Shane)

last_verified: 2026-01-04

## Summary

- Clipboard/paste tweaks for KDE: force `text/plain`, prefer Ctrl+Shift+V, keep fallbacks (Shift+Insert, Ctrl+V, wtype).
- KDE notifications via `notify-send` (auto-dismiss, 3s), with explicit icons and status text.
- Recording notification now uses a custom red mic icon file (see below).
- External toggle script plays three tones (start/stop/complete) and supports a send‑enter mode (see below).
- keyd mappings updated for AL68 device ID changes and i4 middle‑click tap/hold toggle (see below).

## Files touched

- `src/text_io/mod.rs`
  - Force `wl-copy` to `text/plain` for clipboard and primary selection.
  - Prefer Ctrl+Shift+V paste on Wayland/KDE, with Shift+Insert and Ctrl+V fallback.
- `src/ui/mod.rs`
  - Add KDE notification support using `notify-send`.
  - Use a custom red mic icon for recording notifications.

## Local assets (not in repo)

- `/home/sl/.local/share/icons/audetic-mic-red.svg`

## Local system changes (outside repo)

These are kept outside the repo but documented here for reproducibility.

- Toggle script with audio cues:
  - `/home/sl/.local/bin/audetic-toggle.sh`
  - Beeps: start/stop/complete (speaker-test/paplay/pw-play/aplay).
  - Optional `--send-enter` mode waits for a new Audetic history entry, then sends Enter.
- keyd mapping (AL68 consumer control + keyboard interface):
  - `/etc/keyd/al68.conf` → `mute` maps to `audetic-toggle.sh`
  - `/etc/keyd/al68-kbd.conf` → `leftcontrol+capslock` maps to `audetic-toggle.sh --send-enter`
  - AL68 device IDs now include both `4d4b:304e` (SmartCloud) and `056d:c077` (I‑CHIP/YUNZII 2.4G).
  - `keyd` service must be running.
- keyd mapping (i4 mouse, tap/hold):
  - `/etc/keyd/i4-mouse.conf` (ids: `m:32c2:0015`)
  - `middlemouse` tap → `audetic-toggle.sh`
  - `middlemouse` hold (~350ms) → `audetic-toggle.sh --send-enter`
  - Implemented with `timeout(...)` in keyd.

## Notes

- API keys are stored outside the repo (e.g. `~/.config/chezwizper/.env`), not committed here.

## Failed attempts (rolled back)

last_attempt: 2025-12-29

- Tried to add an alternate keybind (`=`) for `--send-enter`; keyd did not fire the binding reliably.
- Enter-after-transcription remains unreliable on KDE/Wayland with the current tooling.

## Failed attempts (rolled back)

last_attempt: 2025-12-29

- Tried to implement a built-in `send_enter` after auto-paste; caused paste regressions and did not reliably send Enter.
- Attempted keyd and Audetic-native methods (wtype/ydotool); behavior was inconsistent under Wayland focus rules.
- All changes reverted; current setup remains the external toggle script + keyd bindings.
