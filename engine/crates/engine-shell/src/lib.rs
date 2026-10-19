//! Stage 1 SDL3 application shell — windowing, input intents, application
//! lifecycle (lead-authorized 2026-10-19; TECH-ARCHITECTURE §6 Stage 1).
//!
//! Scope: a real SDL3 window with the OS event pump, a CPU-driven paced
//! application loop, and an input-intent state machine (WASD movement
//! axes, mouse-look deltas, key/button edges, focus loss/regain with
//! stuck-key and stale-delta prevention, relative mouse capture, resize,
//! quit). OS monotonic time is used for frame pacing only and never
//! enters simulation logic; the authoritative campaign clock belongs to
//! engine-core, is not present in Stage 1, and attaches in Stage 3 behind
//! its own gate.
//!
//! Strictly excluded (Stage 1 authorization): Vulkan, ash, swapchains, GPU
//! rendering/presentation, vsync, physics, ECS, world simulation, gameplay
//! logic, multiplayer, persistence, audio, editor systems.
//!
//! Binding and native-library strategy: the `sdl3` crate 0.20
//! (vhspace/sdl3-rs, MIT) over `sdl3-sys` 0.7.2 (maia-s/sdl3-sys-rs,
//! Zlib), building vendored SDL 3.4.18 (`sdl3-src`) from source with CMake
//! and linking it statically — identical native version on the dev pod,
//! CI runners, and the lead's Windows rig, with no system SDL packages or
//! DLL redistribution. SDL3 is zlib-licensed, so static linking is
//! unproblematic. On Unix without X11/Wayland development libraries (the
//! pod, CI), a target-scoped `sdl-unix-console-build` feature permits the
//! build with SDL's dummy driver; any available desktop backends are
//! still autodetected.
//!
//! Test evidence discipline (Stage 1 authorization, item 4): tests run
//! SDL's dummy drivers and dispatch **synthetic events** through the same
//! path real events take. That verifies the state machine and lifecycle —
//! it does not verify physical hardware input. GUI evidence requires a
//! graphical session and is documented separately (engine-slice
//! `--window` mode; manual instructions in the repository README).
//!
//! Corrective-pass notes (Stage 1 review): driver selection uses SDL3's
//! safe hint interface (`SDL_HINT_VIDEO_DRIVER` / `SDL_HINT_AUDIO_DRIVER`,
//! renamed from the SDL2-era concatenated names) — Stage 1 contains
//! **zero project-owned unsafe code** — and focus loss/regain are recorded
//! through engine-core's Diagnostics subsystem (shared L0 logger, no
//! second diagnostics implementation).

pub mod app;
pub mod input;
pub mod platform;

pub use app::{
    FrameStats, LoopControl, QuitReason, RunSummary, ShellApp, ShellConfig, resident_set_kib,
};
pub use input::{InputSnapshot, InputState};
pub use platform::{DriverPreference, Platform};

/// Re-exported so applications configure the shell's diagnostics (focus
/// loss/regain records) through the shared engine-core subsystem.
pub use engine_core::diag::{Diagnostics, Level};

/// Re-exported input-identification types so applications bind keys and
/// buttons through the shell's curated surface rather than the SDL binding
/// directly — the shell is the SDL boundary, and `engine-core` stays
/// platform-independent.
pub use sdl3::keyboard::Keycode;
pub use sdl3::mouse::MouseButton;

/// The SDL3 version linked into this binary (the vendored build).
pub fn linked_sdl_version() -> sdl3::version::Version {
    Platform::linked_sdl_version()
}
