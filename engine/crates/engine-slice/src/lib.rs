//! Interactive-slice application library (Stage 0).
//!
//! The binary is a thin stdin loop; all logic lives here so tests can drive
//! the same code path without spawning a process.

pub mod benchmark;
pub mod crossing;
pub mod frame_timing;
pub mod math;
pub mod npc;
pub mod region;
pub mod region_render;
pub mod render_demo;
pub mod save_demo;
pub mod session;
pub mod sim;
pub mod sim_demo;
pub mod window_demo;
pub mod world;
