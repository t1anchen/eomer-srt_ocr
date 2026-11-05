# Changelog

All notable changes to this project are documented here.

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
