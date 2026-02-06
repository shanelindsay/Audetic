# Streaming Transcription + Overlay + Volume Meter

Date: 2026-02-06
Project: `audetic`
Status: Implementation Plan (v3)
Owner: local `audetic` service + overlay client

## 1. Goal

Deliver a genuinely low-latency dictation mode for Linux/Wayland that feels live:

1. Partial transcript appears while speaking.
2. Volume meter updates continuously.
3. Final text commits once per completed segment.
4. Existing batch push-to-talk flow remains intact.

## 2. Success Metrics

1. Time to first partial: p50 <= 350 ms, p95 <= 700 ms.
2. End-of-utterance to final commit: p50 <= 900 ms.
3. Meter refresh: 20-30 Hz.
4. Audio callback never blocks on network I/O.
5. Batch mode regressions: zero functional drift.

## 3. Locked Decisions

1. First streaming provider: `mistral_realtime`.
2. Overlay runs as separate process: `audetic-overlay`.
3. Overlay transport: SSE from existing Axum API (`127.0.0.1:3737`).
4. Partials are never injected into external text fields.
5. Streaming default commit target: `clipboard`.

## 4. Non-Goals (v1)

1. Full-duplex voice assistant (STT + LLM + TTS barge-in).
2. Guaranteed insertion into every Wayland text widget.
3. Multi-provider parity from day one.
4. Schema migration for new workflow tables.

## 5. Current Constraints (From Code)

1. API server already exists (`src/api/mod.rs`) and is local-loopback.
2. Recording lifecycle is batch-oriented (`src/audio/recording_machine.rs`).
3. Provider trait currently returns one final `String` (`src/transcription/providers/mod.rs`).
4. Capture path currently buffers and writes WAV on stop (`src/audio/audio_stream_manager.rs`).
5. Database requires non-null `audio_path` (`src/db/init.rs`, `src/db/operations.rs`).

## 6. Architecture

## 6.1 Runtime Components

1. `LiveSessionController`
   - owns one streaming session lifecycle
   - owns provider connection
   - owns finalisation + commit policy
2. `AudioTap`
   - forwards capture frames without blocking callback
3. `MistralRealtimeProvider`
   - sends PCM chunks
   - emits partial/final/error events
4. `LiveEventHub`
   - bounded fan-out for session events
5. `audetic-overlay`
   - subscribes to SSE
   - renders transcript + meter

## 6.2 Event Model

```rust
pub enum TranscriptEvent {
    Partial {
        text: String,
        is_stable: bool,
        segment_id: Option<String>,
        ts_ms: i64,
    },
    Final {
        text: String,
        segment_id: String,
        ts_ms: i64,
    },
    Error {
        code: String,
        message: String,
        ts_ms: i64,
    },
}

pub struct AudioLevelEvent {
    pub rms_dbfs: f32,
    pub peak_dbfs: f32,
    pub clipping: bool,
    pub ts_ms: i64,
}

pub enum LiveSessionEvent {
    SessionStarted { job_id: String, ts_ms: i64 },
    Transcript(TranscriptEvent),
    AudioLevel(AudioLevelEvent),
    Warning { code: String, message: String, ts_ms: i64 },
    SessionStopped { job_id: String, ts_ms: i64 },
}
```

## 6.3 SSE Wire Protocol

Endpoint: `GET /stream/events`

SSE envelope:

1. `id`: monotonic per-session integer.
2. `event`: one of `session_started|partial|final|audio_level|warning|error|session_stopped`.
3. `data`: JSON payload.

JSON payload shape:

```json
{
  "job_id": "uuid",
  "ts_ms": 1738839999000,
  "data": { "...": "..." }
}
```

Connection rules:

1. Keepalive comment every 5 seconds.
2. On no active session, stream remains open and only keepalives are sent.
3. On reconnect, client requests fresh state; server does not replay backlog in v1.

## 6.4 Audio Contract

1. Input from CPAL callback may vary by device format.
2. Internal normalisation target: mono `f32`.
3. Provider send format: mono `pcm_s16le` @ 16 kHz.
4. Chunk size default: 20 ms (`320` samples @16 kHz).
5. Conversion pipeline per chunk:
   - downmix (if multi-channel)
   - resample to 16 kHz
   - clamp to [-1.0, 1.0]
   - convert to `i16`

## 6.5 Backpressure Policy

1. Audio callback writes to bounded ring buffer only.
2. Stream sender task drains queue and performs network writes.
3. Queue size: `queue_max_chunks` default `50` (~1s at 20ms/chunk).
4. Overflow policy: drop oldest unsent chunk.
5. Metrics:
   - `stream_chunks_dropped_total`
   - `stream_queue_depth_current`
6. Warning event every 25 dropped chunks.

## 7. Session Lifecycle Semantics

## 7.1 Start

1. `/toggle` starts recording.
2. If streaming enabled, mode = `streaming`.
3. Emit `SessionStarted`.
4. Start provider connection and audio pipeline.

## 7.2 Running

1. Emit `AudioLevelEvent` at meter cadence.
2. Emit partial transcript events as provider deltas arrive.
3. On provider final event, run final commit policy once.

## 7.3 Stop

1. `/toggle` stop marks session as draining.
2. Flush remaining buffered chunks.
3. Wait for final provider close response with timeout.
4. Emit `SessionStopped`.

## 7.4 Failure

1. Provider disconnect or fatal parse error emits `Error` and transitions status to `Error`.
2. User can toggle again to recover without service restart.

## 8. Recording Status Compatibility

Keep existing `RecordingPhase` values for API compatibility.

Mapping:

1. Batch active: `phase=recording`, `mode=batch`.
2. Streaming active: `phase=recording`, `mode=streaming`.
3. Batch post-stop transcription: `phase=processing`, `mode=batch`.
4. Streaming stop drain window: optional brief `phase=processing`, `mode=streaming`.

Add non-breaking status fields:

1. `mode`: `batch|streaming`.
2. `streaming_active`: bool.
3. `stream_counters`: queue depth, dropped chunks, connected clients.

## 9. Persistence Strategy (No Schema Migration in v1)

Constraint: `workflows.audio_path` is required.

Decision:

1. Store one DB row per streaming session, not per segment.
2. Aggregate final segments into a single text block (newline-joined).
3. Use synthetic audio path: `stream://<job_id>`.
4. Save row at session stop.

Rationale:

1. Keeps history behaviour simple.
2. Avoids DB schema changes now.
3. Avoids history spam from per-segment inserts.

## 10. Commit Policy

`commit_target` values:

1. `none`: no external output.
2. `clipboard`: copy each final segment.
3. `text_io`: inject each final segment once.

Additional behaviour:

1. Deduplicate finals by normalised text + `segment_id` in a 1.5s window.
2. If `text_io` fails and clipboard copy enabled, do one paste fallback.

## 11. Config Spec

```toml
[streaming]
enabled = false
provider = "mistral_realtime"
api_key = ""                       # optional; fallback to MISTRAL_API_KEY env
sample_rate_hz = 16000
chunk_ms = 20
queue_max_chunks = 50
min_speech_ms = 250
silence_timeout_ms = 700
max_utterance_ms = 15000
final_dedupe_window_ms = 1500
commit_target = "clipboard"        # none|clipboard|text_io
copy_to_clipboard = true            # used for text_io fallback paste path

[overlay]
enabled = true
url = "http://127.0.0.1:3737/stream/events"
always_on_top = true
width = 560
height = 220
opacity = 0.94
position = "bottom_right"
show_meter = true
reconnect_backoff_ms = 500
```

Key resolution order:

1. `streaming.api_key` (if non-empty).
2. env `MISTRAL_API_KEY`.
3. fail with actionable config error.

## 12. API Changes

1. `GET /stream/events`
   - SSE event stream.
2. `GET /stream/status`
   - returns stream runtime state and counters.
3. `GET /status`
   - extended fields (`mode`, `streaming_active`, `stream_counters`).
4. `POST /toggle`
   - request schema unchanged for v1 (avoid client break risk and scope creep).
   - mode selection comes from config in v1.
   - optional request-level mode override is deferred to v1.1.

## 13. File-Level Work Plan

## 13.1 New Files

1. `src/streaming/mod.rs`
2. `src/streaming/events.rs`
3. `src/streaming/live_event_hub.rs`
4. `src/streaming/audio_level.rs`
5. `src/streaming/audio_convert.rs`
6. `src/streaming/endpointing.rs`
7. `src/streaming/live_session_controller.rs`
8. `src/transcription/providers/mistral_realtime.rs`
9. `src/api/routes/stream.rs`
10. `src/bin/audetic-overlay.rs`

## 13.2 Modified Files

1. `src/api/routes/mod.rs`
2. `src/api/mod.rs`
3. `src/app/mod.rs`
4. `src/config/mod.rs`
5. `src/audio/mod.rs`
6. `src/audio/audio_stream_manager.rs`
7. `src/audio/recording_machine.rs`
8. `src/transcription/providers/mod.rs`
9. `src/transcription/mod.rs`
10. `example_config.toml`
11. `docs/configuration.md`

## 14. Dependency Plan

Planned additions:

1. `tokio-stream` for stream wrappers.
2. `futures-util` for stream combinators.
3. `tokio-tungstenite` for provider websocket.
4. `eframe`/`egui` for overlay UI.

Constraint:

1. Keep total binary bloat reasonable.
2. Pin minimal versions and avoid duplicate runtime stacks.
3. Reuse existing `tokio`; do not introduce a second async runtime.

## 15. Implementation Phases

## Phase 0: Guard Rails (0.5 day)

1. Add regression tests for current batch toggle/status path.
2. Capture baseline latency measurements for current flow.

Exit:

1. Batch tests green.
2. Baseline numbers recorded.

## Phase 1: Streaming Foundations (1 day)

1. Add event model + event hub.
2. Add meter and conversion modules with unit tests.
3. Add stream counters struct.

Exit:

1. Unit tests pass.
2. No callback blocking under synthetic load test.

## Phase 2: Mistral Provider (1.5-2 days)

1. Implement provider connection/send/receive/finish semantics.
2. Add protocol parser tests from captured fixtures.
3. Add reconnect and failure path tests.

Exit:

1. Deterministic mock tests for partial/final/error ordering.

## Phase 3: Session Controller Integration (1-1.5 days)

1. Add streaming branch in recording machine.
2. Implement lifecycle start/run/stop/fail transitions.
3. Implement final commit + dedupe + session aggregation.
4. Persist one workflow row per session (`stream://job_id`).

Exit:

1. End-to-end streaming works without overlay.
2. Batch mode unchanged.

## Phase 4: API Stream + Overlay (1.5-2 days)

1. Add `/stream/events` and `/stream/status` routes.
2. Build `audetic-overlay` client with reconnect.
3. Render transcript lane + meter + status line.

Exit:

1. Overlay receives live updates reliably.
2. Reconnect recovery verified.

## Phase 5: Hardening + Docs (1 day)

1. Improve error messages and troubleshooting docs.
2. Update config examples and API route docs.
3. Validate metrics and manual QA checklist.

Exit:

1. Test suite green.
2. Manual QA checklist complete.

## 16. Test Matrix

## 16.1 Unit

1. meter smoothing and clipping thresholds.
2. float->i16 conversion bounds.
3. resampler edge cases.
4. endpointing timing rules.
5. final dedupe logic.

## 16.2 Integration

1. batch mode regression.
2. streaming controller + mock provider.
3. SSE endpoint payload validity.
4. commit policy behaviour for each target.
5. DB persistence with `stream://` path.

## 16.3 Manual (Wayland)

1. overlay always-on-top.
2. meter responsiveness with loud/quiet input.
3. paste behaviour in `text_io` mode.
4. fast start/stop abuse test (10 quick toggles).

## 17. Observability

Counters to expose in `/stream/status`:

1. `stream_session_started_total`
2. `stream_session_stopped_total`
3. `stream_chunks_sent_total`
4. `stream_chunks_dropped_total`
5. `stream_partials_total`
6. `stream_finals_total`
7. `stream_provider_errors_total`
8. `stream_connected_clients_current`

## 18. Risk Register

1. Provider protocol changes.
   - Mitigation: isolate wire adapter + fixture tests.
2. Audio callback overload.
   - Mitigation: bounded queues + non-blocking callback.
3. Duplicate final events.
   - Mitigation: segment-aware dedupe window.
4. Overlay disconnect churn.
   - Mitigation: lightweight reconnect with backoff and keepalive.
5. History semantics confusion.
   - Mitigation: document `stream://<job_id>` path clearly in history docs.

## 19. Delivery and Rollback

Delivery:

1. Ship behind `streaming.enabled=false` default.
2. Dogfood locally for 2-3 days.
3. Enable by default only after stability targets are met.

Rollback:

1. Disable `streaming.enabled` to revert instantly to batch behaviour.
2. Keep streaming code path isolated; no schema change required in v1.

## 20. Estimate

1. MVP: 5-7 focused dev days.
2. Hardened v1: 8-10 focused dev days.

## 21. PR Sequence (Atomic)

1. PR1: stream events, counters, meter utilities, and tests only.
2. PR2: Mistral provider adapter and mock protocol tests only.
3. PR3: live session controller and recording machine integration.
4. PR4: SSE routes and status payload extensions.
5. PR5: overlay binary and reconnect logic.
6. PR6: docs, config examples, and QA checklist.

Rules:

1. Keep each PR focused on one subsystem.
2. Stage only touched files for each PR.
3. Keep batch-mode tests green in every PR.

## 22. Immediate Next Step

Implement Phase 0 first:

1. Add batch-mode regression tests around `/toggle` and `/status`.
2. Record baseline timings for current batch flow.
3. Freeze those as acceptance baselines before touching streaming code.
