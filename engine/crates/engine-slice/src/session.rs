//! Command session for the interactive slice — testable without stdin.

use engine_core::{EventClass, EventId, EventSpec, Level, Schedule, ScheduleError, Spine, Tick};

use crate::world::DemoWorld;

/// Default per-tick execution budget for the slice — a policy *value*; the
/// budget mechanism lives in the engine (P5).
pub const BUDGET: u32 = 32;

/// Result of one command line.
#[derive(Debug)]
pub enum Outcome {
    /// The session continues; the response line to print.
    Running(String),
    /// Clean shutdown requested (`quit`).
    Quit,
}

pub struct Session {
    spine: Spine<DemoWorld>,
}

impl Default for Session {
    fn default() -> Self {
        Self::new()
    }
}

impl Session {
    pub fn new() -> Self {
        Session {
            spine: Spine::new(DemoWorld::new(), BUDGET),
        }
    }

    pub fn spine(&self) -> &Spine<DemoWorld> {
        &self.spine
    }

    pub fn spine_mut(&mut self) -> &mut Spine<DemoWorld> {
        &mut self.spine
    }

    /// Executes one command line (CRLF tolerated). Errors are user-facing.
    pub fn execute_line(&mut self, line: &str) -> Result<Outcome, String> {
        let line = line.trim();
        if line.is_empty() {
            return Ok(Outcome::Running(String::new()));
        }
        let mut words = line.split_whitespace();
        let command = words.next().unwrap_or_default();
        let args: Vec<&str> = words.collect();
        match command {
            "help" => Ok(Outcome::Running(help_text())),
            "tick" => self.cmd_tick(&args),
            "at" => self.cmd_at(&args),
            "rel" => self.cmd_rel(&args),
            "status" => Ok(Outcome::Running(self.status_line())),
            "counters" => Ok(Outcome::Running(self.counters_line())),
            "ledger" => self.cmd_ledger(&args),
            "fingerprint" => Ok(Outcome::Running(format!(
                "engine={} state={}",
                self.spine.engine_fingerprint_hex(),
                self.spine.state_fingerprint_hex()
            ))),
            "diag" => self.cmd_diag(&args),
            "quit" | "exit" => Ok(Outcome::Quit),
            other => Err(format!("unknown command: {other} (try 'help')")),
        }
    }

    fn cmd_tick(&mut self, args: &[&str]) -> Result<Outcome, String> {
        let count = match args.first() {
            Some(text) => text
                .parse::<u64>()
                .map_err(|_| format!("tick count must be a number: {text}"))?,
            None => 1,
        };
        self.spine.run_ticks(count.clamp(1, 10_000));
        Ok(Outcome::Running(self.summary_line()))
    }

    fn cmd_at(&mut self, args: &[&str]) -> Result<Outcome, String> {
        let (due, payload, deps) = parse_schedule_args(args)?;
        let spec = EventSpec::new(
            EventClass::new(0),
            Schedule::AtLogicalTime { due: Tick(due) },
            payload,
        )
        .with_dependencies(deps);
        self.schedule_and_report(spec)
    }

    fn cmd_rel(&mut self, args: &[&str]) -> Result<Outcome, String> {
        let (delay, payload, deps) = parse_schedule_args(args)?;
        // External execution-relative scheduling anchors at the current
        // clock; handler follow-ups anchor at their event's execution tick.
        let spec = EventSpec::new(
            EventClass::new(0),
            Schedule::ExecutionRelative {
                origin: self.spine.clock(),
                delay,
            },
            payload,
        )
        .with_dependencies(deps);
        self.schedule_and_report(spec)
    }

    fn schedule_and_report(&mut self, spec: EventSpec) -> Result<Outcome, String> {
        let due = spec.schedule.due();
        match self.spine.schedule(spec) {
            Ok(id) => {
                let mut line = format!("scheduled {id} due={due}");
                if due < self.spine.clock() {
                    line.push_str(" (late arrival — the safe watermark may retreat)");
                }
                Ok(Outcome::Running(line))
            }
            Err(ScheduleError::UnknownDependency(dep)) => Err(format!(
                "unknown dependency {dep}: dependencies must reference already-scheduled events"
            )),
        }
    }

    fn cmd_ledger(&self, args: &[&str]) -> Result<Outcome, String> {
        let count = match args.first() {
            Some(text) => text
                .parse::<usize>()
                .map_err(|_| format!("count must be a number: {text}"))?,
            None => 10,
        };
        let count = count.clamp(1, 1000);
        let ledger = self.spine.ledger();
        let start = ledger.len().saturating_sub(count);
        let lines: Vec<String> = ledger[start..].iter().map(|r| r.to_string()).collect();
        Ok(Outcome::Running(if lines.is_empty() {
            "no executions yet".to_string()
        } else {
            lines.join("\n")
        }))
    }

    fn cmd_diag(&mut self, args: &[&str]) -> Result<Outcome, String> {
        match args.first() {
            None => Ok(Outcome::Running(format!(
                "diag level: {}",
                self.spine.diagnostics().min_level().as_str()
            ))),
            Some(text) => {
                let level = Level::parse(text).ok_or_else(|| {
                    format!("unknown level: {text} (off|trace|debug|info|warn|error)")
                })?;
                self.spine.diagnostics_mut().set_min_level(level);
                Ok(Outcome::Running(format!(
                    "diag level set: {}",
                    level.as_str()
                )))
            }
        }
    }

    fn summary_line(&self) -> String {
        format!(
            "clock={} executed={} pending={} blocked_now={} watermark={}",
            self.spine.clock(),
            self.spine.stats().executed,
            self.spine.pending_count(),
            self.spine.blocked_count(),
            opt_tick(self.spine.processed_through())
        )
    }

    fn status_line(&self) -> String {
        let stats = self.spine.stats();
        format!(
            "clock={} budget={} pending={} blocked_now={} executed={}\nmax_executed_due={} min_pending_due={} processed_through={}\nstats: scheduled={} executed={} late_arrivals={} late_executions={} blocked_transitions={} invalid_dropped={} budget_exhausted={}",
            self.spine.clock(),
            self.spine.budget(),
            self.spine.pending_count(),
            self.spine.blocked_count(),
            stats.executed,
            opt_tick(self.spine.max_executed_due()),
            opt_tick(self.spine.min_pending_due()),
            opt_tick(self.spine.processed_through()),
            stats.scheduled,
            stats.executed,
            stats.late_arrivals,
            stats.late_executions,
            stats.blocked_transitions,
            stats.invalid_dropped,
            stats.budget_exhausted_ticks
        )
    }

    fn counters_line(&self) -> String {
        let world = self.spine.world();
        let mut lines: Vec<String> = world
            .counters()
            .iter()
            .map(|(payload, count)| format!("payload {payload}: {count} executions"))
            .collect();
        if lines.is_empty() {
            "no executions yet".to_string()
        } else {
            lines.push(format!("total: {}", world.total_executions()));
            lines.join("\n")
        }
    }
}

fn parse_schedule_args(args: &[&str]) -> Result<(u64, u64, Vec<EventId>), String> {
    if args.len() < 2 {
        return Err("usage: <time> <payload> [dep_seq ...]".to_string());
    }
    let time = args[0]
        .parse::<u64>()
        .map_err(|_| format!("time must be a number: {}", args[0]))?;
    let payload = args[1]
        .parse::<u64>()
        .map_err(|_| format!("payload must be a number: {}", args[1]))?;
    let mut deps = Vec::new();
    for text in &args[2..] {
        let seq = text
            .parse::<u64>()
            .map_err(|_| format!("dependency must be a seq number: {text}"))?;
        deps.push(EventId { seq });
    }
    Ok((time, payload, deps))
}

fn opt_tick(tick: Option<Tick>) -> String {
    tick.map(|t| t.as_u64().to_string())
        .unwrap_or_else(|| "none".to_string())
}

fn help_text() -> String {
    [
        "commands:",
        "  tick [n]                        advance n ticks (default 1)",
        "  at <due> <payload> [deps...]    schedule at a fixed logical time",
        "  rel <delay> <payload> [deps...] schedule relative to the current clock",
        "  status                          clock, watermark and stats",
        "  counters                        demo-world execution counters",
        "  ledger [n]                      last n execution records (default 10)",
        "  fingerprint                     engine and state fingerprints (16-hex)",
        "  diag [level]                    show/set diagnostics level",
        "  quit                            clean shutdown (EOF works too)",
    ]
    .join("\n")
}

fn run_cmd(session: &mut Session, line: &str) -> String {
    match session
        .execute_line(line)
        .expect("fixed command must succeed")
    {
        Outcome::Running(text) => text,
        Outcome::Quit => panic!("fixed command must not quit"),
    }
}

/// Deterministic built-in scenario for cross-process double-run fingerprint
/// comparison: run `engine-slice --fingerprint-selftest` twice — the outputs
/// must match (the project's double-run determinism discipline).
pub fn run_selftest() -> (String, String) {
    let mut session = Session::new();
    for (due, payload) in [(3u64, 10u64), (3, 21), (5, 15), (9, 30), (12, 7)] {
        run_cmd(&mut session, &format!("at {due} {payload}"));
    }
    run_cmd(&mut session, "rel 4 14");
    // #2 is the third scheduled event ("at 5 15"): a dependent due *before*
    // its prerequisite, so the scenario exercises causality blocking.
    run_cmd(&mut session, "at 4 5 2");
    session.spine_mut().run_to_completion(500);
    (
        session.spine().engine_fingerprint_hex(),
        session.spine().state_fingerprint_hex(),
    )
}
