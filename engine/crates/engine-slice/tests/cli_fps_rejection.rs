//! F-09 CLI regressions: `--fps 0` is refused at the top-level mode
//! dispatch with a visible nonzero error directing to
//! `--fps uncapped` — the numeric 0 previously clamped to a silent
//! 1 FPS, so a documented "uncapped" measurement run was actually
//! paced at 1 FPS (the GPU kit and the CI render smokes both carried
//! the defect). These spawn the real binary; the refusal happens
//! during argument parsing, before any SDL/Vulkan or campaign
//! initialization, so the checks are GPU-free and deterministic on
//! every platform.

use std::process::Command;

fn run_binary(args: &[&str]) -> std::process::Output {
    Command::new(env!("CARGO_BIN_EXE_engine-slice"))
        .args(args)
        .output()
        .expect("spawn the engine-slice binary")
}

#[test]
fn render_rejects_fps_zero_directing_to_uncapped() {
    let output = run_binary(&["--render", "--fps", "0"]);
    assert!(
        !output.status.success(),
        "the binary must exit nonzero on --fps 0"
    );
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains("--fps uncapped"),
        "stderr must direct to uncapped: {stderr}"
    );
    assert!(
        stderr.contains("1 FPS"),
        "stderr must state the silent 1 FPS clamp it refuses: {stderr}"
    );
}

#[test]
fn demo_persist_rejects_fps_zero_directing_to_uncapped() {
    // The campaign directory is never touched — the refusal exits
    // during argument parsing, before any campaign or GPU work.
    let output = run_binary(&["--demo-persist", "unused-campaign-dir", "--fps", "0"]);
    assert!(
        !output.status.success(),
        "the binary must exit nonzero on --fps 0"
    );
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains("--fps uncapped"),
        "stderr must direct to uncapped: {stderr}"
    );
    assert!(
        stderr.contains("1 FPS"),
        "stderr must state the silent 1 FPS clamp it refuses: {stderr}"
    );
}
