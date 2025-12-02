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
- [x] (2025-11-26 17:05Z) Captured example benchmark outputs and CI wiring notes covering fixtures, regeneration steps, and report expectations.
- [x] (2025-11-26 17:20Z) Checked in sample manifest JSON and a minimal `run` report under `installer/examples/bench/` to anchor downstream consumers.
- [x] (2025-11-26 17:30Z) Added CI smoke targets for `printree-bench gen` (small counts) and `run` to guard the binary contract while the full suite incubates.
- [x] (2025-11-27 08:50Z) Captured the requirement that include-filtered listings must surface all intermediate directories on the path to matched entries and defined normal/abnormal test coverage to enforce it.
- [x] (2025-11-27 10:20Z) Implemented include-friendly traversal that preserves ancestor directories under include filters, added success/failure coverage for path-prefix handling, and refreshed plan notes.

## Surprises & Discoveries

- Observation: Benchmark runs must tolerate missing generation manifests because production-like deployments can prune artifacts.
  Evidence: `printree-bench run` now logs a warning when the manifest is absent but still emits a report.
- Observation: Generation can leave sparse files that appear zeroed even when random sizes are requested because `set_len` does not allocate blocks.
  Evidence: The plan assumes sparse writes; document this so benchmarks do not mistake low disk usage for a bug.

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
- Decision: Sparse allocations remain intentional for `--random-sizes`; do not convert to buffered writes unless we explicitly need filled blocks for an I/O case.
  Rationale: Preserves speed and disk footprint for the 1M-file generator while leaving space for a future "real I/O" case.
  Date/Author: 2025-11-26 / assistant
- Decision: Reference manifests and reports will live under `installer/examples/bench/` so downstream consumers have stable fixtures that match the harness contract.
  Rationale: Keeps fixtures versioned and easy to copy into docs or CI while avoiding churn in the main repo root.
  Date/Author: 2025-11-26 / assistant
- Decision: Include filters must preserve ancestor directories by deriving path prefixes (when possible) and otherwise allowing directory descent whenever includes are present, while still honoring excludes.
  Rationale: Users expect filtered listings to show the full path to matches, and traversal would otherwise prune necessary context.
  Date/Author: 2025-11-27 / assistant

## Outcomes & Retrospective

- The harness now provides reproducible generation (`gen`), traversal metrics (`run`), manifest/report fixtures, and CI smoke guidance so newcomers can prove the contract before extending it. Remaining work for the broader v2 effort (index schema, update engine) should append new milestones while preserving the fixture/CI guardrails to avoid regressions.

## Context and Orientation

 The `printree` crate currently exposes the main CLI in `src/main.rs` with argument parsing in `src/cli/args.rs`. The new `printree-bench` binary (in `src/bin/printree_bench.rs`) implements `gen` and `run` subcommands. Generation builds deep/wide trees with hidden entries, randomized mtimes, optional sparse sizes, and symlink storms. Each generation now writes a manifest alongside the tree (counts by entry type, random seed, and relative paths) and warns on manifest write failures without aborting the creation flow. The `run` subcommand walks the generated tree, records traversal metrics (time, RSS/faults/block ops/context switches, `/proc/self/io` deltas, jemalloc allocations), validates parent-before-child ordering, and includes the manifest path in its JSON report while tolerating missing manifests. Reference fixtures under `installer/examples/bench/` document the current manifest/report schema and sparse-allocation behavior so downstream consumers can lock to concrete examples while CI smoke jobs keep the contract live.

## Plan of Work

First, add a new binary target `src/bin/printree_bench.rs` that uses `clap` to expose `gen` and `run` subcommands. The `gen` command will accept file count, maximum depth, symlink count, random size toggle, optional RNG seed, destination root, and a `--force` flag to clear existing output. Implement deterministic yet varied directory generation by sampling depth and path segments, ensuring both deep and wide layouts, hidden names, and randomized mtimes using `filetime`. File sizes will be assigned via `set_len` to avoid heavy writes while still spanning bytes to ~1GB when random sizes are requested. Symlink creation must target existing files or directories and remain deterministic when a seed is provided. Each generation writes a manifest (JSON) next to the tree with counts by entry type, symlink counts, and the RNG seed; manifest writes should not abort generation on failure but must log warnings. The `run` subcommand accepts `--cases` and `--out` arguments, emits a structured JSON report that includes traversal metrics and, when available, the manifest path while warning if it cannot be read. Update `Cargo.toml` with required dependencies (`rand`, `filetime`) and keep all changes gated to the new binary so existing CLI behavior remains untouched.

Next, harden the plan by locking in fixtures and automation. Capture a small reference manifest and traversal report under `installer/examples/bench/` that match the current schema and explicitly note sparse allocations so readers do not assume dense data. Add CI smoke jobs that run `printree-bench gen --files 50 --depth 4 --symlinks 2 --random-sizes --seed 7 --force --root target/ci-gen` and `printree-bench run --cases all --out target/ci-gen/report.json --root target/ci-gen/tree` to guard against argument or contract regressions without blowing CI time. Document in this plan how to regenerate fixtures when the schema changes and require updates when flags or output fields evolve. Close the loop by recording acceptance criteria and retrospective notes once fixtures and smoke jobs are in place.

Finally, extend the tree listing path filters so that include patterns still render every intermediate directory between the root and matching entries, even when those directories do not themselves satisfy the include glob or regex. Define and implement deterministic logic (favoring path-mode glob prefixes first, then falling back to allowing directories when includes are present) that keeps traversal viable under includes while still respecting excludes. Write normal-path tests that prove ancestors remain in output for include-filtered listings and abnormal-path tests that confirm unrelated directories remain filtered out. Update fixtures or plan notes if output shape or acceptance expectations shift because of the include handling.

## Concrete Steps

Run the following from the repository root:

1. Add `rand` and `filetime` to `[dependencies]` in `Cargo.toml`.
2. Create `src/bin/printree_bench.rs` implementing `clap`-based argument parsing for `gen` and `run` with the behaviors described above.
3. Ensure `gen` can clear the output directory with `--force`, generate directories/files with randomized depth/width, assign mtimes and optional sparse sizes, create the requested number of symlinks pointing at generated entries, and emit a manifest JSON alongside the tree even when some writes fail (warn only).
4. Implement `run` to write a JSON report containing traversal metrics and, when present, the manifest path; emit a warning instead of failing if the manifest cannot be read.
5. Format and test with `cargo fmt` and `cargo test` to ensure the new binary compiles and does not disturb existing behavior.
6. Generate fixtures with `cargo run --bin printree-bench -- gen --files 50 --depth 4 --symlinks 2 --random-sizes --seed 7 --force --root installer/examples/bench/tree` and copy the resulting `manifest.json` to `installer/examples/bench/manifest.small.json`.
7. Produce a traversal report with `cargo run --bin printree-bench -- run --cases all --root installer/examples/bench/tree --out installer/examples/bench/run.small.json`.
8. Add a short README note under `installer/examples/bench/` explaining sparse allocation expectations and how to refresh fixtures when CLI flags or schema fields change.
9. Wire CI smoke jobs (or document the intended pipeline) invoking the same small gen/run commands against a temporary `target/ci-gen` root so PRs catch contract drift.

## Validation and Acceptance

After implementing the generator, run:

- `cargo run --bin printree-bench -- gen --files 1000 --depth 5 --symlinks 10 --random-sizes --root /tmp/ptree-gen --seed 42 --force`

Expect a populated `/tmp/ptree-gen` tree containing hidden files, varying directory depths, and symlinks. File metadata should show varied sizes (including sparse allocations) and mtimes spanning different timestamps. The command should complete without panics even at large scales (smoke-tested with smaller counts for local sanity). A manifest JSON should appear alongside the tree with counts and seed; if it cannot be written, generation should still finish after logging warnings. The `run` subcommand should write a JSON report when invoked via `cargo run --bin printree-bench -- run --cases all --out /tmp/bench.json`, embedding the manifest path when readable and logging a warning otherwise.

For fixtures and CI readiness, validate:

- `cargo run --bin printree-bench -- gen --files 50 --depth 4 --symlinks 2 --random-sizes --seed 7 --force --root installer/examples/bench/tree`
- `cargo run --bin printree-bench -- run --cases all --root installer/examples/bench/tree --out installer/examples/bench/run.small.json`

Confirm the manifest matches `installer/examples/bench/manifest.small.json`, the report matches `installer/examples/bench/run.small.json`, and the README in that directory explains sparse allocation and refresh steps. CI smoke jobs should mirror these commands against a disposable `target/ci-gen` root and fail on schema or flag regressions.

For include-filter behavior, construct fixtures (or ad-hoc temporary trees) where a deep file matches an include. Validate that the printed tree now shows each ancestor directory even when those ancestors do not match the include pattern. Negative validation must cover directories that are neither ancestors nor matches to prove they stay filtered out. Tests should cover both glob and regex include syntaxes where applicable.

## Idempotence and Recovery

The `--force` flag allows safe regeneration by clearing the target root before writing. Without `--force`, generation should refuse to overwrite existing data to avoid accidental loss. Using a fixed `--seed` yields reproducible directory layouts and symlink choices. If generation fails mid-way, re-run with `--force` to start clean. Manifest writes are best-effort; if a manifest is missing or unreadable, benchmarks should still run and record the absence in the report.

## Artifacts and Notes

- Store reference fixtures under `installer/examples/bench/`:
  - `manifest.small.json`: captured from the small-generation command so consumers see the exact schema and sparse counts.
  - `run.small.json`: captured from the matching small-run command so consumers see traversal metrics and manifest linking.
  - `README.md`: notes sparse allocations, explains when to refresh fixtures, and lists the exact regen commands.
- Capture command transcripts in this plan or sibling notes when fixtures are regenerated to keep provenance obvious.
- CI smoke jobs should run the small gen/run commands against `target/ci-gen` and assert exit-code success while archiving the report for debugging.

Revision note (2025-11-27 08:55Z): Added include-filter ancestor rendering requirements, test expectations for success/failure cases, and the implementation milestone to make the ExecPlan a usable guide for the new behavior.
Revision note (2025-11-27 10:20Z): Marked include-handling work complete with implemented traversal logic and tests that cover both expected ancestors and excluded/unrelated directories.

## Interfaces and Dependencies

- Add `rand` (StdRng with optional seed) for deterministic randomization.
- Add `filetime` to set modified times reliably across platforms.
- The new binary should remain self-contained under `src/bin/printree_bench.rs` without altering existing modules.

Revision note (2025-11-26 17:35Z): Completed fixture guidance, regeneration steps, CI smoke expectations, and validation notes for the benchmark harness.

