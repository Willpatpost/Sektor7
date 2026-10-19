# Sektor7 Original Engine — Stage 0 Workspace

> **Status:** Stage 0 (workspace only) — lead-authorized 2026-10-19, completed the same date. Stage 1 (SDL3 window/input/lifecycle) and Stage 2 (Vulkan presentation) are **not begun** and require explicit approval first (`docs/TECH-ARCHITECTURE.md` §6; `docs/DECISIONS.md` T-08).

Rust workspace holding the first real foundation of the reusable original engine. Scope is deliberately minimal: only the mechanisms the 2026-10-19 architecture review ratified, each exercised by its own tests — nothing speculative (anti-abstraction ledger, `docs/TECH-ARCHITECTURE.md` §8).

## Crates

| Crate | Kind | Contents |
|---|---|---|
| `engine-core` | library | Canonical clock seed (`Tick`); event spine — scheduling with in-class `(due, seq)` ordering, cross-class causal dependencies, per-tick budget, execution ledger with first-class lateness; **safe processed-through watermark** distinct from max-executed and computed live (retreats on late arrivals); FNV-1a logical-state fingerprints; basic level-filtered diagnostics |
| `engine-slice` | library + binary | Interactive command-line slice driving `engine-core`; deterministic self-test scenario |

External dependencies: **none**. Future Vulkan (`ash` investigation candidate) and SDL3 integration attach in Stage 1/2 without touching these contracts — the core is host-loop-agnostic (any event loop can call `run_tick`), which is exactly how Stage 0 preserves the ratified renderer path without implementing it.

## Build and test

```sh
cargo build --workspace
cargo test --workspace
cargo fmt --all -- --check
cargo clippy --workspace --all-targets -- -D warnings
```

Determinism discipline (project tradition from E-07–E-10): logical-state fingerprints use FNV-1a — stable across platforms and compiler versions, unlike `std`'s hashers; fingerprinted state never derives from unordered iteration; wall-clock time never enters logical state. The required double-run test is `engine-core/tests/determinism.rs`; the binary also supports cross-process double runs:

```sh
cargo run -p engine-slice -- --fingerprint-selftest   # run twice — outputs must match
```

## Interactive slice

```sh
cargo run -p engine-slice
```

Commands: `help`, `tick [n]`, `at <due> <payload> [dep_seq…]`, `rel <delay> <payload> [dep_seq…]`, `status`, `counters`, `ledger [n]`, `fingerprint`, `diag [off|trace|debug|info|warn|error]`, `quit` (EOF also exits cleanly).

## What is deliberately absent

Renderer (Vulkan/`ash`), windowing/input (SDL3), physics, ECS, scripting, audio, cooked asset formats, networking, editor, threading/job framework, subsystem registries, render graph — each deferred with a stated trigger (`docs/TECH-ARCHITECTURE.md` §5/§8).

## Tested environments

| Environment | Method | Result |
|---|---|---|
| Linux (dev pod, x86_64, rustc 1.99.1) | direct `cargo build/test/fmt/clippy` | green — see the Stage-0 report |
| Windows (windows-latest runner) | GitHub Actions `engine-ci` | see CI runs |
| macOS, other distributions, 32-bit targets | untested | — |

CI (`.github/workflows/engine-ci.yml`): format check, clippy with `-D warnings`, build, tests, and a cross-process fingerprint double-run on ubuntu-latest and windows-latest. It runs on pushes and PRs touching `engine/`, using GitHub-hosted runners within the repository's existing GitHub allowance — no new developer-funded infrastructure.

Owners: `docs/TECH-ARCHITECTURE.md` (architecture and the §6 Stage-0 record), `docs/DECISIONS.md` (registry, T-01–T-08).
