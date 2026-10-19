//! The Stage 1 application shell: window + event pump + input intents +
//! a CPU-paced application loop.

use std::thread;
use std::time::{Duration, Instant};

use sdl3::EventPump;
use sdl3::event::Event;
use sdl3::mouse::MouseUtil;
use sdl3::video::{Window, WindowBuildError};

use crate::input::{InputSnapshot, InputState};
use crate::platform::{DriverPreference, Platform};

/// Configuration for [`ShellApp::new`].
pub struct ShellConfig {
    pub title: String,
    pub width: u32,
    pub height: u32,
    /// Target loop rate for frame pacing. Pacing uses OS monotonic time
    /// exclusively — the authoritative campaign simulation clock belongs
    /// to engine-core, is not present in Stage 1, and OS time never enters
    /// simulation logic (Stage 1 authorization, item 2).
    pub target_fps: u32,
    pub driver: DriverPreference,
}

/// What the frame callback wants the loop to do next.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LoopControl {
    Continue,
    Quit,
}

/// Why the application loop stopped.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum QuitReason {
    /// A window close request or SDL quit event arrived.
    Requested,
    /// The frame callback returned [`LoopControl::Quit`].
    Callback,
}

/// Frame-interval statistics over a whole run.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct FrameStats {
    pub count: u64,
    pub avg_ms: f64,
    pub p50_ms: f64,
    pub p95_ms: f64,
    pub p99_ms: f64,
    pub max_ms: f64,
}

impl FrameStats {
    fn from_intervals(intervals: &[Duration]) -> FrameStats {
        if intervals.is_empty() {
            return FrameStats {
                count: 0,
                avg_ms: 0.0,
                p50_ms: 0.0,
                p95_ms: 0.0,
                p99_ms: 0.0,
                max_ms: 0.0,
            };
        }
        let mut ms: Vec<f64> = intervals.iter().map(|d| d.as_secs_f64() * 1000.0).collect();
        ms.sort_by(|a, b| a.total_cmp(b));
        let pick = |q: f64| {
            let idx = ((q * ms.len() as f64).ceil() as usize)
                .saturating_sub(1)
                .min(ms.len() - 1);
            ms[idx]
        };
        FrameStats {
            count: ms.len() as u64,
            avg_ms: ms.iter().sum::<f64>() / ms.len() as f64,
            p50_ms: pick(0.50),
            p95_ms: pick(0.95),
            p99_ms: pick(0.99),
            max_ms: ms[ms.len() - 1],
        }
    }
}

/// Summary of one application-loop run.
pub struct RunSummary {
    pub frames: u64,
    pub wall_duration: Duration,
    pub frame_stats: FrameStats,
    pub quit_reason: QuitReason,
    pub rss_start_kib: Option<u64>,
    pub rss_end_kib: Option<u64>,
}

/// The Stage 1 application shell: a real SDL3 window, the OS event pump,
/// the input-intent state machine, and a CPU-driven paced loop. Rendering
/// and presentation are Stage 2 and are absent by authorization.
pub struct ShellApp {
    platform: Platform,
    window: Window,
    pump: EventPump,
    mouse: MouseUtil,
    input: InputState,
    frame_target: Duration,
}

impl ShellApp {
    pub fn new(config: ShellConfig) -> Result<ShellApp, String> {
        let platform = Platform::init(config.driver)?;
        let video = platform.sdl().video().map_err(|e| e.to_string())?;
        let window = video
            .window(&config.title, config.width, config.height)
            .position_centered()
            .resizable()
            .build()
            .map_err(|e: WindowBuildError| e.to_string())?;
        let pump = platform.sdl().event_pump().map_err(|e| e.to_string())?;
        let mouse = platform.sdl().mouse();
        let frame_target = Duration::from_secs_f64(1.0 / f64::from(config.target_fps.max(1)));
        Ok(ShellApp {
            platform,
            window,
            pump,
            mouse,
            input: InputState::new(),
            frame_target,
        })
    }

    /// The SDL window id (events are not filtered by id; Stage 1 is a
    /// single-window shell).
    pub fn window_id(&self) -> u32 {
        self.window.id()
    }

    pub fn window_size(&self) -> (u32, u32) {
        self.window.size()
    }

    pub fn is_held(&self, key: sdl3::keyboard::Keycode) -> bool {
        self.input.is_held(key)
    }

    /// Applies relative mouse capture to the window and mirrors it in the
    /// input state (look deltas are collected only while captured).
    pub fn set_relative_mouse(&mut self, on: bool) {
        self.mouse.set_relative_mouse_mode(&self.window, on);
        self.input.set_relative_capture(on);
    }

    /// The relative-mouse mode SDL reports for this window.
    pub fn relative_mouse(&self) -> bool {
        self.mouse.relative_mouse_mode(&self.window)
    }

    /// Pushes an event onto SDL's queue. Used by tests and tooling — this is
    /// event dispatch, not physical device input.
    pub fn push_event(&mut self, event: Event) -> Result<(), String> {
        let events = self.platform.sdl().event().map_err(|e| e.to_string())?;
        events.push_event(event).map_err(|e| e.to_string())
    }

    /// Pumps pending OS events through the input state machine and returns
    /// this frame's intent snapshot.
    pub fn frame(&mut self) -> InputSnapshot {
        for event in self.pump.poll_iter() {
            self.input.process(&event);
        }
        self.input.poll()
    }

    /// Runs the CPU-paced application loop until quit is requested or the
    /// callback returns [`LoopControl::Quit`]. Pacing uses OS monotonic
    /// time exclusively; if the loop falls behind the target cadence it
    /// resets the cadence instead of busy-catching-up.
    pub fn run<F>(&mut self, mut on_frame: F) -> RunSummary
    where
        F: FnMut(&mut ShellApp, &InputSnapshot) -> LoopControl,
    {
        let rss_start = resident_set_kib();
        let run_start = Instant::now();
        let mut frame_begin = run_start;
        let mut next_frame_at = run_start;
        let mut intervals: Vec<Duration> = Vec::new();
        let mut control = LoopControl::Continue;
        let mut quit_reason = QuitReason::Callback;

        while control == LoopControl::Continue && !self.input.quit_requested() {
            next_frame_at += self.frame_target;
            let snapshot = self.frame();
            control = on_frame(self, &snapshot);
            if control == LoopControl::Quit {
                quit_reason = QuitReason::Callback;
            } else if self.input.quit_requested() {
                quit_reason = QuitReason::Requested;
            }
            let frame_end = Instant::now();
            if next_frame_at > frame_end {
                thread::sleep(next_frame_at - frame_end);
            } else {
                next_frame_at = frame_end;
            }
            let next_begin = Instant::now();
            intervals.push(next_begin - frame_begin);
            frame_begin = next_begin;
        }

        RunSummary {
            frames: intervals.len() as u64,
            wall_duration: run_start.elapsed(),
            frame_stats: FrameStats::from_intervals(&intervals),
            quit_reason,
            rss_start_kib: rss_start,
            rss_end_kib: resident_set_kib(),
        }
    }

    /// Kept for Stage 3: the platform handle that will host the
    /// campaign-clock integration behind its own gate.
    pub fn platform(&self) -> &Platform {
        &self.platform
    }
}

/// Resident set size in KiB (Linux only; `None` elsewhere). Used for soak
/// memory-trend reporting.
#[cfg(target_os = "linux")]
pub fn resident_set_kib() -> Option<u64> {
    let status = std::fs::read_to_string("/proc/self/status").ok()?;
    for line in status.lines() {
        if let Some(rest) = line.strip_prefix("VmRSS:") {
            return rest.trim().trim_end_matches("kB").trim().parse().ok();
        }
    }
    None
}

#[cfg(not(target_os = "linux"))]
pub fn resident_set_kib() -> Option<u64> {
    None
}
