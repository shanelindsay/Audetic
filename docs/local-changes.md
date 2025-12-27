# Local changes (Shane)

last_verified: 2025-12-27

## Summary

- Clipboard/paste tweaks for KDE: force `text/plain`, prefer Ctrl+Shift+V, keep fallbacks (Shift+Insert, Ctrl+V, wtype).
- KDE notifications via `notify-send` (auto-dismiss, 3s), with explicit icons and status text.
- Recording notification now uses a custom red mic icon file (see below).

## Files touched

- `src/text_io/mod.rs`
  - Force `wl-copy` to `text/plain` for clipboard and primary selection.
  - Prefer Ctrl+Shift+V paste on Wayland/KDE, with Shift+Insert and Ctrl+V fallback.
- `src/ui/mod.rs`
  - Add KDE notification support using `notify-send`.
  - Use a custom red mic icon for recording notifications.

## Local assets (not in repo)

- `/home/sl/.local/share/icons/audetic-mic-red.svg`

## Notes

- API keys are stored outside the repo (e.g. `~/.config/chezwizper/.env`), not committed here.
