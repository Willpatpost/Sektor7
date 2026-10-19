//! Interactive slice binary (Stage 0): drives `engine-core` from stdin.
//!
//! Rendering and SDL3 arrive in later stages — this binary demonstrates the
//! engine mechanics headlessly, which is also how CI runs them.

use std::io::{self, Write};

use engine_slice::session::{Outcome, Session, run_selftest};

fn main() {
    if std::env::args().any(|arg| arg == "--fingerprint-selftest") {
        let (engine, state) = run_selftest();
        println!("engine={engine}");
        println!("state={state}");
        return;
    }

    println!(
        "Sektor7 original engine — Stage 0 interactive slice (engine-core demo; no renderer yet)"
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
