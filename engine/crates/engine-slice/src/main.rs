//! Interactive slice binary: the Stage 0 stdin CLI for `engine-core`, plus
//! the Stage 1 SDL3 shell modes (`--window`, `--window --dummy`,
//! `--soak <secs>`).
//!
//! Rendering is Stage 2 and is absent by authorization; the window modes
//! are CPU-driven only.

use std::io::{self, Write};

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
