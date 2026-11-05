//! Unit tests for pure helpers: text cleaning, similarity, timestamps, SRT.

use eomer_srt_ocr::timeline::{
    Cue, FrameObservation, TimelineConfig, build_timeline, clean_lines, distance, fmt_ts, to_srt,
};

// ---------------------------------------------------------------- clean_lines

#[test]
fn clean_lines_drops_blank_lines_and_trims() {
    let lines = clean_lines("  first  \n\n\nsecond\n\t\n  ");
    assert_eq!(lines, vec!["first".to_string(), "second".to_string()]);
}

#[test]
fn clean_lines_strips_quotes_and_fences() {
    // Opening fence with a language tag, standalone fences, and rule lines are
    // all model artefacts and must disappear.
    let lines = clean_lines("```text\n\"quoted line\"\n---\n`tick`\n```");
    assert_eq!(lines, vec!["quoted line".to_string(), "tick".to_string()]);
}

#[test]
fn clean_lines_keeps_subtitle_punctuation() {
    let lines = clean_lines("我从来不想做英雄。\n只是命运把我推到了这里。");
    assert_eq!(
        lines,
        vec![
            "我从来不想做英雄。".to_string(),
            "只是命运把我推到了这里。".to_string()
        ]
    );
}

#[test]
fn clean_lines_of_empty_input_is_empty() {
    assert!(clean_lines("").is_empty());
    assert!(clean_lines("   \n\t\n").is_empty());
}

// ------------------------------------------------------------------ distance

#[test]
fn distance_identical_text_is_zero() {
    assert_eq!(distance("我問你", "我問你"), 0.0);
    assert_eq!(distance("", ""), 0.0);
}

#[test]
fn distance_ignores_whitespace() {
    assert_eq!(distance("我 問 你", "我問你"), 0.0);
    assert_eq!(distance("a b c", "abc"), 0.0);
}

#[test]
fn distance_one_char_edit() {
    // 3 chars, one substitution -> 1/3
    let d = distance("我問他", "我問你");
    assert!((d - 1.0 / 3.0).abs() < 1e-9, "got {d}");
}

#[test]
fn distance_one_extra_char() {
    let d = distance("我問你", "我問你了");
    assert!((d - 0.25).abs() < 1e-9, "got {d}");
}

#[test]
fn distance_completely_different_is_one() {
    assert_eq!(distance("abc", "xyz"), 1.0);
}

#[test]
fn distance_symmetric() {
    assert!(
        (distance("照會各國", "照會各國駐") - distance("照會各國駐", "照會各國")).abs() < 1e-12
    );
}

// --------------------------------------------------------------------- fmt_ts

#[test]
fn fmt_ts_formats_standard_cases() {
    assert_eq!(fmt_ts(0.0), "00:00:00,000");
    assert_eq!(fmt_ts(1.5), "00:00:01,500");
    assert_eq!(fmt_ts(61.25), "00:01:01,250");
    assert_eq!(fmt_ts(3661.0), "01:01:01,000");
}

#[test]
fn fmt_ts_clamps_negative() {
    assert_eq!(fmt_ts(-3.0), "00:00:00,000");
}

#[test]
fn fmt_ts_rounds_to_millis() {
    assert_eq!(fmt_ts(0.0006), "00:00:00,001");
    assert_eq!(fmt_ts(59.9996), "00:01:00,000");
}

// --------------------------------------------------------------------- to_srt

fn cue(text: &str, start: f64, end: f64) -> Cue {
    Cue {
        text: text.to_string(),
        start,
        end,
        support: 1,
    }
}

#[test]
fn to_srt_numbers_sequentially_and_ends_with_blank_line() {
    let cues = vec![cue("第一句", 0.0, 1.0), cue("第二句", 1.5, 3.0)];
    let srt = to_srt(&cues);
    assert_eq!(
        srt,
        "1\n00:00:00,000 --> 00:00:01,000\n第一句\n\n2\n00:00:01,500 --> 00:00:03,000\n第二句\n\n"
    );
}

#[test]
fn to_srt_empty_is_empty() {
    assert_eq!(to_srt(&[]), "");
}

#[test]
fn to_srt_preserves_multiline_cue_body() {
    let srt = to_srt(&[cue("第一行\n第二行", 0.0, 2.0)]);
    assert!(srt.contains("第一行\n第二行\n\n"));
}

// --------------------------------------------------------------- build_timeline

fn obs(index: usize, t: f64, text: &str) -> FrameObservation {
    let lines = clean_lines(text);
    FrameObservation {
        index,
        t,
        raw: text.to_string(),
        lines,
        latency_ms: 0,
    }
}

fn cfg() -> TimelineConfig {
    TimelineConfig::default()
}

#[test]
fn timeline_empty_input() {
    assert!(build_timeline(&[], 0.5, &cfg()).is_empty());
}

#[test]
fn timeline_single_observation_spanning_one_frame() {
    // One sample lasts frame_dt, then the cue is stretched to the minimum
    // display duration; the start stays at the sampled moment.
    let cues = build_timeline(&[obs(0, 0.0, "唯一一句")], 0.5, &cfg());
    assert_eq!(cues.len(), 1);
    assert_eq!(cues[0].text, "唯一一句");
    assert_eq!(cues[0].start, 0.0);
    assert!((cues[0].end - 1.0).abs() < 1e-9, "got {:?}", cues[0].end);
}

#[test]
fn timeline_merges_identical_consecutive_frames() {
    let o = vec![
        obs(0, 0.0, "我問你"),
        obs(1, 0.5, "我問你"),
        obs(2, 1.0, "我問你"),
        obs(3, 1.5, "我問你"),
    ];
    let cues = build_timeline(&o, 0.5, &cfg());
    assert_eq!(cues.len(), 1, "identical frames must collapse into one cue");
    assert_eq!(cues[0].start, 0.0);
    assert!((cues[0].end - 2.0).abs() < 1e-9);
    assert_eq!(cues[0].support, 4);
}

#[test]
fn timeline_splits_on_different_text() {
    let o = vec![
        obs(0, 0.0, "第一句"),
        obs(1, 0.5, "第一句"),
        obs(2, 1.0, "完全不同的另外一句话"),
        obs(3, 1.5, "完全不同的另外一句话"),
    ];
    let cues = build_timeline(&o, 0.5, &cfg());
    assert_eq!(cues.len(), 2);
    assert_eq!(cues[0].text, "第一句");
    assert_eq!(cues[1].text, "完全不同的另外一句话");
}

#[test]
fn timeline_bridges_a_single_empty_frame() {
    // A one-frame OCR drop-out between two identical lines must not split the cue.
    let o = vec![
        obs(0, 0.0, "取得聯繫了嗎"),
        obs(1, 0.5, "取得聯繫了嗎"),
        obs(2, 1.0, ""),
        obs(3, 1.5, "取得聯繫了嗎"),
        obs(4, 2.0, "取得聯繫了嗎"),
    ];
    let cues = build_timeline(&o, 0.5, &cfg());
    assert_eq!(
        cues.len(),
        1,
        "single empty frame should be bridged: {cues:?}"
    );
    assert!((cues[0].start - 0.0).abs() < 1e-9);
    assert!((cues[0].end - 2.5).abs() < 1e-9);
}

#[test]
fn timeline_keeps_real_gaps_between_sentences() {
    let o = vec![
        obs(0, 0.0, "第一句"),
        obs(1, 0.5, "第一句"),
        obs(2, 1.0, ""),
        obs(3, 1.5, ""),
        obs(4, 2.0, ""),
        obs(5, 2.5, "第二句"),
        obs(6, 3.0, "第二句"),
    ];
    let cues = build_timeline(&o, 0.5, &cfg());
    assert_eq!(cues.len(), 2);
    assert!(
        (cues[0].end - 1.0).abs() < 1e-9,
        "first cue ends before the gap: {cues:?}"
    );
    assert!((cues[1].start - 2.5).abs() < 1e-9);
}

#[test]
fn timeline_ignores_flicker_blip_between_identical_lines() {
    let o = vec![
        obs(0, 0.0, "建議大總統退位"),
        obs(1, 0.5, "建議大總統退位"),
        obs(2, 1.0, "建議大總統退位"),
        obs(3, 1.5, "建議大總統退仿"), // single noisy sample
        obs(4, 2.0, "建議大總統退位"),
    ];
    let cues = build_timeline(&o, 0.5, &cfg());
    assert_eq!(
        cues.len(),
        1,
        "one noisy sample between identical lines is flicker: {cues:?}"
    );
}

#[test]
fn timeline_keeps_back_to_back_cues_without_overlap() {
    // Two different lines sampled on consecutive frames. The first cue cannot
    // reach the minimum without overlapping, so it stops at the next cue
    // instead of being dropped or swallowing its neighbour.
    let mut c = cfg();
    c.min_cue_secs = 1.0;
    let o = vec![obs(0, 0.0, "短"), obs(1, 0.5, "另一句")];
    let cues = build_timeline(&o, 0.5, &c);
    assert_eq!(cues.len(), 2, "{cues:?}");
    assert!((cues[0].end - 0.5).abs() < 1e-9, "{cues:?}");
    assert!((cues[1].start - 0.5).abs() < 1e-9, "{cues:?}");
}

#[test]
fn timeline_chooses_variant_with_most_lines() {
    // A multi-line variant is made by *prepending* a line to the single-line
    // one (same final line). The cue must keep the fuller variant, and the
    // preceding single-line samples belong to the same cue.
    let o = vec![
        obs(0, 0.0, "取得聯繫了嗎"),
        obs(1, 0.5, "取得聯繫了嗎"),
        obs(2, 1.0, "跟各國駐北京的公使館\n取得聯繫了嗎"),
        obs(3, 1.5, "跟各國駐北京的公使館\n取得聯繫了嗎"),
    ];
    let cues = build_timeline(&o, 0.5, &cfg());
    assert_eq!(cues.len(), 1, "variants must not split: {cues:?}");
    assert_eq!(cues[0].text, "跟各國駐北京的公使館\n取得聯繫了嗎");
}

#[test]
fn timeline_prefers_fuller_variant_when_line_is_missing() {
    // The middle sample drops the first line entirely: that is the "most lines"
    // case in reverse, and the fuller text must win.
    let o = vec![
        obs(0, 0.0, "取得聯繫了嗎"),
        obs(1, 0.5, "跟各國駐北京的公使館\n取得聯繫了嗎"),
        obs(2, 1.0, "取得聯繫了嗎"),
        obs(3, 1.5, "取得聯繫了嗎"),
    ];
    let cues = build_timeline(&o, 0.5, &cfg());
    assert_eq!(
        cues.len(),
        1,
        "three one-line samples plus one two-line: {cues:?}"
    );
    assert_eq!(
        cues[0].text, "取得聯繫了嗎",
        "one two-line sample is a minority variant"
    );
}

#[test]
fn timeline_output_is_chronological_and_non_overlapping() {
    let o = vec![
        obs(0, 0.0, "A"),
        obs(1, 0.5, "A"),
        obs(2, 1.0, ""),
        obs(3, 1.5, "B"),
        obs(4, 2.0, "B"),
        obs(5, 2.5, "C"),
        obs(6, 3.0, "C"),
    ];
    let cues = build_timeline(&o, 0.5, &cfg());
    for w in cues.windows(2) {
        assert!(w[0].end <= w[1].start, "cues overlap: {w:?}");
    }
}

// --------------------------------------------------------------- looks_like_subtitle

use eomer_srt_ocr::pipeline::looks_like_subtitle;

#[test]
fn subtitle_like_short_lines_are_accepted() {
    assert!(looks_like_subtitle(&["我問你".to_string()]));
    assert!(looks_like_subtitle(&[
        "各國承認我中華民國的重要性".to_string()
    ]));
    assert!(looks_like_subtitle(&[
        "跟各國駐北京的公使館".to_string(),
        "取得聯繫了嗎".to_string()
    ]));
}

#[test]
fn subtitle_like_rejects_empty_and_long_blocks() {
    assert!(!looks_like_subtitle(&[]));
    assert!(!looks_like_subtitle(&["".to_string()]));
    let long: Vec<String> = (0..5).map(|i| format!("line {i}")).collect();
    assert!(!looks_like_subtitle(&long), "5 lines is not a subtitle");
    assert!(
        !looks_like_subtitle(&["x".repeat(200)]),
        "very long single line"
    );
}

// --------------------------------------------------------------- choose_fps

use eomer_srt_ocr::ffmpeg::MediaInfo;
use eomer_srt_ocr::pipeline::choose_fps;

fn media(duration: f64) -> MediaInfo {
    MediaInfo {
        width: 1920,
        height: 1080,
        duration_secs: duration,
        has_audio: true,
        fps: None,
    }
}

#[test]
fn choose_fps_short_media_is_2fps() {
    assert_eq!(choose_fps(&media(60.0), Some(24.0)), 2.0);
    assert_eq!(choose_fps(&media(600.0), Some(24.0)), 2.0);
}

#[test]
fn choose_fps_long_media_is_1fps() {
    assert_eq!(choose_fps(&media(7200.0), Some(24.0)), 1.0);
}

#[test]
fn choose_fps_never_exceeds_native_rate() {
    assert_eq!(choose_fps(&media(60.0), Some(1.5)), 1.5);
    assert_eq!(
        choose_fps(&media(60.0), Some(0.25)),
        0.5,
        "clamped to a floor of 0.5"
    );
    assert_eq!(
        choose_fps(&media(60.0), None),
        2.0,
        "unknown native rate keeps default"
    );
    assert_eq!(
        choose_fps(&media(60.0), Some(0.0)),
        2.0,
        "zero native rate is ignored"
    );
}

// --------------------------------------------------------------- MaskRect

use eomer_srt_ocr::pipeline::MaskRect;

#[test]
fn mask_rect_pixel_bounds_are_relative_to_the_frame() {
    let m = MaskRect {
        x0: 0.11,
        y0: 0.90,
        x1: 0.17,
        y1: 0.96,
    };
    let (x0, y0, x1, y1) = m.pixel_bounds(1920, 1080);
    // 1920 * 0.11 = 211.2 -> 211, and so on
    assert_eq!((x0, x1), (211, 326));
    assert_eq!((y0, y1), (972, 1036));
}

#[test]
fn mask_rect_clamps_out_of_range_values() {
    let m = MaskRect {
        x0: -0.5,
        y0: 1.5,
        x1: 2.0,
        y1: -1.0,
    };
    let (x0, y0, x1, y1) = m.pixel_bounds(100, 50);
    assert!(
        x0 <= x1 && y0 <= y1,
        "bounds must be ordered: {x0},{y0},{x1},{y1}"
    );
    assert!(x1 <= 100 && y1 <= 50, "bounds must be inside the image");
}

#[test]
fn mask_rect_order_is_normalised_when_inverted() {
    let a = MaskRect {
        x0: 0.8,
        y0: 0.8,
        x1: 0.2,
        y1: 0.2,
    };
    let (_ax0, _ay0, ax1, ay1) = a.pixel_bounds(1000, 1000);
    let b = MaskRect {
        x0: 0.2,
        y0: 0.2,
        x1: 0.8,
        y1: 0.8,
    };
    let (_bx0, _by0, bx1, by1) = b.pixel_bounds(1000, 1000);
    assert_eq!((ax1, ay1), (bx1, by1));
}

// --------------------------------------------------------------- overlays

use eomer_srt_ocr::pipeline::overlay_lines_from;

fn obs_with(index: usize, text: &str) -> FrameObservation {
    let lines = clean_lines(text);
    FrameObservation {
        index,
        t: index as f64 * 0.5,
        raw: text.to_string(),
        lines,
        latency_ms: 0,
    }
}

#[test]
fn repeated_watermark_is_detected_as_overlay() {
    // 40 frames: the watermark appears in 18 of them, dialogue never more than
    // twice. Only the watermark may be classified as an overlay.
    let mut frames = Vec::new();
    for i in 0..40 {
        let text = if i % 2 == 0 { "IP属地" } else { "我問你" };
        frames.push(obs_with(i, text));
    }
    // make dialogue diverse so its lines are rare
    for (i, f) in frames.iter_mut().enumerate() {
        if i % 2 == 1 {
            f.lines = vec![format!("台词{i}")];
        }
    }
    let overlays = overlay_lines_from(&frames, 0.10, 5);
    assert!(overlays.contains("IP属地"), "got {overlays:?}");
    assert!(
        !overlays.iter().any(|l| l.starts_with("台词")),
        "dialogue must survive: {overlays:?}"
    );
}

#[test]
fn overlay_variants_are_merged() {
    // The same tag misread in different ways must all be removed.
    let variants = ["IP属地", "IP屬地", "IP屋地", "IP 属地"];
    let mut frames = Vec::new();
    for i in 0..40 {
        frames.push(obs_with(i, variants[i % variants.len()]));
    }
    let overlays = overlay_lines_from(&frames, 0.10, 5);
    for v in variants {
        assert!(overlays.contains(v), "{v} not removed; got {overlays:?}");
    }
}

#[test]
fn long_repeated_line_is_not_treated_as_overlay() {
    // A long sentence that repeats is suspicious but not a short tag.
    let long = "如果有一天你忘了这一切记得曾经有个夏天我们并肩作战过";
    let mut frames = Vec::new();
    for i in 0..20 {
        frames.push(obs_with(i, long));
    }
    assert!(
        overlay_lines_from(&frames, 0.10, 5).is_empty(),
        "long lines are not overlay tags"
    );
}

#[test]
fn no_overlay_detected_when_everything_is_unique() {
    let frames: Vec<FrameObservation> = (0..20)
        .map(|i| obs_with(i, &format!("独特台词{i}")))
        .collect();
    assert!(overlay_lines_from(&frames, 0.10, 5).is_empty());
}

// ------------------------------------------------------- minimum cue duration

#[test]
fn short_cue_is_extended_to_the_minimum() {
    // A single supported sample yields one frame of duration; with the default
    // minimum it must be stretched, and the opening time kept.
    let mut c = cfg();
    c.min_cue_secs = 1.0;
    let o = vec![
        obs(0, 0.0, ""),
        obs(1, 0.5, ""),
        obs(2, 1.0, "我問你"),
        obs(3, 1.5, ""),
        obs(4, 2.0, ""),
        obs(5, 2.5, ""),
    ];
    let cues = build_timeline(&o, 0.5, &c);
    assert_eq!(cues.len(), 1, "{cues:?}");
    let d = cues[0].end - cues[0].start;
    assert!((d - 1.0).abs() < 1e-9, "expected 1.0s, got {d}");
    assert!((cues[0].start - 1.0).abs() < 1e-9, "start kept at the sampled time");
}

#[test]
fn extension_does_not_overlap_the_next_cue() {
    // Back-to-back cues: the first one may only grow into the free gap, never
    // over the following cue.
    let mut c = cfg();
    c.min_cue_secs = 1.0;
    let o = vec![
        obs(0, 0.0, "第一句"),
        obs(1, 0.25, "完全不同的第二句"),
        obs(2, 0.5, "完全不同的第二句"),
    ];
    let cues = build_timeline(&o, 0.25, &c);
    for w in cues.windows(2) {
        assert!(
            w[0].end <= w[1].start + 1e-9,
            "cue overran its neighbour: {w:?}"
        );
    }
}

#[test]
fn extension_is_capped_by_available_gap() {
    // Only a small gap before the next cue: stretch as far as possible without
    // touching it rather than producing an overlap.
    let mut c = cfg();
    c.min_cue_secs = 1.0;
    let o = vec![
        obs(0, 0.0, "短"),
        obs(1, 0.5, ""),
        obs(2, 1.0, "完全不同的第二句"),
        obs(3, 1.5, "完全不同的第二句"),
    ];
    let cues = build_timeline(&o, 0.5, &c);
    assert_eq!(cues.len(), 2, "{cues:?}");
    assert!(cues[0].end <= cues[1].start + 1e-9, "{cues:?}");
    assert!(
        (cues[0].end - cues[0].start - 1.0).abs() < 1e-9,
        "there was room for one second before the next cue"
    );
}

#[test]
fn long_cue_is_left_untouched() {
    let mut c = cfg();
    c.min_cue_secs = 1.0;
    let o = vec![
        obs(0, 0.0, "一句比较长的话"),
        obs(1, 0.5, "一句比较长的话"),
        obs(2, 1.0, "一句比较长的话"),
        obs(3, 1.5, "一句比较长的话"),
    ];
    let cues = build_timeline(&o, 0.5, &c);
    assert!((cues[0].end - cues[0].start - 2.0).abs() < 1e-9, "{cues:?}");
}

#[test]
fn extension_can_run_past_end_of_media() {
    // Last cue of the media: nothing follows, so it may grow freely.
    let mut c = cfg();
    c.min_cue_secs = 1.0;
    let o = vec![obs(0, 0.0, "最后一句"), obs(1, 0.5, "")];
    let cues = build_timeline(&o, 0.5, &c);
    assert_eq!(cues.len(), 1);
    assert!((cues[0].end - cues[0].start - 1.0).abs() < 1e-9, "{cues:?}");
}

// ------------------------------------------------------------ output layout

use eomer_srt_ocr::paths::resolve_output_dir;

fn tmp(name: &str) -> std::path::PathBuf {
    let p = std::env::temp_dir().join(format!("eomer_out_{name}"));
    let _ = std::fs::remove_dir_all(&p);
    p
}

#[test]
fn default_output_goes_below_out_with_the_input_stem() {
    let cwd = tmp("cwd1");
    std::fs::create_dir_all(&cwd).unwrap();
    let got = resolve_output_dir(
        std::path::Path::new("/media/My Movie.mkv"),
        None,
        &cwd,
    );
    assert_eq!(got, cwd.join("out").join("My Movie"));
}

#[test]
fn default_output_never_equals_the_current_directory() {
    let cwd = tmp("cwd2");
    std::fs::create_dir_all(&cwd).unwrap();
    let got = resolve_output_dir(std::path::Path::new("/media/clip.mp4"), None, &cwd);
    assert_ne!(got, cwd, "must not write results into the working directory");
    assert!(got.starts_with(&cwd), "default output stays under cwd/out");
    assert_eq!(got.file_name().unwrap(), "clip");
}

#[test]
fn explicit_out_is_used_verbatim() {
    let cwd = tmp("cwd3");
    let target = cwd.join("somewhere").join("here");
    let got = resolve_output_dir(std::path::Path::new("/media/clip.mp4"), Some(&target), &cwd);
    assert_eq!(got, target);
}

#[test]
fn explicit_dot_out_is_not_silently_redirected() {
    // Passing "." means the caller really wants the current directory; the
    // tool must honour it rather than guessing a subdirectory.
    let cwd = tmp("cwd4");
    let got = resolve_output_dir(std::path::Path::new("/media/clip.mp4"), Some(std::path::Path::new(".")), &cwd);
    assert_eq!(got, std::path::PathBuf::from("."));
}

#[test]
fn input_without_extension_gets_a_sane_stem() {
    let cwd = tmp("cwd5");
    let got = resolve_output_dir(std::path::Path::new("/media/README"), None, &cwd);
    assert_eq!(got.file_name().unwrap(), "README");
}

#[test]
fn non_ascii_and_spaced_stems_are_kept() {
    let cwd = tmp("cwd6");
    let got = resolve_output_dir(
        std::path::Path::new("/media/走向共和 第51集.mkv"),
        None,
        &cwd,
    );
    assert_eq!(got.file_name().unwrap(), "走向共和 第51集");
}

#[test]
fn resolve_output_dir_is_pure_and_repeatable() {
    let cwd = tmp("cwd7");
    let input = std::path::Path::new("/media/a.mp4");
    let a = resolve_output_dir(input, None, &cwd);
    let b = resolve_output_dir(input, None, &cwd);
    assert_eq!(a, b);
}

// ---------------------------------------------------------------- validation

use eomer_srt_ocr::validate::{
    check_duration, check_fps, check_image_size, check_min_cue_duration, check_similarity,
    check_width,
};

#[test]
fn fps_within_range_is_accepted() {
    assert!(check_fps(0.1).is_ok());
    assert!(check_fps(2.0).is_ok());
    assert!(check_fps(10.0).is_ok());
}

#[test]
fn fps_out_of_range_is_rejected_with_guidance() {
    for bad in [0.0, -1.0, 60.0, f64::NAN, f64::INFINITY] {
        let err = check_fps(bad).expect_err(&format!("{bad} should be rejected"));
        assert!(err.to_string().contains("--fps"), "message should name the flag: {err}");
    }
}

#[test]
fn width_out_of_range_is_rejected() {
    assert!(check_width(320).is_ok());
    assert!(check_width(1920).is_ok());
    assert!(check_width(3840).is_ok());
    assert!(check_width(1).is_err());
    assert!(check_width(0).is_err());
    assert!(check_width(8000).is_err());
}

#[test]
fn similarity_outside_zero_one_is_rejected() {
    assert!(check_similarity(0.0).is_ok());
    assert!(check_similarity(1.0).is_ok());
    assert!(check_similarity(-0.1).is_err());
    assert!(check_similarity(1.5).is_err());
    assert!(check_similarity(f64::NAN).is_err());
}

#[test]
fn min_cue_duration_is_bounded() {
    assert!(check_min_cue_duration(0.0).is_ok());
    assert!(check_min_cue_duration(1.0).is_ok());
    assert!(check_min_cue_duration(-1.0).is_err());
    assert!(check_min_cue_duration(120.0).is_err());
}

#[test]
fn oversized_image_is_rejected() {
    assert!(check_image_size(1024).is_ok());
    assert!(check_image_size(64 * 1024 * 1024).is_ok());
    assert!(check_image_size(64 * 1024 * 1024 + 1).is_err());
}

#[test]
fn too_short_media_is_rejected() {
    assert!(check_duration(0.5).is_ok());
    assert!(check_duration(60.0).is_ok());
    assert!(check_duration(0.0).is_err());
    assert!(check_duration(-1.0).is_err());
    assert!(check_duration(f64::NAN).is_err());
}
