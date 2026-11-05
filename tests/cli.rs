//! CLI-level tests: argument handling, error paths and exit codes.
//!
//! These never touch the network or a model. They assert the process contract:
//! what happens with missing files, bad flags, an unreachable server, and the
//! shape of `--help` output.

use std::process::Command;

fn bin() -> Command {
    Command::new(env!("CARGO_BIN_EXE_eomer-srt-ocr"))
}

// ------------------------------------------------------------------ --help

#[test]
fn help_exits_successfully_and_lists_subcommands() {
    let out = bin().arg("--help").output().expect("run --help");
    assert!(out.status.success(), "exit={:?}", out.status);
    let stdout = String::from_utf8_lossy(&out.stdout);
    for cmd in ["video", "health", "models"] {
        assert!(stdout.contains(cmd), "help output must mention `{cmd}`");
    }
}

#[test]
fn version_flag_works() {
    let out = bin().arg("--version").output().expect("run --version");
    assert!(out.status.success());
    assert!(String::from_utf8_lossy(&out.stdout).contains("eomer-srt-ocr"));
}

// ----------------------------------------------------------- argument errors

#[test]
fn unknown_subcommand_fails() {
    let out = bin().arg("nonsense").output().expect("run nonsense");
    assert!(!out.status.success(), "unknown subcommand must be an error");
}

#[test]
fn unknown_flag_fails() {
    let out = bin()
        .args(["health", "--bogus-flag"])
        .output()
        .expect("run bad flag");
    assert!(!out.status.success());
}

#[test]
fn invalid_fps_value_fails() {
    let out = bin()
        .args(["video", "whatever.mp4", "--fps", "not-a-number"])
        .output()
        .expect("run bad fps");
    assert!(!out.status.success(), "non-numeric --fps must be rejected");
}

#[test]
fn invalid_width_value_fails() {
    let out = bin()
        .args(["video", "whatever.mp4", "--width", "-5"])
        .output()
        .expect("run bad width");
    assert!(!out.status.success(), "negative --width must be rejected");
}

// -------------------------------------------------------------- missing input

#[test]
fn missing_video_file_fails_with_clear_message() {
    let out = bin()
        .args(["video", "/definitely/not/here.mp4"])
        .output()
        .expect("run missing input");
    assert!(
        !out.status.success(),
        "missing input must be a non-zero exit"
    );
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(
        stderr.contains("not found") || stderr.contains("input"),
        "error message should explain the missing input, got: {stderr}"
    );
}

// ------------------------------------------------------ unreachable server

#[test]
fn health_on_dead_port_fails() {
    // Port 1 is reserved and never listening, so this is deterministic.
    let out = bin()
        .args(["health", "--server", "http://127.0.0.1:1"])
        .output()
        .expect("run health");
    assert!(
        !out.status.success(),
        "health against a dead port must fail"
    );
    let text = format!(
        "{}{}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    );
    assert!(text.contains("not reachable"), "got: {text}");
}

#[test]
fn health_on_malformed_url_fails_gracefully() {
    let out = bin()
        .args(["health", "--server", "not a url"])
        .output()
        .expect("run health");
    assert!(!out.status.success());
    // Must not panic: stderr should hold a message, not a backtrace.
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(!stderr.contains("panicked"), "must not panic: {stderr}");
}

#[test]
fn video_with_dead_server_fails_before_processing() {
    // A real, tiny file path is not needed: absence is checked first.
    let out = bin()
        .args(["video", "/tmp/x.mp4", "--server", "http://127.0.0.1:1"])
        .output()
        .expect("run video");
    assert!(!out.status.success());
}

// ------------------------------------------------------------------- models

#[test]
fn models_on_empty_dir_succeeds_with_no_output_rows() {
    let dir = std::env::temp_dir().join("eomer_srt_ocr_models_empty");
    std::fs::create_dir_all(&dir).unwrap();
    let out = bin()
        .args(["models", "--dir", dir.to_str().unwrap()])
        .output()
        .expect("run models");
    assert!(
        out.status.success(),
        "listing an empty model dir is not an error"
    );
    assert!(String::from_utf8_lossy(&out.stdout).contains("no manifests"));
}

#[test]
fn models_on_missing_dir_is_not_a_crash() {
    let out = bin()
        .args(["models", "--dir", "/definitely/not/a/dir"])
        .output()
        .expect("run models");
    assert!(
        out.status.success(),
        "a missing model dir simply has no models"
    );
}

#[test]
fn models_lists_valid_manifest() {
    let dir = std::env::temp_dir().join("eomer_srt_ocr_models_one");
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    // Create the GGUF + mmproj files the manifest points at.
    let model = dir.join("model.gguf");
    let mmproj = dir.join("mmproj.gguf");
    std::fs::write(&model, b"x").unwrap();
    std::fs::write(&mmproj, b"x").unwrap();
    let manifest = serde_json::json!({
        "id": "test-ocr",
        "display_name": "Test OCR",
        "backend": "llamacpp-vlm",
        "model_path": model,
        "mmproj_path": mmproj,
    });
    std::fs::write(dir.join("test-ocr.json"), manifest.to_string()).unwrap();

    let out = bin()
        .args(["models", "--dir", dir.to_str().unwrap()])
        .output()
        .expect("run models");
    assert!(
        out.status.success(),
        "stderr={}",
        String::from_utf8_lossy(&out.stderr)
    );
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(
        stdout.contains("test-ocr"),
        "manifest id should be listed: {stdout}"
    );
    assert!(stdout.contains("Test OCR"));
}

#[test]
fn models_skips_invalid_manifest_with_warning() {
    let dir = std::env::temp_dir().join("eomer_srt_ocr_models_bad");
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    // mmproj missing -> manifest must be rejected, not crash the listing.
    let manifest = serde_json::json!({
        "id": "broken",
        "display_name": "Broken",
        "model_path": dir.join("nope.gguf"),
        "mmproj_path": dir.join("nope-mmproj.gguf"),
    });
    std::fs::write(dir.join("broken.json"), manifest.to_string()).unwrap();
    let out = bin()
        .args(["models", "--dir", dir.to_str().unwrap()])
        .output()
        .expect("run models");
    assert!(
        out.status.success(),
        "a broken manifest must not abort the command"
    );
    assert!(
        String::from_utf8_lossy(&out.stderr).contains("warning"),
        "broken manifest should produce a warning"
    );
}

// --------------------------------------------------------- option validation

fn stderr_of(args: &[&str]) -> String {
    let out = bin().args(args).output().expect("run");
    format!(
        "{}{}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    )
}

fn exit_ok(args: &[&str]) -> bool {
    bin().args(args).output().expect("run").status.success()
}

#[test]
fn out_of_range_options_fail_before_touching_files() {
    // The input does not exist on purpose: the option error must win.
    let cases: &[(&[&str], &str)] = &[
        (&["video", "/no/such/file.mp4", "--fps", "60"], "--fps"),
        (&["video", "/no/such/file.mp4", "--width", "1"], "--width"),
        (
            &["video", "/no/such/file.mp4", "--similarity", "2"],
            "--similarity",
        ),
        (
            &["video", "/no/such/file.mp4", "--min-cue-duration", "-1"],
            "--min-cue-duration",
        ),
        (&["image", "/no/such/file.png", "--width", "5"], "--width"),
    ];
    for (args, flag) in cases {
        assert!(!exit_ok(args), "should fail: {args:?}");
        let msg = stderr_of(args);
        assert!(
            msg.contains(flag),
            "message for {args:?} should name {flag}: {msg}"
        );
        assert!(
            !msg.contains("input not found"),
            "option error first: {msg}"
        );
    }
}

#[test]
fn valid_options_still_reach_the_input_check() {
    let msg = stderr_of(&["video", "/no/such/file.mp4", "--fps", "2"]);
    assert!(msg.contains("input not found"), "{msg}");
}

#[test]
fn unreadable_video_reports_a_readable_error() {
    let dir = std::env::temp_dir().join("eomer_cli_badinput");
    std::fs::create_dir_all(&dir).unwrap();
    let txt = dir.join("notes.txt");
    std::fs::write(&txt, b"not a video").unwrap();
    let msg = stderr_of(&[
        "video",
        txt.to_str().unwrap(),
        "--server",
        "http://127.0.0.1:1",
    ]);
    // Either ffprobe rejection or the unreachable server may fire first; what
    // matters is that the process does not panic and says something useful.
    assert!(!msg.contains("panicked"), "{msg}");
}

#[test]
fn non_ascii_path_with_spaces_is_parsed() {
    // Parsing only: the file need not exist, but the name must survive.
    let msg = stderr_of(&["image", "/tmp/中文 图片 测试.png"]);
    assert!(msg.contains("中文 图片 测试.png"), "path mangled: {msg}");
}
