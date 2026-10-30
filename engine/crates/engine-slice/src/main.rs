//! Interactive slice binary: the Stage 0 stdin CLI for `engine-core`, the
//! Stage 1 SDL3 shell modes (`--window`, `--window --dummy`,
//! `--soak <secs>`), the Stage 2 Vulkan render demo (`--render`, with
//! `--present auto|fifo|mailbox|immediate`, `--fps <n>|uncapped`,
//! `--frames <n>`, `--no-overlay`, `--validation auto|on|off`,
//! `--simulate-resize`, `--sim-budget <n>`, `--sim-near-reserve <n>`,
//! and the Stage 4 region visualization `--regions` with the
//! pipeline-replacement verification scenario `--replace-pipelines`),
//! and the Stage 3 headless simulation (`--sim`, with `--seed`,
//! `--ticks`, `--budget`, `--near-reserve`, `--rate`, `--cadence
//! <hz>|irregular`, and the Stage 4 region scenarios `--presence
//! stationary|walk|tour`), and the Stage 5 Batch 1 canonical-save
//! self-test (`--save-selftest`: the fixed demo scenario's save length
//! and FNV-1a digest over the canonical bytes), and the Slice 0
//! integration demonstration (`--demo-persist <dir>`: ONE
//! authoritative, persistently-orchestrated campaign behind the
//! render loop — founded-and-anchored or recovered from the durable
//! state, the Space action on the nearest citizen, the truthful
//! pending-vs-confirmed persistence overlay, and the graceful durable
//! close on exit; `--headless --ticks <n> [--action-at <tick>]` runs
//! the same lifecycle deterministically without a GPU).

use std::io::{self, Write};
use std::path::PathBuf;

use engine_slice::region::PresenceScenario;
use engine_slice::session::{Outcome, Session, run_selftest};
use engine_slice::window_demo;

fn main() {
    // C3-F5B: the namespaced integrated-soak flags are valid ONLY
    // within `--demo-persist` — outside it they reject every top-level
    // mode with a visible nonzero error instead of falling through to
    // another command path (the Stage 1 `--soak` legacy command and
    // all other accepted modes are unchanged; this guard precedes the
    // whole dispatch cascade).
    const INTEGRATED_SOAK_FLAGS: [&str; 5] = [
        "--soak-wall-seconds",
        "--scenario",
        "--scenario-seed",
        "--evidence-dir",
        "--stabilization-deadline-secs",
    ];
    let raw_args: Vec<String> = std::env::args().collect();
    let uses_soak_flags = INTEGRATED_SOAK_FLAGS
        .iter()
        .any(|flag| raw_args.iter().any(|arg| arg == flag));
    if uses_soak_flags && !raw_args.iter().any(|arg| arg == "--demo-persist") {
        eprintln!(
            "error: the integrated-soak flags ({}) require --demo-persist \
             (the integrated persistence demo)",
            INTEGRATED_SOAK_FLAGS.join(" ")
        );
        std::process::exit(1);
    }
    if std::env::args().any(|arg| arg == "--fingerprint-selftest") {
        let (engine, state) = run_selftest();
        println!("engine={engine}");
        println!("state={state}");
        return;
    }

    if std::env::args().any(|arg| arg == "--save-selftest") {
        // Stage 5 Batch 1: canonical save bytes of a fixed scenario —
        // length + FNV-1a digest, deterministic across processes and
        // platforms (CI double-runs this and pins both values).
        let (len, digest) = engine_slice::save_demo::run_save_selftest();
        println!("save-len={len}");
        println!("save-fnv={digest}");
        return;
    }

    if std::env::args().any(|arg| arg == "--benchmark-persistence") {
        // Stage 5 Batch 5: the persistence performance benchmark (the
        // GO's fourth item) — pod results, NOT physical-rig evidence.
        engine_slice::benchmark::run();
        return;
    }

    if std::env::args().any(|arg| arg == "--benchmark-instrumentation") {
        // S6-C1: the instrumentation-overhead benchmark — the same
        // deterministic headless workload with evidence collection
        // disabled and enabled, plus the produce-path cost (pod
        // results, observational; never CI thresholds).
        engine_slice::benchmark::run_instrumentation_overhead();
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

    if let Some(position) = args.iter().position(|arg| arg == "--demo-persist") {
        // Slice 0 integration demonstration: ONE authoritative,
        // persistently-orchestrated campaign behind the render loop
        // (founded-and-anchored or recovered; the Space action; the
        // truthful persistence overlay; the graceful durable close).
        let Some(dir) = args.get(position + 1) else {
            eprintln!(
                "usage: --demo-persist <campaign-dir> [--headless --ticks <n> \
                 [--action-at <tick>]] [--fps <n>|uncapped] [--frames <n>] \
                 [--validation auto|on|off] [--present auto|fifo|mailbox|immediate] \
                 [--no-overlay] [--scenario stage6-v1|stage6-v1-short \
                 [--scenario-seed <n>] [--evidence-dir <dir>]] \
                 [--soak-wall-seconds <n> --scenario <id> --evidence-dir <dir> \
                 [--stabilization-deadline-secs <n>]]"
            );
            std::process::exit(1);
        };
        let mut options = engine_slice::persist_demo::DemoOptions {
            campaign_dir: PathBuf::from(dir),
            fps: 60,
            present: engine_renderer::PresentModePreference::Auto,
            frames: None,
            overlay: true,
            validation: engine_renderer::ValidationPreference::Auto,
            headless: false,
            headless_ticks: 600,
            action_at: None,
            poison_at_tick: None,
            abort_at_tick: None,
            evidence: None,
            evidence_fail_writes: false,
            scenario: None,
            scenario_seed: None,
            soak_wall_seconds: None,
            stabilization_deadline_secs: None,
        };
        let mut ticks_given = false;
        let mut index = position + 2;
        while index < args.len() {
            match args[index].as_str() {
                "--headless" => options.headless = true,
                // The S6-C4 crossing-latency profiling flag (the
                // profiling GO): strictly observational, opt-in,
                // inactive by default — enables the per-step marks;
                // the summary prints with the run summary.
                "--crossing-profile" => engine_slice::crossing_profile::enable(),
                "--ticks" => {
                    if let Some(value) = args.get(index + 1)
                        && let Ok(ticks) = value.parse::<u64>()
                    {
                        options.headless_ticks = ticks.clamp(1, 1_000_000);
                        ticks_given = true;
                        index += 1;
                    }
                }
                "--action-at" => {
                    if let Some(value) = args.get(index + 1)
                        && let Ok(tick) = value.parse::<u64>()
                    {
                        options.action_at = Some(tick);
                        index += 1;
                    }
                }
                // Test-only (the S6-C4 deterministic interruption
                // seam — see DemoOptions::abort_at_tick): the
                // controlled hard exit at a completed tick boundary,
                // reserved code 9.
                "--abort-after-tick" => {
                    if let Some(value) = args.get(index + 1)
                        && let Ok(tick) = value.parse::<u64>()
                    {
                        options.abort_at_tick = Some(tick);
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
                "--validation" => {
                    if let Some(value) = args.get(index + 1)
                        && let Some(preference) =
                            engine_renderer::ValidationPreference::parse(value)
                    {
                        options.validation = preference;
                        index += 1;
                    }
                }
                "--present" => {
                    if let Some(value) = args.get(index + 1)
                        && let Some(mode) = engine_renderer::PresentModePreference::parse(value)
                    {
                        options.present = mode;
                        index += 1;
                    }
                }
                _ => {}
            }
            index += 1;
        }
        // S6-C3: the namespaced integrated-soak flags — STRICT
        // parsing (visible errors with a nonzero exit; never silent
        // defaults, never silent clamps), then the relational rules
        // against the mode context. All existing non-soak behavior is
        // unchanged (the legacy lenient flags above stay exactly as
        // they were).
        let soak = match engine_slice::soak_scenario::SoakOptions::parse(&args) {
            Ok(soak) => soak,
            Err(message) => {
                eprintln!("error: {message}");
                std::process::exit(1);
            }
        };
        if let Err(message) = soak.validate(
            options.headless,
            options.frames,
            options.action_at,
            ticks_given,
        ) {
            eprintln!("error: {message}");
            std::process::exit(1);
        }
        options.scenario = soak.scenario;
        options.scenario_seed = soak.scenario_seed;
        options.soak_wall_seconds = soak.wall_seconds;
        options.stabilization_deadline_secs = soak.stabilization_deadline_secs;
        if let Some(evidence_dir) = &soak.evidence_dir {
            // The collector's exclusive-create destination rule
            // enforces the fresh evidence directory (a prior package
            // is never overwritten — the typed refusal names it).
            let label = options.scenario.map(|id| id.as_str()).unwrap_or("demo");
            options.evidence = Some(engine_slice::soak_evidence::EvidenceConfig::new(
                evidence_dir.clone(),
                label,
            ));
        }
        // The headless replay's tick budget defaults to the scenario
        // horizon when --scenario is given without --ticks (an
        // explicit --ticks caps the replay — the interrupted-run
        // fixtures use exactly that).
        if options.headless
            && let Some(id) = options.scenario
            && !ticks_given
        {
            options.headless_ticks = id.horizon();
        }
        if options.headless {
            match engine_slice::persist_demo::run_persist_demo_headless(options) {
                Ok(report) => {
                    report.print();
                    if report.application_failure.is_some()
                        || report.close.failure.is_some()
                        || report.evidence_failed()
                        || report.scenario_failed()
                    {
                        std::process::exit(1);
                    }
                }
                Err(message) => {
                    eprintln!("error: {message}");
                    std::process::exit(1);
                }
            }
        } else if let Err(message) = engine_slice::persist_demo::run_persist_demo(options) {
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
                "--replace-pipelines" => options.replace_pipelines = true,
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
         --demo-persist <dir> for the Slice 0 integration demonstration; \
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
