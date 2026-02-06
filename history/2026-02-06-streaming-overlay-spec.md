# Audetic Personal MVP Plan

Date: 2026-02-06
Branch: `feat/streaming-overlay-mvp`
Goal: fast live transcript + nice overlay UI + clickable taskbar launcher icon

## 1. What We Are Building

A lightweight personal-use streaming mode with:

1. Mistral realtime streaming transcription.
2. A simple always-on-top overlay window showing:
   - live partial text
   - final text lines
   - volume meter
3. Final text commit once per segment (clipboard by default).
4. A desktop/taskbar launcher icon you can click to start Audetic.

No production hardening. No multi-provider abstraction beyond what we need.

## 2. Keep It Simple Rules

1. Single user, single active session.
2. No schema migrations.
3. No complex retry/state recovery logic.
4. No request-level mode override in API.
5. If streaming fails, show error and let user restart.

## 3. MVP User Flow

1. Click Audetic icon in taskbar/app launcher (or start service as usual).
2. Press record hotkey.
3. Overlay appears and updates while speaking.
4. Final segments are copied to clipboard (or injected once if configured).
5. Press hotkey again to stop.

## 4. Technical Scope

## 4.1 Streaming Path

1. Add one streaming provider: `mistral_realtime`.
2. Convert mic audio to mono 16k PCM16.
3. Send chunks to Mistral via websocket.
4. Receive partial/final events.

## 4.2 Overlay UI

1. New binary: `audetic-overlay`.
2. Minimal native UI with:
   - transcript area
   - horizontal volume meter
   - session state text
3. Connect to local SSE endpoint.

## 4.3 Event Transport

1. Add `GET /stream/events` (SSE).
2. Add `GET /stream/status` (small status JSON).
3. Keep existing `/status` mostly unchanged.

## 4.4 Commit Behaviour

1. `commit_target = "clipboard"` default.
2. Optional `commit_target = "text_io"` for one-shot injection of final segments.
3. Never inject partial text.

## 4.5 History Persistence (simple)

1. Save one history row per session at stop.
2. Join final segments with newlines.
3. Use `audio_path = "stream://<job_id>"`.

## 5. Taskbar/App Launcher Icon

Add a desktop launcher so you can click to launch.

## 5.1 Files to add

1. `assets/linux/audetic.desktop`
2. `assets/linux/audetic.png` (or svg icon)

## 5.2 Desktop entry template

```ini
[Desktop Entry]
Type=Application
Name=Audetic
Comment=Voice to text streaming
Exec=audetic
Icon=audetic
Terminal=false
Categories=Utility;AudioVideo;
StartupNotify=true
```

## 5.3 Install location (user scope)

1. Desktop file: `~/.local/share/applications/audetic.desktop`
2. Icon: `~/.local/share/icons/hicolor/256x256/apps/audetic.png`
3. Refresh: `update-desktop-database ~/.local/share/applications || true`

## 5.4 Optional tray indicator

Not required for MVP. We can add later if wanted.

## 6. Minimal Config Additions

```toml
[streaming]
enabled = false
provider = "mistral_realtime"
api_key = ""                      # fallback to MISTRAL_API_KEY env
sample_rate_hz = 16000
chunk_ms = 20
silence_timeout_ms = 700
commit_target = "clipboard"       # clipboard|text_io|none

[overlay]
enabled = true
url = "http://127.0.0.1:3737/stream/events"
always_on_top = true
width = 560
height = 220
opacity = 0.94
show_meter = true
```

## 7. Implementation Steps

1. Phase A: add streaming events + audio level meter + SSE route.
2. Phase B: add `mistral_realtime` provider and wire live session loop.
3. Phase C: add overlay binary and basic rendering.
4. Phase D: add desktop launcher/icon install logic.
5. Phase E: docs + quick manual QA.

## 8. Manual QA Checklist (Short)

1. Start audetic, toggle recording, partial text appears quickly.
2. Meter moves with voice and clips on loud input.
3. Final text copied to clipboard.
4. Stop/start works repeatedly.
5. Clicking Audetic launcher starts app.

## 9. Estimate

1. Core streaming + overlay: 3-5 days.
2. Launcher/icon + polish: 0.5-1 day.
