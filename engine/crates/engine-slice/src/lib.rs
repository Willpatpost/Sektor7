//! Interactive-slice application library (Stage 0).
//!
//! The binary is a thin stdin loop; all logic lives here so tests can drive
//! the same code path without spawning a process.

pub mod session;
pub mod window_demo;
pub mod world;
