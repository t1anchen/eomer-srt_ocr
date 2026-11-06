# Changelog

All notable changes to this project are documented here.

## [0.2.1] - 2026-11-07

Sprint 2 finishes the local HTTP service: the crash-prone legacy upload path is
gone, inference is serialised behind one gate, and progress reaches the browser
as a server-sent event stream.

### Added

- `GET /api/v1/streams/{id}/events` streams progress over Server-Sent Events.
  A subscriber receives the current snapshot immediately, then every state
  change; idle streams resend their state on an interval so intermediaries do
  not drop the connection. Unknown ids wait quietly instead of returning an
  error, because the browser subscribes before its first frame exists.
- The browser listens with `EventSource` and shows received and text-bearing
  frame counts as frames arrive.
- `serve` reads `EOMER_SRT_OCR_BIND`, `EOMER_SRT_OCR_SERVER` and
  `EOMER_SRT_OCR_WIDTH`; explicit flags still win. Blank values are ignored.
- HTTP tests drive the real router on an ephemeral loopback port.

### Changed

- `--server` is now optional, since the environment can supply it.
- `server::serve` is split into `bind` and `serve_on` so tests can bind an
  ephemeral port.
- A single `InferenceGate` now serialises every inference call. The job worker
  holds it for a whole run and the frame API holds it per frame; previously the
  frame API bypassed the queue and could compete with the worker for a backend
  that only keeps one model resident.
- The browser no longer polls for progress.

### Removed

- The legacy `/api/v0` upload, job, list and SRT routes, along with their
  handlers. A blocking client built inside an async handler tore down a runtime
  in an async context and killed the worker thread; the whole path is gone and
  the browser now reports unsupported containers instead of uploading them.

## [0.2.0] - 2026-11-06

Sprint 1 hardens the CLI and adds the first local HTTP and browser workflow.

### Added

- Optional `http` build feature and `serve` subcommand, with the browser assets
  embedded in the executable and the listener bound to loopback by default.
- Browser drag-and-drop for images and videos, local frame decoding, subtitle
  band cropping, progress polling, and SRT download.
- Versioned `/api/v1` frame-streaming endpoints. Frames can be retried
  individually, duplicate indices replace earlier observations, and finishing a
  stream is idempotent.
- Resumable video processing with input/parameter identity checks, persisted
  observations, and an explicit fresh-start option.
- Server model verification through llama.cpp `/props`, including manifest and
  filename mismatch reporting.
- Separate minimal CLI and HTTP-enabled Windows release binaries.

### Changed

- OCR requests fail quickly in debug builds while release builds retain a
  larger budget for cold starts and slower machines.
- The browser sends one cropped frame at a time instead of uploading the whole
  video. The legacy whole-file API remains available only as a compatibility
  fallback under `/api/v0`.
- CLI validation and media/server error messages now identify the failing
  parameter or stage more precisely.

### Fixed

- Overlay removal anchors on the most frequent observed line and merges only
  genuine OCR variants, preventing dialogue from being removed as a watermark.
- Streamed crops use the selected OCR prompt and are not cropped a second time.
- Explicit ignore lines are applied before streamed timelines are built.
- Rectangle pixel bounds normalise each axis independently.

### Known limitations

- The legacy `/api/v0` upload path still has its documented blocking-client and
  whole-file buffering defects; new clients should use `/api/v1`.
- Streamed timelines do not yet perform automatic overlay detection. Their
  frequency distribution must be measured before reusing the CLI thresholds.
- Browser-side decoding depends on the codecs supported by the browser.
- The browser does not yet provide timeline editing or plain-text export.

### Tests

- Pure-logic, CLI-contract, model-check, resume, and frame-stream unit tests.
- Manual Windows smoke coverage for single-image OCR, a complete short-video
  run, and the v1 frame-streaming API.

## [0.1.0] - 2026-11-05

First usable milestone: a command line tool that turns a video (or a still
image) into subtracted subtitle text using a local llama.cpp vision model.

### Added

- `video` command: ffmpeg frame sampling, subtitle-band discovery, VLM OCR,
  overlay removal, timeline assembly, SRT + per-frame JSONL output.
- `image` command: OCR a single image to a text file.
- `health` command: verify a llama-server endpoint.
- `models` command: validate and list JSON model manifests.
- Subtitle-band discovery by scanning horizontal strips with the model itself
  (pixel heuristics proved unreliable on real footage).
- Overlay removal in post-processing: lines that recur across a large share of
  frames (`IP属地`, `江西`, ...) are dropped, including near-identical OCR
  misreadings.
- Adaptive sampling rate (2 fps, 1 fps for media over an hour).
- Windows release binary that runs without a runtime install beyond the VC++
  redistributable.

### Tests

- 39 unit tests for pure logic (text cleaning, edit distance, timeline merge /
  split / flicker handling, SRT formatting, overlay detection, fps choice,
  mask geometry).
- 14 CLI contract tests (help output, invalid arguments, missing input,
  unreachable server, manifest loading).

### Known limitations

- Pixel-level overlay masking (`--mask`) is experimental and off by default.
- Subtitles that straddle two strips may be trimmed.
- Each frame spawns an ffmpeg process; long videos are dominated by I/O.
- Cues supported by a single sample can be as short as half a second.
