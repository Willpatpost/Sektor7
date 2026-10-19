//! Session command tests — the same code path the binary drives.

use engine_slice::session::{Outcome, Session, run_selftest};

fn run(session: &mut Session, line: &str) -> String {
    match session.execute_line(line).expect("command should succeed") {
        Outcome::Running(text) => text,
        Outcome::Quit => panic!("unexpected quit"),
    }
}

#[test]
fn schedules_executes_and_reports() {
    let mut session = Session::new();
    let out = run(&mut session, "at 3 10");
    assert!(out.starts_with("scheduled #0"), "{out}");
    assert!(out.contains("due=3"), "{out}");

    let out = run(&mut session, "tick 4");
    assert!(out.contains("clock=4"), "{out}");
    assert!(out.contains("executed=1"), "{out}");
    assert!(out.contains("watermark=3"), "{out}");

    // The demo chain: payload 10 (due 3) schedules payload 12 at tick 10,
    // which spawns payload 13 one tick later.
    let out = run(&mut session, "tick 8");
    assert!(out.contains("clock=12"), "{out}");
    assert!(out.contains("executed=3"), "{out}");
    assert!(out.contains("watermark=11"), "{out}");

    let counters = run(&mut session, "counters");
    assert!(counters.contains("payload 10"), "{counters}");
    assert!(counters.contains("payload 13"), "{counters}");
    assert!(counters.contains("total: 3"), "{counters}");
}

#[test]
fn crlf_lines_are_tolerated() {
    let mut session = Session::new();
    match session
        .execute_line("at 2 7\r\n")
        .expect("command should succeed")
    {
        Outcome::Running(text) => assert!(text.contains("scheduled"), "{text}"),
        Outcome::Quit => panic!("unexpected quit"),
    }
}

#[test]
fn unknown_command_is_an_error() {
    let mut session = Session::new();
    let error = session.execute_line("frobnicate").unwrap_err();
    assert!(error.contains("unknown command"), "{error}");
}

#[test]
fn missing_arguments_report_usage() {
    let mut session = Session::new();
    let error = session.execute_line("at 5").unwrap_err();
    assert!(error.contains("usage"), "{error}");
}

#[test]
fn quit_returns_quit_outcome() {
    let mut session = Session::new();
    assert!(matches!(
        session.execute_line("quit").unwrap(),
        Outcome::Quit
    ));
}

#[test]
fn fingerprint_format_and_determinism() {
    let mut first = Session::new();
    run(&mut first, "at 3 10");
    run(&mut first, "tick 5");
    let out = run(&mut first, "fingerprint");
    assert!(out.starts_with("engine="), "{out}");
    let hex = out
        .trim_start_matches("engine=")
        .split(" state=")
        .collect::<Vec<_>>();
    assert_eq!(hex.len(), 2, "{out}");
    assert_eq!(hex[0].len(), 16, "engine fingerprint is 16 hex chars");
    assert_eq!(hex[1].len(), 16, "state fingerprint is 16 hex chars");
    assert!(
        hex[0]
            .chars()
            .all(|c| c.is_ascii_hexdigit() && !c.is_ascii_uppercase()),
        "lowercase hex: {}",
        hex[0]
    );

    let mut second = Session::new();
    run(&mut second, "at 3 10");
    run(&mut second, "tick 5");
    assert_eq!(
        run(&mut second, "fingerprint"),
        out,
        "identical command sequences must produce identical fingerprints"
    );
}

#[test]
fn dependency_commands_block_and_release() {
    let mut session = Session::new();
    let out = run(&mut session, "at 8 1"); // prerequisite, due 8
    let seq = out
        .split('#')
        .nth(1)
        .expect("response carries the assigned seq")
        .split(' ')
        .next()
        .expect("seq token");
    run(&mut session, &format!("at 6 2 {seq}")); // dependent due 6 on #0

    run(&mut session, "tick 6");
    let status = run(&mut session, "status");
    assert!(status.contains("blocked_now=1"), "{status}");

    run(&mut session, "tick 2");
    let status = run(&mut session, "status");
    assert!(status.contains("blocked_now=0"), "{status}");
    assert!(status.contains("processed_through=8"), "{status}");
}

#[test]
fn late_arrival_is_reported() {
    let mut session = Session::new();
    run(&mut session, "tick 5");
    let out = run(&mut session, "at 2 9");
    assert!(out.contains("late arrival"), "{out}");
    let status = run(&mut session, "status");
    assert!(status.contains("late_arrivals=1"), "{status}");
}

#[test]
fn selftest_is_deterministic() {
    let first = run_selftest();
    let second = run_selftest();
    assert_eq!(first.0, second.0, "engine fingerprint stable across runs");
    assert_eq!(first.1, second.1, "state fingerprint stable across runs");
    assert_eq!(first.0.len(), 16);
    assert_eq!(first.1.len(), 16);
}
