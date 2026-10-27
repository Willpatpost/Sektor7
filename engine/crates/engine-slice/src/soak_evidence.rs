//! S6-C1 observational evidence collection (Stage 6 Checkpoint 1, the
//! owner GO of 2026-10-27): a composition-level, strictly opt-in
//! collector for the integrated demonstration's full-run measurement
//! streams. The contract (the accepted measurement plan v2, HOLD-3):
//!
//! * **Never on the critical path.** Sample production is a nonblocking
//!   `try_send` onto a bounded channel; a dedicated writer thread owns
//!   all serialization and disk work, buffered, flushed in batches —
//!   never a synchronous flush on the authoritative tick or render
//!   submission path.
//! * **No silent loss.** Every stream keeps produced / written / lost /
//!   write-failed counts; a full queue or a failing writer counts the
//!   loss and fails the evidence verdict — the run's logical outcome is
//!   reported independently and the graceful close is never bypassed.
//! * **Null ≠ zero.** Unsupported GPU timestamps serialize as `null`
//!   with their absence countable, never as a measured `0`.
//! * **Observational only.** Nothing here touches logical state; the
//!   instrumentation-enabled and -disabled runs must produce
//!   byte-identical logical fingerprints (the integration gate).
//! * **Bounded memory.** In-memory retention is the channel plus the
//!   writer's buffers; the existing rolling diagnostic histories (the
//!   interactive overlay's inputs) are deliberately untouched.
//!
//! Streams (one JSONL file each under the evidence directory):
//! `frames` (per loop iteration), `ticks` (per authoritative tick),
//! `persistence` (application-boundary latencies), `regions` (per
//! transition identity), `memory` (process + structural samples), plus
//! `manifest.json` (counts, per-stream integrity digests, metadata, and
//! the completeness verdict). The schema is versioned
//! ([`EVIDENCE_SCHEMA_VERSION`]); decoding is strict — unknown or
//! missing fields are errors, never guesses.

use std::fs::File;
use std::io::{BufWriter, Write};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::Mutex;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::mpsc::{Receiver, RecvTimeoutError, SyncSender, TrySendError, sync_channel};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

use engine_core::LodLevel;
use engine_core::fingerprint::Fingerprinter;

/// The evidence schema's version tag — carried by every manifest and
/// compared on decode.
pub const EVIDENCE_SCHEMA_VERSION: &str = "s6-evidence/1";

/// Default bounded-channel capacity (records). Worst-case in-memory
/// retention is this capacity plus the writer's buffers — a few MB at
/// the schema's record sizes, fixed and independent of run length.
pub const DEFAULT_CHANNEL_CAPACITY: usize = 8192;

/// Default writer flush interval (records between buffered flushes;
/// a 200 ms receive timeout also flushes).
pub const DEFAULT_FLUSH_RECORDS: usize = 256;

const STREAM_COUNT: usize = 5;
const STREAM_NAMES: [&str; STREAM_COUNT] = ["frames", "ticks", "persistence", "regions", "memory"];

// ----------------------------------------------------------- JSON support

/// Minimal JSON support for the fixed evidence schema: a line writer
/// with escaping, and a strict parser for the flat record layout
/// (objects of scalars, `null`s, and arrays of numbers — the schema
/// has no nested objects).
mod json {
    /// One parsed JSON value (the schema's subset).
    #[derive(Debug, Clone, PartialEq)]
    pub enum Value {
        Uint(u64),
        Num(f64),
        Bool(bool),
        Str(String),
        Null,
        Arr(Vec<Value>),
    }

    /// Appends `text` to `out` as escaped JSON string content (no
    /// surrounding quotes).
    pub fn escape_into(out: &mut String, text: &str) {
        for character in text.chars() {
            match character {
                '"' => out.push_str("\\\""),
                '\\' => out.push_str("\\\\"),
                '\n' => out.push_str("\\n"),
                '\r' => out.push_str("\\r"),
                '\t' => out.push_str("\\t"),
                other if (other as u32) < 0x20 => {
                    out.push_str(&format!("\\u{:04x}", other as u32));
                }
                other => out.push(other),
            }
        }
    }

    /// The quoted, escaped JSON form of `text`.
    pub fn quoted(text: &str) -> String {
        let mut out = String::with_capacity(text.len() + 2);
        out.push('"');
        escape_into(&mut out, text);
        out.push('"');
        out
    }

    /// Parses one flat JSON object into its ordered fields (strict:
    /// duplicates and trailing content are errors).
    pub fn parse_object(line: &str) -> Result<Vec<(String, Value)>, String> {
        Parser::new(line).parse_entries()
    }

    struct Parser<'a> {
        bytes: &'a [u8],
        position: usize,
    }

    impl<'a> Parser<'a> {
        fn new(line: &'a str) -> Self {
            Parser {
                bytes: line.as_bytes(),
                position: 0,
            }
        }

        fn skip_whitespace(&mut self) {
            while matches!(
                self.bytes.get(self.position),
                Some(b' ' | b'\t' | b'\r' | b'\n')
            ) {
                self.position += 1;
            }
        }

        fn peek(&self) -> Option<u8> {
            self.bytes.get(self.position).copied()
        }

        fn expect(&mut self, byte: u8) -> Result<(), String> {
            match self.peek() {
                Some(actual) if actual == byte => {
                    self.position += 1;
                    Ok(())
                }
                other => Err(format!("expected {byte:?}, found {other:?}")),
            }
        }

        fn literal(&mut self, word: &str, length: usize) -> Result<(), String> {
            let end = self.position + length;
            if self.bytes.get(self.position..end) == Some(word.as_bytes()) {
                self.position = end;
                Ok(())
            } else {
                Err(format!("expected the literal {word:?}"))
            }
        }

        fn parse_value(&mut self) -> Result<Value, String> {
            self.skip_whitespace();
            match self.peek() {
                Some(b'{') => Err("nested objects are not part of the schema".into()),
                Some(b'"') => Ok(Value::Str(self.parse_string()?)),
                Some(b'[') => self.parse_array().map(Value::Arr),
                Some(b't') => {
                    self.literal("true", 4)?;
                    Ok(Value::Bool(true))
                }
                Some(b'f') => {
                    self.literal("false", 5)?;
                    Ok(Value::Bool(false))
                }
                Some(b'n') => {
                    self.literal("null", 4)?;
                    Ok(Value::Null)
                }
                Some(byte) if byte == b'-' || byte.is_ascii_digit() => self.parse_number(),
                other => Err(format!("unexpected character {other:?}")),
            }
        }

        fn parse_string(&mut self) -> Result<String, String> {
            self.expect(b'"')?;
            let mut out = String::new();
            loop {
                let byte = self
                    .bytes
                    .get(self.position)
                    .copied()
                    .ok_or("unterminated string")?;
                self.position += 1;
                match byte {
                    b'"' => return Ok(out),
                    b'\\' => {
                        let escape = self
                            .bytes
                            .get(self.position)
                            .copied()
                            .ok_or("unterminated escape")?;
                        self.position += 1;
                        match escape {
                            b'"' => out.push('"'),
                            b'\\' => out.push('\\'),
                            b'n' => out.push('\n'),
                            b'r' => out.push('\r'),
                            b't' => out.push('\t'),
                            b'u' => {
                                let hex = self
                                    .bytes
                                    .get(self.position..self.position + 4)
                                    .ok_or("truncated \\u escape")?;
                                self.position += 4;
                                let text =
                                    std::str::from_utf8(hex).map_err(|_| "invalid \\u escape")?;
                                let code = u32::from_str_radix(text, 16)
                                    .map_err(|_| "invalid \\u escape")?;
                                let character =
                                    char::from_u32(code).ok_or("invalid \\u code point")?;
                                out.push(character);
                            }
                            other => return Err(format!("unsupported escape {other:?}")),
                        }
                    }
                    byte if byte < 0x20 => return Err("raw control character in string".into()),
                    byte => {
                        // Re-assemble multi-byte UTF-8 from the raw bytes.
                        let width = utf8_width(byte);
                        let start = self.position - 1;
                        let end = start + width;
                        let slice = self
                            .bytes
                            .get(start..end)
                            .ok_or("truncated UTF-8 sequence")?;
                        let text = std::str::from_utf8(slice).map_err(|_| "invalid UTF-8")?;
                        out.push_str(text);
                        self.position = end;
                    }
                }
            }
        }

        fn parse_number(&mut self) -> Result<Value, String> {
            let start = self.position;
            while let Some(byte) = self.peek() {
                if byte.is_ascii_digit() || matches!(byte, b'-' | b'+' | b'.' | b'e' | b'E') {
                    self.position += 1;
                } else {
                    break;
                }
            }
            let text = std::str::from_utf8(&self.bytes[start..self.position])
                .map_err(|_| "invalid number")?;
            if text.is_empty() {
                return Err("empty number".into());
            }
            if matches!(text.as_bytes()[0], b'-' | b'+' | b'.' | b'e' | b'E') {
                return Err(format!("malformed number {text:?}"));
            }
            if !text.bytes().any(|byte| matches!(byte, b'.' | b'e' | b'E'))
                && let Ok(value) = text.parse::<u64>()
            {
                return Ok(Value::Uint(value));
            }
            text.parse::<f64>()
                .map(Value::Num)
                .map_err(|_| format!("malformed number {text:?}"))
        }

        fn parse_array(&mut self) -> Result<Vec<Value>, String> {
            self.expect(b'[')?;
            self.skip_whitespace();
            if self.peek() == Some(b']') {
                self.position += 1;
                return Ok(Vec::new());
            }
            let mut values = Vec::new();
            loop {
                values.push(self.parse_value()?);
                self.skip_whitespace();
                match self.peek() {
                    Some(b',') => {
                        self.position += 1;
                    }
                    Some(b']') => {
                        self.position += 1;
                        return Ok(values);
                    }
                    other => return Err(format!("expected ',' or ']', found {other:?}")),
                }
            }
        }

        fn parse_entries(&mut self) -> Result<Vec<(String, Value)>, String> {
            self.skip_whitespace();
            self.expect(b'{')?;
            let mut fields: Vec<(String, Value)> = Vec::new();
            self.skip_whitespace();
            if self.peek() == Some(b'}') {
                self.position += 1;
                self.skip_whitespace();
                if self.position != self.bytes.len() {
                    return Err("trailing content after the object".into());
                }
                return Ok(fields);
            }
            loop {
                let key = self.parse_string()?;
                self.skip_whitespace();
                self.expect(b':')?;
                let value = self.parse_value()?;
                if fields.iter().any(|(existing, _)| *existing == key) {
                    return Err(format!("duplicate field {key:?}"));
                }
                fields.push((key, value));
                self.skip_whitespace();
                match self.peek() {
                    Some(b',') => {
                        self.position += 1;
                        self.skip_whitespace();
                    }
                    Some(b'}') => {
                        self.position += 1;
                        self.skip_whitespace();
                        if self.position != self.bytes.len() {
                            return Err("trailing content after the object".into());
                        }
                        return Ok(fields);
                    }
                    other => return Err(format!("expected ',' or '}}', found {other:?}")),
                }
            }
        }
    }

    fn utf8_width(byte: u8) -> usize {
        match byte {
            byte if byte < 0x80 => 1,
            byte if byte & 0xE0 == 0xC0 => 2,
            byte if byte & 0xF0 == 0xE0 => 3,
            byte if byte & 0xF8 == 0xF0 => 4,
            _ => 1,
        }
    }
}

/// Quantizes a millisecond value to the schema's documented precision
/// (microsecond resolution — six decimal places) so encode/decode
/// round-trips are exact. Non-finite values pass through (encoded as
/// `null`, never as silent garbage).
fn ms6(value: f64) -> f64 {
    if !value.is_finite() {
        return value;
    }
    (value * 1e6).round() / 1e6
}

/// The one-line JSON writer (flat fields; `finish` prepends the type
/// tag). Every floating-point field passes through [`ms6`].
struct Line {
    out: String,
    first: bool,
}

impl Line {
    fn new() -> Self {
        Line {
            out: String::new(),
            first: true,
        }
    }

    fn key(&mut self, name: &str) {
        if self.first {
            self.first = false;
        } else {
            self.out.push(',');
        }
        self.out.push('"');
        json::escape_into(&mut self.out, name);
        self.out.push_str("\":");
    }

    fn u64(&mut self, name: &str, value: u64) {
        self.key(name);
        self.out.push_str(&value.to_string());
    }

    fn u32(&mut self, name: &str, value: u32) {
        self.u64(name, u64::from(value));
    }

    fn f64(&mut self, name: &str, value: f64) {
        self.key(name);
        let quantized = ms6(value);
        if quantized.is_finite() {
            self.out.push_str(&format!("{quantized:.6}"));
        } else {
            self.out.push_str("null");
        }
    }

    fn boolean(&mut self, name: &str, value: bool) {
        self.key(name);
        self.out.push_str(if value { "true" } else { "false" });
    }

    fn text(&mut self, name: &str, value: &str) {
        self.key(name);
        self.out.push_str(&json::quoted(value));
    }

    fn opt_f64(&mut self, name: &str, value: Option<f64>) {
        match value {
            Some(value) => self.f64(name, value),
            None => {
                self.key(name);
                self.out.push_str("null");
            }
        }
    }

    fn opt_u64(&mut self, name: &str, value: Option<u64>) {
        match value {
            Some(value) => self.u64(name, value),
            None => {
                self.key(name);
                self.out.push_str("null");
            }
        }
    }

    fn u64_list(&mut self, name: &str, values: &[u64]) {
        self.key(name);
        self.out.push('[');
        for (index, value) in values.iter().enumerate() {
            if index > 0 {
                self.out.push(',');
            }
            self.out.push_str(&value.to_string());
        }
        self.out.push(']');
    }

    fn finish(self, tag: &str) -> String {
        let mut out = String::with_capacity(self.out.len() + 32);
        out.push_str("{\"type\":\"");
        out.push_str(tag);
        out.push_str("\",");
        out.push_str(&self.out);
        out.push('}');
        out
    }
}

// ------------------------------------------------------------ the samples

/// The schema's LOD detail ordinal: 3 Full · 2 Simplified ·
/// 1 EventDriven · 0 Dormant (higher is more detailed).
pub fn lod_ordinal(level: LodLevel) -> u8 {
    match level {
        LodLevel::Full => 3,
        LodLevel::Simplified => 2,
        LodLevel::EventDriven => 1,
        LodLevel::Dormant => 0,
    }
}

/// One graphical loop iteration. Sampled every iteration (presented or
/// not); `presented` marks the presented subset. All `_ms` fields are
/// milliseconds; `gpu_ms` is `null` when the device exposes no usable
/// timestamp queries — never a substituted zero.
#[derive(Debug, Clone, PartialEq)]
pub struct FrameSample {
    /// The loop-iteration index (every iteration samples).
    pub sample: u64,
    /// Wall-clock milliseconds since the collector started.
    pub wall_offset_ms: f64,
    /// The raw interval handed to the sim accumulator (unclamped).
    pub interval_ms: f64,
    pub presented: bool,
    pub recreated: bool,
    pub deferred: bool,
    /// poll→present CPU work excluding pacing sleep (presented frames
    /// only; `null` otherwise).
    pub cpu_work_ms: Option<f64>,
    pub input_ms: Option<f64>,
    pub sim_ms: Option<f64>,
    pub render_ms: Option<f64>,
    pub poll_ms: Option<f64>,
    pub gpu_ms: Option<f64>,
}

/// One authoritative tick's observational snapshot. Counters are
/// cumulative (spine `Stats`/lane totals read after the tick);
/// `outstanding_ticks` is the fixed-step time debt remaining after
/// this tick within the not-yet-settled accumulator (session-local —
/// recovery does not restore it; never conflated with event-spine
/// obligations).
#[derive(Debug, Clone, PartialEq)]
pub struct TickSample {
    pub tick: u64,
    pub wall_offset_ms: f64,
    /// The orchestrated boundary duration (gate + transitions + spine),
    /// the established label — never bare spine time.
    pub tick_ms: f64,
    /// The ABSOLUTE campaign ticks due (the recovered starting clock
    /// included — the conservation input).
    pub ticks_due_total: u64,
    pub outstanding_ticks: u32,
    pub executed: u64,
    pub scheduled: u64,
    pub late_arrivals: u64,
    pub late_executions: u64,
    pub blocked_transitions: u64,
    pub invalid_dropped: u64,
    pub budget_exhausted_ticks: u64,
    pub near_pending: u32,
    pub far_pending: u32,
    pub blocked: u32,
    pub held: u32,
    pub near_executed_total: u64,
    pub far_executed_total: u64,
    pub near_backlogged_ticks: u64,
    pub far_backlogged_ticks: u64,
    pub processed_through: Option<u64>,
    pub max_executed_due: Option<u64>,
    /// The regions that transitioned at this tick (empty most ticks).
    pub transitioned_regions: Vec<u64>,
}

/// The application-boundary persistence operation kinds (each latency
/// family is reported separately — commit acknowledgment, publication
/// acceptance, close, recovery — never conflated).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PersistenceKind {
    Recovery,
    Capture,
    Admission,
    Busy,
    Completion,
    Confirmation,
    Close,
}

impl PersistenceKind {
    fn tag(self) -> &'static str {
        match self {
            PersistenceKind::Recovery => "recovery",
            PersistenceKind::Capture => "capture",
            PersistenceKind::Admission => "admission",
            PersistenceKind::Busy => "busy",
            PersistenceKind::Completion => "completion",
            PersistenceKind::Confirmation => "confirmation",
            PersistenceKind::Close => "close",
        }
    }
}

/// One persistence-boundary observation (milliseconds; `null` latency
/// only where the boundary produced no duration).
#[derive(Debug, Clone, PartialEq)]
pub struct PersistenceSample {
    pub kind: PersistenceKind,
    pub wall_offset_ms: f64,
    pub tick: u64,
    pub latency_ms: Option<f64>,
    /// The truthful outcome label (accepted / busy / confirmed … /
    /// refused — verbatim, never summarized into success).
    pub outcome: String,
    pub revision: Option<String>,
}

/// One region transition identity (from the tick's `TickReport` and
/// the director's before/after levels).
#[derive(Debug, Clone, PartialEq)]
pub struct RegionSample {
    pub tick: u64,
    pub wall_offset_ms: f64,
    pub region: u64,
    /// LOD detail ordinals ([`lod_ordinal`]).
    pub from_level: u8,
    pub to_level: u8,
}

/// One process/structural memory sample (the graphical loop's fixed
/// wall-clock cadence). `rss_kib` is `null` on platforms without the
/// sampler (Windows physical runs use the external sampler — S6-C4).
#[derive(Debug, Clone, PartialEq)]
pub struct MemorySample {
    pub wall_offset_ms: f64,
    pub rss_kib: Option<u64>,
    /// Held (dormant-parked) obligations per region — the structural
    /// dormant occupancy, an attribution estimate, never an
    /// allocation-exact per-NPC cost.
    pub held_by_region: Vec<u64>,
    /// The capture ledger's pending consequence total (structural).
    pub pending_total: u64,
}

/// One evidence record (the channel's unit; the stream is derived).
#[derive(Debug, Clone, PartialEq)]
pub enum EvidenceRecord {
    Frame(FrameSample),
    Tick(TickSample),
    Persistence(PersistenceSample),
    Region(RegionSample),
    Memory(MemorySample),
}

impl EvidenceRecord {
    /// The stream file this record belongs to.
    pub fn stream(&self) -> &'static str {
        STREAM_NAMES[self.stream_index()]
    }

    fn stream_index(&self) -> usize {
        match self {
            EvidenceRecord::Frame(_) => 0,
            EvidenceRecord::Tick(_) => 1,
            EvidenceRecord::Persistence(_) => 2,
            EvidenceRecord::Region(_) => 3,
            EvidenceRecord::Memory(_) => 4,
        }
    }

    /// The JSONL line (microsecond-resolution milliseconds; `null` for
    /// unsupported/absent values).
    pub fn encode(&self) -> String {
        match self {
            EvidenceRecord::Frame(sample) => {
                let mut line = Line::new();
                line.u64("sample", sample.sample);
                line.f64("wall_offset_ms", sample.wall_offset_ms);
                line.f64("interval_ms", sample.interval_ms);
                line.boolean("presented", sample.presented);
                line.boolean("recreated", sample.recreated);
                line.boolean("deferred", sample.deferred);
                line.opt_f64("cpu_work_ms", sample.cpu_work_ms);
                line.opt_f64("input_ms", sample.input_ms);
                line.opt_f64("sim_ms", sample.sim_ms);
                line.opt_f64("render_ms", sample.render_ms);
                line.opt_f64("poll_ms", sample.poll_ms);
                line.opt_f64("gpu_ms", sample.gpu_ms);
                line.finish("frame")
            }
            EvidenceRecord::Tick(sample) => {
                let mut line = Line::new();
                line.u64("tick", sample.tick);
                line.f64("wall_offset_ms", sample.wall_offset_ms);
                line.f64("tick_ms", sample.tick_ms);
                line.u64("ticks_due_total", sample.ticks_due_total);
                line.u32("outstanding_ticks", sample.outstanding_ticks);
                line.u64("executed", sample.executed);
                line.u64("scheduled", sample.scheduled);
                line.u64("late_arrivals", sample.late_arrivals);
                line.u64("late_executions", sample.late_executions);
                line.u64("blocked_transitions", sample.blocked_transitions);
                line.u64("invalid_dropped", sample.invalid_dropped);
                line.u64("budget_exhausted_ticks", sample.budget_exhausted_ticks);
                line.u32("near_pending", sample.near_pending);
                line.u32("far_pending", sample.far_pending);
                line.u32("blocked", sample.blocked);
                line.u32("held", sample.held);
                line.u64("near_executed_total", sample.near_executed_total);
                line.u64("far_executed_total", sample.far_executed_total);
                line.u64("near_backlogged_ticks", sample.near_backlogged_ticks);
                line.u64("far_backlogged_ticks", sample.far_backlogged_ticks);
                line.opt_u64("processed_through", sample.processed_through);
                line.opt_u64("max_executed_due", sample.max_executed_due);
                line.u64_list("transitioned_regions", &sample.transitioned_regions);
                line.finish("tick")
            }
            EvidenceRecord::Persistence(sample) => {
                let mut line = Line::new();
                line.text("kind", sample.kind.tag());
                line.f64("wall_offset_ms", sample.wall_offset_ms);
                line.u64("tick", sample.tick);
                line.opt_f64("latency_ms", sample.latency_ms);
                line.text("outcome", &sample.outcome);
                match &sample.revision {
                    Some(revision) => line.text("revision", revision),
                    None => {
                        line.key("revision");
                        line.out.push_str("null");
                    }
                }
                line.finish("persistence")
            }
            EvidenceRecord::Region(sample) => {
                let mut line = Line::new();
                line.u64("tick", sample.tick);
                line.f64("wall_offset_ms", sample.wall_offset_ms);
                line.u64("region", sample.region);
                line.u32("from_level", u32::from(sample.from_level));
                line.u32("to_level", u32::from(sample.to_level));
                line.finish("region")
            }
            EvidenceRecord::Memory(sample) => {
                let mut line = Line::new();
                line.f64("wall_offset_ms", sample.wall_offset_ms);
                line.opt_u64("rss_kib", sample.rss_kib);
                line.u64_list("held_by_region", &sample.held_by_region);
                line.u64("pending_total", sample.pending_total);
                line.finish("memory")
            }
        }
    }
}

/// A collected evidence failure (open errors, strict decode errors).
#[derive(Debug)]
pub enum EvidenceError {
    Open {
        path: PathBuf,
        error: std::io::Error,
    },
    Decode(String),
}

impl std::fmt::Display for EvidenceError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            EvidenceError::Open { path, error } => {
                write!(formatter, "open {}: {error}", path.display())
            }
            EvidenceError::Decode(detail) => write!(formatter, "decode: {detail}"),
        }
    }
}

impl std::error::Error for EvidenceError {}

fn decode_error(detail: String) -> EvidenceError {
    EvidenceError::Decode(detail)
}

fn take(fields: &mut Vec<(String, json::Value)>, name: &str) -> Result<json::Value, EvidenceError> {
    let position = fields
        .iter()
        .position(|(key, _)| key == name)
        .ok_or_else(|| decode_error(format!("missing field {name}")))?;
    Ok(fields.remove(position).1)
}

fn finish_fields(fields: Vec<(String, json::Value)>) -> Result<(), EvidenceError> {
    match fields.first() {
        None => Ok(()),
        Some((key, _)) => Err(decode_error(format!("unexpected field {key}"))),
    }
}

fn as_str(value: json::Value) -> Result<String, EvidenceError> {
    match value {
        json::Value::Str(text) => Ok(text),
        other => Err(decode_error(format!("expected a string, found {other:?}"))),
    }
}

fn as_bool(value: json::Value) -> Result<bool, EvidenceError> {
    match value {
        json::Value::Bool(flag) => Ok(flag),
        other => Err(decode_error(format!("expected a boolean, found {other:?}"))),
    }
}

fn as_u64(value: json::Value) -> Result<u64, EvidenceError> {
    match value {
        json::Value::Uint(value) => Ok(value),
        json::Value::Num(value) if value.fract() == 0.0 && value >= 0.0 => Ok(value as u64),
        other => Err(decode_error(format!(
            "expected an integer, found {other:?}"
        ))),
    }
}

fn as_u32(value: json::Value) -> Result<u32, EvidenceError> {
    let value = as_u64(value)?;
    u32::try_from(value).map_err(|_| decode_error(format!("u32 out of range: {value}")))
}

fn as_f64(value: json::Value) -> Result<f64, EvidenceError> {
    match value {
        json::Value::Num(value) => Ok(value),
        json::Value::Uint(value) => Ok(value as f64),
        other => Err(decode_error(format!("expected a number, found {other:?}"))),
    }
}

fn as_opt_f64(value: json::Value) -> Result<Option<f64>, EvidenceError> {
    match value {
        json::Value::Null => Ok(None),
        other => Ok(Some(as_f64(other)?)),
    }
}

fn as_opt_u64(value: json::Value) -> Result<Option<u64>, EvidenceError> {
    match value {
        json::Value::Null => Ok(None),
        other => Ok(Some(as_u64(other)?)),
    }
}

fn as_u64_list(value: json::Value) -> Result<Vec<u64>, EvidenceError> {
    match value {
        json::Value::Arr(values) => values.into_iter().map(as_u64).collect(),
        other => Err(decode_error(format!("expected an array, found {other:?}"))),
    }
}

fn as_kind(value: json::Value) -> Result<PersistenceKind, EvidenceError> {
    let tag = as_str(value)?;
    match tag.as_str() {
        "recovery" => Ok(PersistenceKind::Recovery),
        "capture" => Ok(PersistenceKind::Capture),
        "admission" => Ok(PersistenceKind::Admission),
        "busy" => Ok(PersistenceKind::Busy),
        "completion" => Ok(PersistenceKind::Completion),
        "confirmation" => Ok(PersistenceKind::Confirmation),
        "close" => Ok(PersistenceKind::Close),
        other => Err(decode_error(format!("unknown persistence kind {other:?}"))),
    }
}

fn as_opt_revision(value: json::Value) -> Result<Option<String>, EvidenceError> {
    match value {
        json::Value::Null => Ok(None),
        other => Ok(Some(as_str(other)?)),
    }
}

/// Strictly decodes one evidence line (the type tag dispatches; the
/// field set must match the schema exactly — unknown or missing
/// fields are errors).
pub fn decode_line(line: &str) -> Result<EvidenceRecord, EvidenceError> {
    let mut fields = json::parse_object(line).map_err(decode_error)?;
    let tag = as_str(take(&mut fields, "type")?)?;
    let record = match tag.as_str() {
        "frame" => {
            let mut sample = FrameSample {
                sample: as_u64(take(&mut fields, "sample")?)?,
                wall_offset_ms: as_f64(take(&mut fields, "wall_offset_ms")?)?,
                interval_ms: as_f64(take(&mut fields, "interval_ms")?)?,
                presented: as_bool(take(&mut fields, "presented")?)?,
                recreated: as_bool(take(&mut fields, "recreated")?)?,
                deferred: as_bool(take(&mut fields, "deferred")?)?,
                cpu_work_ms: as_opt_f64(take(&mut fields, "cpu_work_ms")?)?,
                input_ms: as_opt_f64(take(&mut fields, "input_ms")?)?,
                sim_ms: as_opt_f64(take(&mut fields, "sim_ms")?)?,
                render_ms: as_opt_f64(take(&mut fields, "render_ms")?)?,
                poll_ms: as_opt_f64(take(&mut fields, "poll_ms")?)?,
                gpu_ms: as_opt_f64(take(&mut fields, "gpu_ms")?)?,
            };
            sample.wall_offset_ms = ms6(sample.wall_offset_ms);
            sample.interval_ms = ms6(sample.interval_ms);
            EvidenceRecord::Frame(sample)
        }
        "tick" => {
            let mut sample = TickSample {
                tick: as_u64(take(&mut fields, "tick")?)?,
                wall_offset_ms: as_f64(take(&mut fields, "wall_offset_ms")?)?,
                tick_ms: as_f64(take(&mut fields, "tick_ms")?)?,
                ticks_due_total: as_u64(take(&mut fields, "ticks_due_total")?)?,
                outstanding_ticks: as_u32(take(&mut fields, "outstanding_ticks")?)?,
                executed: as_u64(take(&mut fields, "executed")?)?,
                scheduled: as_u64(take(&mut fields, "scheduled")?)?,
                late_arrivals: as_u64(take(&mut fields, "late_arrivals")?)?,
                late_executions: as_u64(take(&mut fields, "late_executions")?)?,
                blocked_transitions: as_u64(take(&mut fields, "blocked_transitions")?)?,
                invalid_dropped: as_u64(take(&mut fields, "invalid_dropped")?)?,
                budget_exhausted_ticks: as_u64(take(&mut fields, "budget_exhausted_ticks")?)?,
                near_pending: as_u32(take(&mut fields, "near_pending")?)?,
                far_pending: as_u32(take(&mut fields, "far_pending")?)?,
                blocked: as_u32(take(&mut fields, "blocked")?)?,
                held: as_u32(take(&mut fields, "held")?)?,
                near_executed_total: as_u64(take(&mut fields, "near_executed_total")?)?,
                far_executed_total: as_u64(take(&mut fields, "far_executed_total")?)?,
                near_backlogged_ticks: as_u64(take(&mut fields, "near_backlogged_ticks")?)?,
                far_backlogged_ticks: as_u64(take(&mut fields, "far_backlogged_ticks")?)?,
                processed_through: as_opt_u64(take(&mut fields, "processed_through")?)?,
                max_executed_due: as_opt_u64(take(&mut fields, "max_executed_due")?)?,
                transitioned_regions: as_u64_list(take(&mut fields, "transitioned_regions")?)?,
            };
            sample.wall_offset_ms = ms6(sample.wall_offset_ms);
            sample.tick_ms = ms6(sample.tick_ms);
            EvidenceRecord::Tick(sample)
        }
        "persistence" => {
            let mut sample = PersistenceSample {
                kind: as_kind(take(&mut fields, "kind")?)?,
                wall_offset_ms: as_f64(take(&mut fields, "wall_offset_ms")?)?,
                tick: as_u64(take(&mut fields, "tick")?)?,
                latency_ms: as_opt_f64(take(&mut fields, "latency_ms")?)?,
                outcome: as_str(take(&mut fields, "outcome")?)?,
                revision: as_opt_revision(take(&mut fields, "revision")?)?,
            };
            sample.wall_offset_ms = ms6(sample.wall_offset_ms);
            if let Some(latency) = &mut sample.latency_ms {
                *latency = ms6(*latency);
            }
            EvidenceRecord::Persistence(sample)
        }
        "region" => {
            let mut sample = RegionSample {
                tick: as_u64(take(&mut fields, "tick")?)?,
                wall_offset_ms: as_f64(take(&mut fields, "wall_offset_ms")?)?,
                region: as_u64(take(&mut fields, "region")?)?,
                from_level: as_u32(take(&mut fields, "from_level")?)? as u8,
                to_level: as_u32(take(&mut fields, "to_level")?)? as u8,
            };
            sample.wall_offset_ms = ms6(sample.wall_offset_ms);
            EvidenceRecord::Region(sample)
        }
        "memory" => {
            let mut sample = MemorySample {
                wall_offset_ms: as_f64(take(&mut fields, "wall_offset_ms")?)?,
                rss_kib: as_opt_u64(take(&mut fields, "rss_kib")?)?,
                held_by_region: as_u64_list(take(&mut fields, "held_by_region")?)?,
                pending_total: as_u64(take(&mut fields, "pending_total")?)?,
            };
            sample.wall_offset_ms = ms6(sample.wall_offset_ms);
            EvidenceRecord::Memory(sample)
        }
        other => return Err(decode_error(format!("unknown record type {other:?}"))),
    };
    finish_fields(fields)?;
    Ok(record)
}

// ------------------------------------------------------------- statistics

/// The 1%-low frame rate (the documented calculation): the
/// `ceil(0.01 × n)` worst (longest) presented-frame intervals, their
/// mean rate — count ÷ summed seconds. `None` below 100 samples (the
/// metric is not claimed on short runs).
pub fn one_percent_low_fps(intervals_ms: &[f64]) -> Option<f64> {
    let count = intervals_ms.len();
    if count < 100 {
        return None;
    }
    let mut sorted = intervals_ms.to_vec();
    sorted.sort_by(|a, b| a.total_cmp(b));
    let worst = ((count as f64) * 0.01).ceil() as usize;
    let sum_ms: f64 = sorted[count - worst..].iter().sum();
    if sum_ms <= 0.0 {
        return None;
    }
    Some(worst as f64 / (sum_ms / 1000.0))
}

// -------------------------------------------------------------- the faults

#[derive(Debug, Default)]
struct FaultInner {
    fail_writes: AtomicBool,
    park_writes: AtomicBool,
    park_entered: AtomicBool,
}

/// Deterministic fault injection for the evidence writer (the
/// `WorkerFaultControl` discipline — observable state, never sleeps):
/// `fail_writes` makes every subsequent record fail at the write
/// boundary; `park_writes` parks the writer between records (with
/// `park_entered` observable) so a full bounded queue can be produced
/// deterministically. Engine code never sets these.
#[derive(Debug, Clone)]
pub struct EvidenceFaultControl {
    inner: Arc<FaultInner>,
}

impl EvidenceFaultControl {
    pub fn set_fail_writes(&self, on: bool) {
        self.inner.fail_writes.store(on, Ordering::SeqCst);
    }

    /// Parks the writer between records (releases with
    /// [`Self::release_park`]; the collector's finalize always
    /// releases, so a parked writer can never hang a finalization).
    pub fn park_writes(&self) {
        self.inner.park_writes.store(true, Ordering::SeqCst);
    }

    pub fn release_park(&self) {
        self.inner.park_writes.store(false, Ordering::SeqCst);
    }

    pub fn park_entered(&self) -> bool {
        self.inner.park_entered.load(Ordering::SeqCst)
    }
}

// ------------------------------------------------------------ the counters

#[derive(Debug)]
struct Counters {
    produced: [AtomicU64; STREAM_COUNT],
    lost: [AtomicU64; STREAM_COUNT],
    written: [AtomicU64; STREAM_COUNT],
    write_failed: [AtomicU64; STREAM_COUNT],
}

impl Counters {
    fn new() -> Self {
        Counters {
            produced: std::array::from_fn(|_| AtomicU64::new(0)),
            lost: std::array::from_fn(|_| AtomicU64::new(0)),
            written: std::array::from_fn(|_| AtomicU64::new(0)),
            write_failed: std::array::from_fn(|_| AtomicU64::new(0)),
        }
    }

    fn summary_rows(&self) -> Vec<StreamSummary> {
        (0..STREAM_COUNT)
            .map(|stream| StreamSummary {
                name: STREAM_NAMES[stream],
                produced: self.produced[stream].load(Ordering::Relaxed),
                written: self.written[stream].load(Ordering::Relaxed),
                lost: self.lost[stream].load(Ordering::Relaxed),
                write_failed: self.write_failed[stream].load(Ordering::Relaxed),
            })
            .collect()
    }
}

fn note_failure(slot: &Mutex<Option<String>>, message: String) {
    let mut guard = slot.lock().unwrap_or_else(|poisoned| poisoned.into_inner());
    if guard.is_none() {
        *guard = Some(message);
    }
}

// --------------------------------------------------------------- the writer

struct WriterInputs {
    receiver: Receiver<EvidenceRecord>,
    files: Vec<BufWriter<File>>,
    counters: Arc<Counters>,
    fault: Arc<FaultInner>,
    failure: Arc<Mutex<Option<String>>>,
    metadata: Arc<Mutex<Vec<(String, String)>>>,
    scenario: String,
    dir: PathBuf,
    flush_records: usize,
}

fn writer_main(inputs: WriterInputs) {
    let WriterInputs {
        receiver,
        mut files,
        counters,
        fault,
        failure,
        metadata,
        scenario,
        dir,
        flush_records,
    } = inputs;
    let mut fingerprints: Vec<Fingerprinter> =
        (0..STREAM_COUNT).map(|_| Fingerprinter::new()).collect();
    let mut bytes = [0u64; STREAM_COUNT];
    let mut buffered = 0usize;
    let mut broken = false;
    let flush_all = |files: &mut [BufWriter<File>], buffered: &mut usize| {
        for file in files {
            let _ = file.flush();
        }
        *buffered = 0;
    };
    'writer: loop {
        if fault.park_writes.load(Ordering::SeqCst) {
            fault.park_entered.store(true, Ordering::SeqCst);
            while fault.park_writes.load(Ordering::SeqCst) {
                std::hint::spin_loop();
            }
            fault.park_entered.store(false, Ordering::SeqCst);
        }
        match receiver.recv_timeout(Duration::from_millis(200)) {
            Ok(record) => {
                let stream = record.stream_index();
                if broken {
                    // Counted loss at the write boundary — visible,
                    // never silent, never blocking the producers.
                    counters.write_failed[stream].fetch_add(1, Ordering::Relaxed);
                    continue;
                }
                if fault.fail_writes.load(Ordering::SeqCst) {
                    broken = true;
                    note_failure(
                        &failure,
                        "an injected evidence write failure (the test seam)".into(),
                    );
                    counters.write_failed[stream].fetch_add(1, Ordering::Relaxed);
                    continue;
                }
                let line = record.encode();
                let write = files[stream]
                    .write_all(line.as_bytes())
                    .and_then(|()| files[stream].write_all(b"\n"));
                match write {
                    Ok(()) => {
                        fingerprints[stream].write_bytes(line.as_bytes());
                        fingerprints[stream].write_bytes(b"\n");
                        bytes[stream] += line.len() as u64 + 1;
                        counters.written[stream].fetch_add(1, Ordering::Relaxed);
                        buffered += 1;
                        if buffered >= flush_records {
                            flush_all(&mut files, &mut buffered);
                        }
                    }
                    Err(error) => {
                        broken = true;
                        note_failure(&failure, format!("the evidence write failed: {error}"));
                        counters.write_failed[stream].fetch_add(1, Ordering::Relaxed);
                    }
                }
            }
            Err(RecvTimeoutError::Timeout) => {
                if buffered > 0 {
                    flush_all(&mut files, &mut buffered);
                }
            }
            Err(RecvTimeoutError::Disconnected) => break 'writer,
        }
    }
    for file in &mut files {
        if let Err(error) = file.flush() {
            note_failure(
                &failure,
                format!("the final evidence flush failed: {error}"),
            );
        }
    }
    // The manifest (best-effort; a manifest failure is noted and the
    // in-memory outcome remains the authoritative verdict).
    let rows = counters.summary_rows();
    let digests: Vec<String> = (0..STREAM_COUNT)
        .map(|stream| fingerprints[stream].finish_hex())
        .collect();
    let failure_snapshot = failure
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
        .clone();
    let complete = failure_snapshot.is_none()
        && rows
            .iter()
            .all(|row| row.lost == 0 && row.write_failed == 0 && row.produced == row.written);
    let mut manifest = String::new();
    manifest.push_str("{\"schema\":\"");
    manifest.push_str(EVIDENCE_SCHEMA_VERSION);
    manifest.push_str("\",\"scenario\":");
    manifest.push_str(&json::quoted(&scenario));
    manifest.push_str(",\"metadata\":{");
    {
        let metadata = metadata
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        for (index, (key, value)) in metadata.iter().enumerate() {
            if index > 0 {
                manifest.push(',');
            }
            manifest.push_str(&json::quoted(key));
            manifest.push(':');
            manifest.push_str(&json::quoted(value));
        }
    }
    manifest.push_str("},\"streams\":[");
    for stream in 0..STREAM_COUNT {
        if stream > 0 {
            manifest.push(',');
        }
        let row = &rows[stream];
        manifest.push_str(&format!(
            "{{\"name\":\"{}\",\"produced\":{},\"written\":{},\"lost\":{},\"write_failed\":{},\"bytes\":{},\"fnv\":\"{}\"}}",
            row.name, row.produced, row.written, row.lost, row.write_failed,
            bytes[stream], digests[stream]
        ));
    }
    manifest.push_str("],\"failure\":");
    match &failure_snapshot {
        Some(failure) => manifest.push_str(&json::quoted(failure)),
        None => manifest.push_str("null"),
    }
    manifest.push_str(&format!(",\"complete\":{complete}}}"));
    if let Err(error) = std::fs::write(dir.join("manifest.json"), &manifest) {
        note_failure(&failure, format!("the manifest write failed: {error}"));
    }
}

// ------------------------------------------------------------ the collector

/// The collector's configuration (opt-in; the demonstration runs with
/// `None` by default — zero cost, zero behavior change).
#[derive(Debug, Clone)]
pub struct EvidenceConfig {
    /// The evidence directory (created if absent; outside the tracked
    /// source tree).
    pub dir: PathBuf,
    /// The scenario label carried into the manifest.
    pub scenario: String,
    /// Bounded channel capacity (records).
    pub channel_capacity: usize,
    /// Writer flush interval (records; a 200 ms timeout also flushes).
    pub flush_records: usize,
}

impl EvidenceConfig {
    pub fn new(dir: impl Into<PathBuf>, scenario: impl Into<String>) -> Self {
        EvidenceConfig {
            dir: dir.into(),
            scenario: scenario.into(),
            channel_capacity: DEFAULT_CHANNEL_CAPACITY,
            flush_records: DEFAULT_FLUSH_RECORDS,
        }
    }
}

/// One stream's final counts.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StreamSummary {
    pub name: &'static str,
    pub produced: u64,
    pub written: u64,
    pub lost: u64,
    pub write_failed: u64,
}

/// The collector's final verdict: per-stream counts, the first
/// failure note (queue overflow or writer failure), and the
/// completeness flag. `complete` iff nothing was lost, no write
/// failed, and every produced sample was written — an incomplete
/// package is a failed evidence verdict, reported independently of
/// the run's logical outcome.
#[derive(Debug, Clone)]
pub struct EvidenceOutcome {
    pub schema_version: &'static str,
    pub dir: PathBuf,
    pub scenario: String,
    pub streams: Vec<StreamSummary>,
    pub failure: Option<String>,
    pub complete: bool,
}

impl EvidenceOutcome {
    pub fn is_complete(&self) -> bool {
        self.complete
    }

    pub fn total_samples(&self) -> u64 {
        self.streams.iter().map(|row| row.produced).sum()
    }

    /// The report print (the demo's `--- evidence ---` block).
    pub fn summary(&self) -> String {
        let mut text = format!(
            "schema {} · scenario {} · verdict {}",
            self.schema_version,
            self.scenario,
            if self.complete {
                "COMPLETE"
            } else {
                "INCOMPLETE"
            }
        );
        for row in &self.streams {
            text.push_str(&format!(
                "\n  {}: produced {} · written {} · lost {} · write-failed {}",
                row.name, row.produced, row.written, row.lost, row.write_failed
            ));
        }
        if let Some(failure) = &self.failure {
            text.push_str(&format!("\n  failure: {failure}"));
        }
        text
    }
}

/// The opt-in evidence collector: a bounded non-blocking producer
/// surface plus a dedicated writer thread. Created before the campaign
/// opens (so recovery is timed) and finalized exactly once through the
/// demonstration's single finalization site — after the graceful
/// close, so a writer failure can never bypass it.
pub struct EvidenceCollector {
    sender: Option<SyncSender<EvidenceRecord>>,
    counters: Arc<Counters>,
    fault: Arc<FaultInner>,
    failure: Arc<Mutex<Option<String>>>,
    metadata: Arc<Mutex<Vec<(String, String)>>>,
    handle: Option<JoinHandle<()>>,
    start: Instant,
    scenario: String,
    dir: PathBuf,
}

impl EvidenceCollector {
    /// Creates the evidence directory, opens the stream files (fail
    /// fast, typed — before any session exists), and spawns the
    /// writer thread.
    pub fn start(config: EvidenceConfig) -> Result<Self, EvidenceError> {
        std::fs::create_dir_all(&config.dir).map_err(|error| EvidenceError::Open {
            path: config.dir.clone(),
            error,
        })?;
        let mut files = Vec::with_capacity(STREAM_COUNT);
        for name in STREAM_NAMES {
            let path = config.dir.join(format!("{name}.jsonl"));
            let file = File::create(&path).map_err(|error| EvidenceError::Open { path, error })?;
            files.push(BufWriter::new(file));
        }
        let (sender, receiver) = sync_channel(config.channel_capacity.max(1));
        let counters = Arc::new(Counters::new());
        let fault = Arc::new(FaultInner::default());
        let failure = Arc::new(Mutex::new(None));
        let metadata = Arc::new(Mutex::new(Vec::new()));
        let handle = std::thread::Builder::new()
            .name("s6-evidence-writer".into())
            .spawn({
                let inputs = WriterInputs {
                    receiver,
                    files,
                    counters: Arc::clone(&counters),
                    fault: Arc::clone(&fault),
                    failure: Arc::clone(&failure),
                    metadata: Arc::clone(&metadata),
                    scenario: config.scenario.clone(),
                    dir: config.dir.clone(),
                    flush_records: config.flush_records.max(1),
                };
                move || writer_main(inputs)
            })
            .map_err(|error| EvidenceError::Open {
                path: config.dir.clone(),
                error: std::io::Error::other(error),
            })?;
        Ok(EvidenceCollector {
            sender: Some(sender),
            counters,
            fault,
            failure,
            metadata,
            handle: Some(handle),
            start: Instant::now(),
            scenario: config.scenario,
            dir: config.dir,
        })
    }

    /// Submits one sample — never blocking, never on the logical
    /// path. A full bounded queue or a gone writer counts the loss
    /// (the verdict fails); the run continues.
    pub fn produce(&self, record: EvidenceRecord) {
        let stream = record.stream_index();
        self.counters.produced[stream].fetch_add(1, Ordering::Relaxed);
        let Some(sender) = self.sender.as_ref() else {
            return;
        };
        if let Err(send_error) = sender.try_send(record) {
            self.counters.lost[stream].fetch_add(1, Ordering::Relaxed);
            let message = match send_error {
                TrySendError::Full(_) => "the evidence queue filled — samples were lost",
                TrySendError::Disconnected(_) => "the evidence writer is gone — samples were lost",
            };
            note_failure(&self.failure, message.to_string());
        }
    }

    /// Wall-clock milliseconds since the collector started.
    pub fn wall_offset_ms(&self) -> f64 {
        self.start.elapsed().as_secs_f64() * 1000.0
    }

    pub fn fault_control(&self) -> EvidenceFaultControl {
        EvidenceFaultControl {
            inner: Arc::clone(&self.fault),
        }
    }

    /// Records one metadata pair into the manifest (environment and
    /// presentation facts the loops observe).
    pub fn note_metadata(&self, key: &str, value: &str) {
        self.metadata
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .push((key.to_string(), value.to_string()));
    }

    pub fn scenario(&self) -> &str {
        &self.scenario
    }

    pub fn dir(&self) -> &Path {
        &self.dir
    }

    /// Finalizes exactly once: releases any park, drops the sender
    /// (the writer drains every buffered sample, flushes, and writes
    /// the manifest), joins the writer, and returns the verdict.
    pub fn finalize(mut self) -> EvidenceOutcome {
        self.fault_control().release_park();
        self.sender = None;
        let join = self.handle.take().map(|handle| handle.join());
        if let Some(Err(panic)) = join {
            note_failure(
                &self.failure,
                format!("the evidence writer panicked: {panic:?}"),
            );
        }
        let rows = self.counters.summary_rows();
        let failure = self
            .failure
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .clone();
        let complete = failure.is_none()
            && rows
                .iter()
                .all(|row| row.lost == 0 && row.write_failed == 0 && row.produced == row.written);
        EvidenceOutcome {
            schema_version: EVIDENCE_SCHEMA_VERSION,
            dir: self.dir,
            scenario: self.scenario,
            streams: rows,
            failure,
            complete,
        }
    }
}

// ------------------------------------------------------------------ tests

#[cfg(test)]
mod tests {
    use super::*;

    fn evidence_temp_dir(tag: &str) -> PathBuf {
        let path = std::env::temp_dir().join(format!(
            "s6-evidence-{tag}-{}",
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        std::fs::create_dir_all(&path).unwrap();
        path
    }

    fn frame_sample() -> FrameSample {
        FrameSample {
            sample: 42,
            wall_offset_ms: 1234.5,
            interval_ms: 16.667,
            presented: true,
            recreated: false,
            deferred: false,
            cpu_work_ms: Some(3.25),
            input_ms: Some(0.125),
            sim_ms: Some(0.5),
            render_ms: Some(2.0),
            poll_ms: Some(0.0625),
            gpu_ms: None,
        }
    }

    fn tick_sample() -> TickSample {
        TickSample {
            tick: 99,
            wall_offset_ms: 500.25,
            tick_ms: 0.187,
            ticks_due_total: 1000,
            outstanding_ticks: 3,
            executed: 990,
            scheduled: 1200,
            late_arrivals: 2,
            late_executions: 7,
            blocked_transitions: 0,
            invalid_dropped: 0,
            budget_exhausted_ticks: 1,
            near_pending: 40,
            far_pending: 80,
            blocked: 0,
            held: 250,
            near_executed_total: 600,
            far_executed_total: 390,
            near_backlogged_ticks: 0,
            far_backlogged_ticks: 12,
            processed_through: Some(97),
            max_executed_due: Some(98),
            transitioned_regions: vec![3, 7],
        }
    }

    fn persistence_sample() -> PersistenceSample {
        PersistenceSample {
            kind: PersistenceKind::Confirmation,
            wall_offset_ms: 77.5,
            tick: 240,
            latency_ms: Some(8.125),
            outcome: "confirmed 125 keys @ 72323405".into(),
            revision: Some("72323405deadbeef".into()),
        }
    }

    fn region_sample() -> RegionSample {
        RegionSample {
            tick: 101,
            wall_offset_ms: 250.0,
            region: 4,
            from_level: 1,
            to_level: 3,
        }
    }

    fn memory_sample() -> MemorySample {
        MemorySample {
            wall_offset_ms: 1000.0,
            rss_kib: Some(48_512),
            held_by_region: vec![0, 0, 125, 125, 125, 125, 125, 125],
            pending_total: 37,
        }
    }

    #[test]
    fn every_record_kind_round_trips() {
        let records = [
            EvidenceRecord::Frame(frame_sample()),
            EvidenceRecord::Tick(tick_sample()),
            EvidenceRecord::Persistence(persistence_sample()),
            EvidenceRecord::Region(region_sample()),
            EvidenceRecord::Memory(memory_sample()),
        ];
        for record in &records {
            let line = record.encode();
            let decoded =
                decode_line(&line).unwrap_or_else(|error| panic!("decode {line}: {error}"));
            assert_eq!(&decoded, record, "round-trip mismatch for {line}");
        }
    }

    #[test]
    fn precision_contract_is_microsecond_milliseconds() {
        // A value finer than the documented precision quantizes to it
        // on encode and decodes to exactly the quantized form.
        let mut sample = frame_sample();
        sample.interval_ms = 0.1234567;
        let line = EvidenceRecord::Frame(sample).encode();
        let mut expected = frame_sample();
        expected.interval_ms = 0.123457;
        assert_eq!(decode_line(&line).unwrap(), EvidenceRecord::Frame(expected));
    }

    #[test]
    fn decode_rejects_unknown_missing_and_malformed_lines() {
        let line = EvidenceRecord::Region(region_sample()).encode();
        assert!(decode_line(&line.replace("\"region\":4", "\"zone\":4")).is_err());
        let missing = line.replace(",\"region\":4", "");
        assert!(decode_line(&missing).is_err());
        assert!(decode_line("{\"type\":\"frame\"").is_err());
        assert!(decode_line("not json at all").is_err());
        assert!(decode_line("{\"type\":\"warp\",\"tick\":1}").is_err());
        // Duplicate fields are rejected.
        let duplicated = line.replace(
            "\"type\":\"region\",",
            "\"type\":\"region\",\"type\":\"region\",",
        );
        assert!(decode_line(&duplicated).is_err());
    }

    #[test]
    fn unavailable_gpu_timestamps_stay_null_never_zero() {
        let mut sample = frame_sample();
        sample.gpu_ms = None;
        let line = EvidenceRecord::Frame(sample.clone()).encode();
        assert!(line.contains("\"gpu_ms\":null"));
        assert!(!line.contains("\"gpu_ms\":0"));
        assert_eq!(decode_line(&line).unwrap(), EvidenceRecord::Frame(sample));

        // A measured zero is a real value, distinct from null.
        let mut measured = frame_sample();
        measured.gpu_ms = Some(0.0);
        let line = EvidenceRecord::Frame(measured.clone()).encode();
        assert!(line.contains("\"gpu_ms\":0.000000"));
        assert_eq!(decode_line(&line).unwrap(), EvidenceRecord::Frame(measured));
    }

    #[test]
    fn percentile_and_one_percent_low_math() {
        // The established ceil-index percentile convention.
        let samples: Vec<f64> = (1..=100).map(f64::from).collect();
        assert_eq!(crate::math::percentile(&samples, 0.50), 50.0);
        assert_eq!(crate::math::percentile(&samples, 0.95), 95.0);
        assert_eq!(crate::math::percentile(&samples, 1.0), 100.0);

        // 101 frames: 100 at 10 ms and one 100 ms spike. The worst 1%
        // is ceil(1.01) = 2 intervals (100 ms + 10 ms) → 2 / 0.11 s.
        let mut intervals = vec![10.0; 100];
        intervals.push(100.0);
        let low = one_percent_low_fps(&intervals).expect("101 samples is enough");
        assert!((low - 2.0 / 0.11).abs() < 1e-9, "got {low}");

        // Below 100 samples the metric is not claimed.
        assert!(one_percent_low_fps(&[10.0; 99]).is_none());
    }

    #[test]
    fn clean_collection_counts_match_files_and_manifest() {
        let dir = evidence_temp_dir("clean");
        let collector =
            EvidenceCollector::start(EvidenceConfig::new(dir.clone(), "clean-test")).unwrap();
        collector.note_metadata("platform", std::env::consts::OS);
        for tick in 0..50u64 {
            let mut sample = tick_sample();
            sample.tick = tick;
            collector.produce(EvidenceRecord::Tick(sample));
            if tick % 10 == 0 {
                collector.produce(EvidenceRecord::Region(region_sample()));
                collector.produce(EvidenceRecord::Persistence(persistence_sample()));
            }
        }
        let outcome = collector.finalize();
        assert!(outcome.is_complete(), "{outcome:?}");
        assert_eq!(outcome.total_samples(), 50 + 5 + 5);

        let manifest = std::fs::read_to_string(dir.join("manifest.json")).unwrap();
        assert!(manifest.contains("\"schema\":\"s6-evidence/1\""));
        assert!(manifest.contains("\"scenario\":\"clean-test\""));
        assert!(manifest.contains("\"platform\""));
        assert!(manifest.contains("\"complete\":true"));

        let ticks = std::fs::read_to_string(dir.join("ticks.jsonl")).unwrap();
        let lines: Vec<&str> = ticks.lines().collect();
        assert_eq!(lines.len(), 50);
        for line in lines {
            assert!(decode_line(line).is_ok(), "undecodable line: {line}");
        }
        let regions = std::fs::read_to_string(dir.join("regions.jsonl")).unwrap();
        assert_eq!(regions.lines().count(), 5);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn queue_overflow_counts_loss_and_fails_the_verdict() {
        let dir = evidence_temp_dir("overflow");
        let mut config = EvidenceConfig::new(dir.clone(), "overflow-test");
        config.channel_capacity = 4;
        let collector = EvidenceCollector::start(config).unwrap();
        let control = collector.fault_control();
        control.park_writes();
        let deadline = Instant::now() + Duration::from_secs(5);
        while !control.park_entered() {
            assert!(Instant::now() < deadline, "the writer never parked");
            std::hint::spin_loop();
        }
        // 50 samples against a 4-slot queue held by the parked
        // writer: 4 admitted, 46 counted lost — deterministically.
        for tick in 0..50u64 {
            let mut sample = tick_sample();
            sample.tick = tick;
            collector.produce(EvidenceRecord::Tick(sample));
        }
        control.release_park();
        let outcome = collector.finalize();
        assert!(!outcome.is_complete());
        let ticks = outcome
            .streams
            .iter()
            .find(|row| row.name == "ticks")
            .unwrap();
        assert_eq!(ticks.produced, 50);
        assert_eq!(ticks.lost, 46);
        assert!(outcome.failure.is_some());
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn write_failure_fails_the_verdict_without_losing_counts() {
        let dir = evidence_temp_dir("write-failure");
        let collector =
            EvidenceCollector::start(EvidenceConfig::new(dir.clone(), "write-failure-test"))
                .unwrap();
        collector.fault_control().set_fail_writes(true);
        for tick in 0..20u64 {
            let mut sample = tick_sample();
            sample.tick = tick;
            collector.produce(EvidenceRecord::Tick(sample));
        }
        let outcome = collector.finalize();
        assert!(!outcome.is_complete());
        let ticks = outcome
            .streams
            .iter()
            .find(|row| row.name == "ticks")
            .unwrap();
        assert_eq!(ticks.produced, 20);
        assert_eq!(ticks.write_failed, 20);
        assert_eq!(ticks.written, 0);
        assert_eq!(ticks.lost, 0);
        assert!(outcome.failure.is_some());
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_clean_empty_collection_writes_a_null_failure_manifest() {
        // The manifest encodes the absence of failure as an explicit
        // null — distinct from any failure text.
        let dir = evidence_temp_dir("manifest-failure-null");
        let collector =
            EvidenceCollector::start(EvidenceConfig::new(dir.clone(), "null-failure")).unwrap();
        let outcome = collector.finalize();
        assert!(outcome.is_complete());
        let manifest = std::fs::read_to_string(dir.join("manifest.json")).unwrap();
        assert!(manifest.contains("\"failure\":null"));
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn lod_ordinals_are_documented_and_stable() {
        assert_eq!(lod_ordinal(LodLevel::Full), 3);
        assert_eq!(lod_ordinal(LodLevel::Simplified), 2);
        assert_eq!(lod_ordinal(LodLevel::EventDriven), 1);
        assert_eq!(lod_ordinal(LodLevel::Dormant), 0);
    }
}
