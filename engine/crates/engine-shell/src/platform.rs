//! SDL3 platform initialization (L0 Platform Shell seed, Stage 1).
//!
//! `Platform::init` owns the SDL3 context. SDL initialization is
//! refcounted, so constructing and dropping `Platform` repeatedly is
//! supported — the lifecycle tests exercise exactly that (repeated
//! initialization/shutdown, 100 create/close cycles).

use sdl3::Sdl;
use sdl3::version::Version;

/// Which SDL video/audio drivers to request at initialization.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DriverPreference {
    /// Let SDL pick the platform's real drivers (Wayland/X11 on Linux,
    /// Win32 on Windows). Requires a graphical session.
    Real,
    /// Force SDL's dummy video and audio drivers — for headless
    /// environments and CI. Windows created this way are logical only.
    /// This yields **dispatch-level** test evidence, not hardware input
    /// verification (Stage 1 authorization, item 4: synthetic events test
    /// dispatch; they do not update physical device state). Driver
    /// selection is process-wide and sticky: SDL3 reads the driver
    /// environment variables once at subsystem init, so a process that has
    /// selected `Dummy` keeps it.
    Dummy,
}

/// The single narrowly-scoped `unsafe` in project-owned Stage 1 code,
/// documented for review per the Stage 1 authorization (item 1).
///
/// SDL3 selects its video/audio drivers from the `SDL_VIDEODRIVER` /
/// `SDL_AUDIODRIVER` environment variables at subsystem initialization —
/// the SDL2-era hints were removed, so there is no API-level override.
/// `std::env::set_var` is `unsafe` in Rust 2024 because the process
/// environment is global state.
///
/// Safety contract: [`Platform::init`] is the only caller and invokes this
/// before SDL initialization; SDL reads each variable exactly once, at
/// subsystem init. Test binaries serialize all SDL work — including this
/// call — under the shared `sdl_lock` (tests/common), and the demo binary
/// is single-threaded at `Platform::init`. Nothing else in the workspace
/// reads these variables.
#[allow(unsafe_code)]
fn set_dummy_driver_environment() {
    // SAFETY: the only concurrent observer is SDL's one-time read at
    // lock-serialized subsystem initialization — see the contract above.
    unsafe {
        std::env::set_var("SDL_VIDEODRIVER", "dummy");
        std::env::set_var("SDL_AUDIODRIVER", "dummy");
    }
}

/// Owns the SDL3 context for one shell instance.
pub struct Platform {
    sdl: Sdl,
}

impl Platform {
    /// Initializes SDL3. `DriverPreference::Dummy` selects SDL's dummy
    /// drivers *before* initialization — SDL3 reads the driver environment
    /// variables at subsystem init and has no API-level override (the
    /// SDL2-era hints were removed).
    pub fn init(driver: DriverPreference) -> Result<Platform, String> {
        if driver == DriverPreference::Dummy {
            set_dummy_driver_environment();
        }
        let sdl = sdl3::init().map_err(|e| e.to_string())?;
        Ok(Platform { sdl })
    }

    pub fn sdl(&self) -> &Sdl {
        &self.sdl
    }

    /// The SDL3 version linked into this binary (the vendored build).
    pub fn linked_sdl_version() -> Version {
        sdl3::version::version()
    }
}
