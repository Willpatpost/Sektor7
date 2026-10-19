//! Shared test helpers.
//!
//! SDL's event queue and initialization are process-global, so every
//! SDL-touching test serializes on this lock regardless of the test
//! harness's parallelism.

use std::sync::{Mutex, OnceLock};

use engine_shell::{DriverPreference, ShellApp, ShellConfig};

pub fn sdl_lock() -> &'static Mutex<()> {
    static LOCK: OnceLock<Mutex<()>> = OnceLock::new();
    LOCK.get_or_init(|| Mutex::new(()))
}

/// A dummy-driver shell — safe in headless environments (CI, the dev pod).
pub fn dummy_app(title: &str) -> ShellApp {
    ShellApp::new(ShellConfig {
        title: title.to_string(),
        width: 640,
        height: 480,
        target_fps: 60,
        driver: DriverPreference::Dummy,
    })
    .expect("dummy-driver shell must initialize in headless environments")
}
