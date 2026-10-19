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
    /// dispatch; they do not update physical device state).
    Dummy,
}

/// Owns the SDL3 context for one shell instance.
pub struct Platform {
    sdl: Sdl,
}

impl Platform {
    /// Initializes SDL3. `DriverPreference::Dummy` selects SDL's dummy
    /// drivers through SDL3's safe hint interface — `SDL_HINT_VIDEO_DRIVER`
    /// and `SDL_HINT_AUDIO_DRIVER`, renamed from the SDL2-era concatenated
    /// hint names (Stage 1 corrective pass, review finding 1: the earlier
    /// implementation used the SDL2 names, failed, and fell back to an
    /// unsafe environment-variable write; with the correct names the
    /// project-owned unsafe code is removed entirely).
    ///
    /// Process-wide limitation: hints are global while SDL is initialized,
    /// and SDL clears them on quit. Every `Platform::init` (re-)sets the
    /// hints it needs, and dropping a `Platform` (SDL quit) clears them,
    /// so sequential shells may switch driver modes freely — concurrently
    /// live shells must share one driver choice.
    pub fn init(driver: DriverPreference) -> Result<Platform, String> {
        if driver == DriverPreference::Dummy {
            sdl3::hint::set("SDL_VIDEO_DRIVER", "dummy");
            sdl3::hint::set("SDL_AUDIO_DRIVER", "dummy");
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
