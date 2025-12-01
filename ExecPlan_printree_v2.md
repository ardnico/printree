# printree v2 Benchmark Harness and Index Foundations ExecPlan

This ExecPlan is a living document maintained in accordance with `.agent/PLANS.md`. It must stay self-contained so a newcomer can execute it without any external context.

## Purpose / Big Picture

The v2 effort aims to deliver verifiable performance and stable on-disk contracts. The immediate user-visible goal is a reproducible benchmark harness (`printree-bench`) that can generate synthetic 1M-file trees with realistic characteristics and later run standardized performance cases. Users should be able to run `printree-bench gen --files 1_000_000 --depth 20 --symlinks 5000 --random-sizes` to create test trees without guessing parameters. This harness is Deliverable #1 and a blocker for all subsequent work (index schema, update engine, mode enforcement, etc.).

## Progress

- [x] (2025-11-25 15:08Z) Drafted the ExecPlan capturing Deliverable #1 priorities and wiring it to the repository context.
- [x] (2025-11-25 15:40Z) Implemented the synthetic tree generator CLI (`printree-bench gen`) with scale controls, symlink storms, hidden files, sparse random sizes, and mtime variance.
- [x] (2025-11-25 15:40Z) Added a benchmark runner stub (`printree-bench run`) that writes a structured JSON report to preserve the future metrics contract.
- [x] (2025-11-26 09:15Z) Promoted the benchmark runner from a stub to a real traversal case that reports wall time and entry counts while validating the presence of the generated tree root.
- [x] (2025-11-26 12:00Z) Instrumented the traversal benchmark with rusage-based deltas (RSS, faults, block ops, context switches) to start quantifying perf guardrails.
- [x] (2025-11-26 13:30Z) Added `/proc/self/io` syscall and I/O byte deltas plus jemalloc allocation deltas to the traversal benchmark to expand measurable perf guardrails.
- [x] (2025-11-26 14:30Z) Hardened traversal correctness metrics by flagging parent-before-child ordering violations and I/O-backed open failures so regressions are surfaced in reports.
- [x] (2025-11-26 15:20Z) Added generation manifests that summarize created entries and wired `printree-bench run` to surface the manifest path in reports while tolerating missing manifests.
- [ ] (2025-11-26 09:15Z) Capture example benchmark outputs and CI wiring notes once additional cases land.

## Surprises & Discoveries

- Observation: Benchmark runs must tolerate missing generation manifests because production-like deployments can prune artifacts.
  Evidence: `printree-bench run` now logs a warning when the manifest is absent but still emits a report.

## Decision Log

- Decision: Default generator root is `./bench-data/gen`, and `--force` is required to overwrite existing trees to avoid accidental data loss.
  Rationale: Keeps benchmarks contained and forces explicit opt-in to destructive cleanup.
  Date/Author: 2025-11-25 / assistant
- Decision: The initial `run` subcommand emits a JSON stub rather than fake metrics to lock the CLI contract while leaving room for real instrumentation.
  Rationale: Prevents misleading outputs and keeps CI wiring unblocked while performance probes are still pending.
  Date/Author: 2025-11-25 / assistant
- Decision: Generation writes a manifest with counts and the RNG seed, but manifest write/read failures only warn.
  Rationale: Benchmarks must remain runnable even when artifacts are pruned or filesystems block metadata writes.
  Date/Author: 2025-11-26 / assistant

## Outcomes & Retrospective

- Pending. This section will summarize whether the harness achieves reproducible generation and how well it supports the planned 1M-file benchmarks.

## Context and Orientation

The `printree` crate currently exposes the main CLI in `src/main.rs` with argument parsing in `src/cli/args.rs`. The new `printree-bench` binary (in `src/bin/printree_bench.rs`) implements `gen` and `run` subcommands. Generation builds deep/wide trees with hidden entries, randomized mtimes, optional sparse sizes, and symlink storms. Each generation now writes a manifest alongside the tree (counts by entry type, random seed, and relative paths) and warns on manifest write failures without aborting the creation flow. The `run` subcommand walks the generated tree, records traversal metrics (time, RSS/faults/block ops/context switches, `/proc/self/io` deltas, jemalloc allocations), validates parent-before-child ordering, and includes the manifest path in its JSON report while tolerating missing manifests.

## Plan of Work

First, add a new binary target `src/bin/printree_bench.rs` that uses `clap` to expose `gen` and `run` subcommands. The `gen` command will accept file count, maximum depth, symlink count, random size toggle, optional RNG seed, destination root, and a `--force` flag to clear existing output. Implement deterministic yet varied directory generation by sampling depth and path segments, ensuring both deep and wide layouts, hidden names, and randomized mtimes using `filetime`. File sizes will be assigned via `set_len` to avoid heavy writes while still spanning bytes to ~1GB when random sizes are requested. Symlink creation must target existing files or directories and remain deterministic when a seed is provided. Each generation writes a manifest (JSON) next to the tree with counts by entry type, symlink counts, and the RNG seed; manifest writes should not abort generation on failure but must log warnings. The `run` subcommand accepts `--cases` and `--out` arguments, emits a structured JSON report that includes traversal metrics and, when available, the manifest path while warning if it cannot be read. Update `Cargo.toml` with required dependencies (`rand`, `filetime`) and keep all changes gated to the new binary so existing CLI behavior remains untouched.

## Concrete Steps

Run the following from the repository root:

1. Add `rand` and `filetime` to `[dependencies]` in `Cargo.toml`.
2. Create `src/bin/printree_bench.rs` implementing `clap`-based argument parsing for `gen` and `run` with the behaviors described above.
3. Ensure `gen` can clear the output directory with `--force`, generate directories/files with randomized depth/width, assign mtimes and optional sparse sizes, create the requested number of symlinks pointing at generated entries, and emit a manifest JSON alongside the tree even when some writes fail (warn only).
4. Implement `run` to write a JSON report containing traversal metrics and, when present, the manifest path; emit a warning instead of failing if the manifest cannot be read.
5. Format and test with `cargo fmt` and `cargo test` to ensure the new binary compiles and does not disturb existing behavior.

## Validation and Acceptance

After implementing the generator, run:

- `cargo run --bin printree-bench -- gen --files 1000 --depth 5 --symlinks 10 --random-sizes --root /tmp/ptree-gen --seed 42 --force`

Expect a populated `/tmp/ptree-gen` tree containing hidden files, varying directory depths, and symlinks. File metadata should show varied sizes (including sparse allocations) and mtimes spanning different timestamps. The command should complete without panics even at large scales (smoke-tested with smaller counts for local sanity). A manifest JSON should appear alongside the tree with counts and seed; if it cannot be written, generation should still finish after logging warnings. The `run` subcommand should write a JSON report when invoked via `cargo run --bin printree-bench -- run --cases all --out /tmp/bench.json`, embedding the manifest path when readable and logging a warning otherwise.

## Idempotence and Recovery

The `--force` flag allows safe regeneration by clearing the target root before writing. Without `--force`, generation should refuse to overwrite existing data to avoid accidental loss. Using a fixed `--seed` yields reproducible directory layouts and symlink choices. If generation fails mid-way, re-run with `--force` to start clean. Manifest writes are best-effort; if a manifest is missing or unreadable, benchmarks should still run and record the absence in the report.

## Artifacts and Notes

- Placeholder: add command transcripts, example manifests, and example JSON outputs once the generator and runner exist.

## Interfaces and Dependencies

- Add `rand` (StdRng with optional seed) for deterministic randomization.
- Add `filetime` to set modified times reliably across platforms.
- The new binary should remain self-contained under `src/bin/printree_bench.rs` without altering existing modules.

Revision note (2025-11-26 15:25Z): Updated the plan to document the generation manifest behavior, report surfacing, and warning-only handling for missing manifests.

