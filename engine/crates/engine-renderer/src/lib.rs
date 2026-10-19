//! Stage 2 Vulkan presentation and first render (lead-authorized 2026-10-19;
//! TECH-ARCHITECTURE §6 Stage 2).
//!
//! Scope: direct Vulkan as the single rendering backend (T-02) via **ash
//! 0.38** (T-03 investigation concluded — see DECISIONS); SDL3-window
//! integration (surface + instance extensions through the sdl3 wrapper's
//! `ash` feature); instance, physical-device selection, logical device,
//! graphics/presentation queues, swapchain with out-of-date/suboptimal/
//! minimization handling; a depth-correct 3D scene with a 6-DoF camera
//! (camera policy lives in the application, not here); a minimal
//! GLSL→SPIR-V path (naga at build time, T-05); GPU timestamps, CPU-side
//! stats, validation-layer routing through engine-core's Diagnostics
//! tradition, and a minimal bitmap-font debug overlay.
//!
//! **Presentation direction (lead, 2026-10-19):** the engine's long-term
//! target is exceptionally smooth operation, including sustained 240+
//! native-rendered FPS where hardware, resolution, scene complexity, and
//! settings permit — a performance objective, NOT a claim the Stage 2
//! scene validates. This renderer imposes **no frame-rate ceiling**;
//! pacing (60/144/240/uncapped) is application policy, and rendering
//! cadence is independent of the future fixed-rate campaign simulation
//! (Stage 3, gated).
//!
//! **Unsafe policy (Stage 2 authorization, item 1):** all Vulkan-specific
//! unsafe code lives in the private [`vulkan`] module, which opens with
//! an invariants manifest (ownership, lifetime, synchronization,
//! destruction order). Everything else in this crate is unsafe-free;
//! `engine-core`'s `unsafe_code = deny` is untouched.
//!
//! Strictly excluded (Stage 2 authorization, item 6): Stage 3+ work
//! (campaign clock, NPC simulation, region LOD, persistence,
//! multiplayer), gameplay combat, production physics, editor, scripting,
//! audio, speculative threading — and no second graphics backend.

// Unsafe containment (Stage 2 authorization, item 1): this crate denies
// unsafe code at the source level; the private `vulkan` module overrides
// the deny with its own `#![allow(unsafe_code)]` plus an invariants
// manifest. (Cargo's `[lints.rust]` deny is passed on the rustc command
// line, which in-source allows cannot override — hence the attribute
// here instead.) `engine-core`'s Cargo-level deny is untouched.
#![deny(unsafe_code)]

pub mod config;
pub mod overlay;
pub mod scene;

mod renderer;
mod vulkan;

pub use config::{PresentModePreference, RenderConfig, ValidationPreference};
pub use renderer::{FrameResult, RenderStats, Renderer};
pub use vulkan::RendererError;
