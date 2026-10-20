//! Interactive slice binary: the Stage 0 stdin CLI for `engine-core`, the
//! Stage 1 SDL3 shell modes (`--window`, `--window --dummy`,
//! `--soak <secs>`), the Stage 2 Vulkan render demo (`--render`, with
//! `--present auto|fifo|mailbox|immediate`, `--fps <n>|uncapped`,
//! `--frames <n>`, `--no-overlay`, `--validation auto|on|off`,
//! `--simulate-resize`, `--sim-budget <n>`, `--sim-near-reserve <n>`,
//! and the Stage 4 region visualization `--regions`),
//! and the Stage 3 headless simulation (`--sim`, with `--seed`,
//! `--ticks`, `--budget`, `--near-reserve`, `--rate`, `--cadence
//! <hz>|irregular`, and the Stage 4 region scenarios `--presence
//! stationary|walk|tour`).

use std::io::{self, Write};

use engine_slice::region::PresenceScenario;
use engine_slice::session::{Outcome, Session, run_selftest};
use engine_slice::window_demo;

fn main() {
    if std::env::args().any(|arg| arg == "--fingerprint-selftest") {
        let (engine, state) = run_selftest();
        println!("engine={engine}");
        println!("state={state}");
        return;
    }

    let args: Vec<String> = std::env::args().collect();

    if args.iter().any(|arg| arg == "--sim") {
        // Stage 3 headless simulation (deterministic; no GPU, no window).
        let mut npc_config = engine_slice::npc::NpcConfig::default();
        let mut sim_config = engine_slice::sim::SimConfig::default();
        let mut ticks: u64 = 600;
        let mut cadence_hz: u32 = 60;
        let mut presence: Option<PresenceScenario> = None;
        let mut index = 1;
        while index < args.len() {
            match args[index].as_str() {
                "--seed" => {
                    if let Some(value) = args.get(index + 1)
                        && let Ok(seed) = value.parse::<u64>()
                    {
                        npc_config.seed = seed;
                        index += 1;
                    }
                }
                "--ticks" => {
                    if let Some(value) = args.get(index + 1)
                        && let Ok(count) = value.parse::<u64>()
                    {
                        ticks = count.clamp(1, 1_000_000);
                        index += 1;
                    }
                }
                "--budget" => {
                    if let Some(value) = args.get(index + 1)
                        && let Ok(budget) = value.parse::<u32>()
                    {
                        sim_config.budget = budget.clamp(1, 100_000);
                        index += 1;
                    }
                }
                "--near-reserve" => {
                    if let Some(value) = args.get(index + 1)
                        && let Ok(reserve) = value.parse::<u32>()
                    {
                        sim_config.near_reserve = Some(reserve);
                        index += 1;
                    }
                }
                "--rate" => {
                    if let Some(value) = args.get(index + 1)
                        && let Ok(rate) = value.parse::<u32>()
                    {
                        sim_config.tick_rate_hz = rate.clamp(1, 1000);
                        index += 1;
                    }
                }
                "--cadence" => {
                    if let Some(value) = args.get(index + 1) {
                        if value == "irregular" {
                            cadence_hz = 0;
                            index += 1;
                        } else if let Ok(hz) = value.parse::<u32>() {
                            cadence_hz = hz.clamp(1, 1000);
                            index += 1;
                        }
                    }
                }
                "--presence" => {
                    // Stage 4 region scenarios (deterministic scripted
                    // presence over the default region ring layout).
                    if let Some(value) = args.get(index + 1) {
                        match value.as_str() {
                            "stationary" => {
                                presence = Some(PresenceScenario::Stationary {
                                    position: (100.0, 0.0),
                                });
                                index += 1;
                            }
                            "walk" => {
                                presence = Some(PresenceScenario::Walk {
                                    from: (100.0, 0.0),
                                    to: (-100.0, 0.0),
                                    ticks: 300,
                                });
                                index += 1;
                            }
                            "tour" => {
                                presence = Some(PresenceScenario::Tour {
                                    ticks_per_region: 75,
                                });
                                index += 1;
                            }
                            _ => {}
                        }
                    }
                }
                _ => {}
            }
            index += 1;
        }
        if let Err(message) = engine_slice::sim_demo::run_sim_demo(
            npc_config, sim_config, ticks, cadence_hz, presence,
        ) {
            eprintln!("error: {message}");
            std::process::exit(1);
        }
        return;
    }

    if args.iter().any(|arg| arg == "--render") {
        let mut options = engine_slice::render_demo::RenderOptions::default();
        let mut index = 1;
        while index < args.len() {
            match args[index].as_str() {
                "--present" => {
                    if let Some(value) = args.get(index + 1)
                        && let Some(mode) = engine_renderer::PresentModePreference::parse(value)
                    {
                        options.present = mode;
                        index += 1;
                    }
                }
                "--fps" => {
                    if let Some(value) = args.get(index + 1) {
                        if value == "uncapped" {
                            options.fps = 0;
                            index += 1;
                        } else if let Ok(fps) = value.parse::<u32>() {
                            options.fps = fps.clamp(1, 1000);
                            index += 1;
                        }
                    }
                }
                "--frames" => {
                    if let Some(value) = args.get(index + 1)
                        && let Ok(frames) = value.parse::<u64>()
                    {
                        options.frames = Some(frames.clamp(1, 1_000_000));
                        index += 1;
                    }
                }
                "--no-overlay" => options.overlay = false,
                "--simulate-resize" => options.resize_test = true,
                "--regions" => options.regions = true,
                "--sim-budget" => {
                    if let Some(value) = args.get(index + 1)
                        && let Ok(budget) = value.parse::<u32>()
                    {
                        options.sim_budget = Some(budget.clamp(1, 100_000));
                        index += 1;
                    }
                }
                "--sim-near-reserve" => {
                    if let Some(value) = args.get(index + 1)
                        && let Ok(reserve) = value.parse::<u32>()
                    {
                        options.sim_near_reserve = Some(reserve.clamp(1, 100_000));
                        index += 1;
                    }
                }
                "--validation" => {
                    if let Some(value) = args.get(index + 1)
                        && let Some(preference) =
                            engine_renderer::ValidationPreference::parse(value)
                    {
                        options.validation = preference;
                        index += 1;
                    }
                }
                _ => {}
            }
            index += 1;
        }
        if let Err(message) = engine_slice::render_demo::run_render_demo(options) {
            eprintln!("error: {message}");
            if message.contains("Vulkan loader") {
                eprintln!(
                    "(no Vulkan runtime installed? headless smoke uses \
                     SDL_VIDEODRIVER=offscreen plus a software ICD)"
                );
            }
            std::process::exit(1);
        }
        return;
    }

    if args.iter().any(|arg| arg == "--window") {
        let dummy = args.iter().any(|arg| arg == "--dummy");
        // Headless smoke: 180 frames (~3 s at the 60 fps target).
        let frame_limit = if dummy { Some(180) } else { None };
        if let Err(message) = window_demo::run_window_demo(dummy, frame_limit) {
            eprintln!("error: {message}");
            if !dummy {
                eprintln!(
                    "(no graphical session available? \
                     use `--window --dummy` for the headless smoke)"
                );
            }
            std::process::exit(1);
        }
        return;
    }

    if let Some(position) = args.iter().position(|arg| arg == "--soak") {
        let seconds: u64 = args
            .get(position + 1)
            .and_then(|text| text.parse().ok())
            .unwrap_or(60)
            .clamp(1, 3600);
        let real = args.iter().any(|arg| arg == "--real");
        if let Err(message) = window_demo::run_soak(seconds, real) {
            eprintln!("error: {message}");
            std::process::exit(1);
        }
        return;
    }

    println!(
        "Sektor7 original engine — interactive slice (stdin CLI; \
         --render for the Stage 2 Vulkan demo; \
         --window / --window --dummy for the Stage 1 shell; \
         --soak <secs> for the loop soak; --fingerprint-selftest for determinism)"
    );
    println!("Type 'help' for commands; 'quit' or Ctrl-D exits cleanly.");

    let mut session = Session::new();
    let stdin = io::stdin();
    let mut stdout = io::stdout();
    let mut line = String::new();
    loop {
        print!("> ");
        let _ = stdout.flush();
        line.clear();
        match stdin.read_line(&mut line) {
            Ok(0) => break, // EOF: clean shutdown
            Ok(_) => match session.execute_line(&line) {
                Ok(Outcome::Running(text)) => {
                    if !text.is_empty() {
                        let _ = writeln!(stdout, "{text}");
                    }
                }
                Ok(Outcome::Quit) => break,
                Err(message) => {
                    let _ = writeln!(stdout, "error: {message}");
                }
            },
            Err(error) => {
                let _ = writeln!(stdout, "input error: {error}");
                break;
            }
        }
    }
    let _ = writeln!(stdout, "clean shutdown");
}
