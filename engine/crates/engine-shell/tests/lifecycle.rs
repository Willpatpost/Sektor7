//! Stage 1 lifecycle tests: repeated initialization/shutdown and 100
//! create/close cycles (Stage 1 authorization, item 5).

mod common;

use engine_shell::Platform;

fn lock() -> std::sync::MutexGuard<'static, ()> {
    common::sdl_lock()
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
}

#[test]
fn linked_sdl_version_is_the_vendored_3_4_18() {
    let _guard = lock();
    let version = Platform::linked_sdl_version();
    assert_eq!(
        (version.major, version.minor, version.patch),
        (3, 4, 18),
        "the vendored SDL 3.4.18 build must be linked"
    );
}

#[test]
fn hundred_create_close_cycles() {
    let _guard = lock();
    for cycle in 0..100 {
        let mut app = common::dummy_app(&format!("lifecycle-{cycle}"));
        let snapshot = app.frame();
        assert!(!snapshot.quit_requested, "cycle {cycle}");
        let (width, height) = app.window_size();
        assert!(
            (width, height) == (640, 480),
            "cycle {cycle}: {width}x{height}"
        );
        // Dropping closes the window and releases the (refcounted) SDL
        // context; the next iteration reinitializes from scratch.
        drop(app);
    }
}
