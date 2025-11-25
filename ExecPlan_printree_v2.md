# printree v2 Benchmark Harness and Index Foundations ExecPlan

This ExecPlan is a living document maintained in accordance with `.agent/PLANS.md`. It must stay self-contained so a newcomer can execute it without any external context.

## Purpose / Big Picture

The v2 effort aims to deliver verifiable performance and stable on-disk contracts. The immediate user-visible goal is a reproducible benchmark harness (`printree-bench`) that can generate synthetic 1M-file trees with realistic characteristics and later run standardized performance cases. Users should be able to run `printree-bench gen --files 1_000_000 --depth 20 --symlinks 5000 --random-sizes` to create test trees without guessing parameters. This harness is Deliverable #1 and a blocker for all subsequent work (index schema, update engine, mode enforcement, etc.).

## Progress

- [x] (2025-11-25 15:08Z) Drafted the ExecPlan capturing Deliverable #1 priorities and wiring it to the repository context.
- [x] (2025-11-25 15:40Z) Implemented the synthetic tree generator CLI (`printree-bench gen`) with scale controls, symlink storms, hidden files, sparse random sizes, and mtime variance.
- [x] (2025-11-25 15:40Z) Added a benchmark runner stub (`printree-bench run`) that writes a structured JSON report to preserve the future metrics contract.
- [ ] (2025-11-25 15:40Z) Document validation steps and expected outputs once the harness produces observable artifacts.

## Surprises & Discoveries

- None yet; this section will capture performance quirks or filesystem behaviors uncovered while generating large trees.

## Decision Log

- Decision: Default generator root is `./bench-data/gen`, and `--force` is required to overwrite existing trees to avoid accidental data loss.
  Rationale: Keeps benchmarks contained and forces explicit opt-in to destructive cleanup.
  Date/Author: 2025-11-25 / assistant
- Decision: The initial `run` subcommand emits a JSON stub rather than fake metrics to lock the CLI contract while leaving room for real instrumentation.
  Rationale: Prevents misleading outputs and keeps CI wiring unblocked while performance probes are still pending.
  Date/Author: 2025-11-25 / assistant

## Outcomes & Retrospective

- Pending. This section will summarize whether the harness achieves reproducible generation and how well it supports the planned 1M-file benchmarks.

## Context and Orientation

The `printree` crate currently exposes the main CLI in `src/main.rs` with argument parsing in `src/cli/args.rs`. There is no benchmark binary or generator. Deliverable #1 from the engineering plan requires a new `printree-bench` binary with `gen` and `run` subcommands. Supporting utilities (randomized tree construction, symlink creation, mtime manipulation) will live under `src/bin/printree_bench.rs`. The harness must create deep and wide directory structures, mix file sizes (including sparse files up to ~1GB), include hidden entries, and apply randomized mtimes. The run subcommand will initially emit a JSON stub so downstream CI wiring has a concrete contract even before metrics are implemented.

## Plan of Work

First, add a new binary target `src/bin/printree_bench.rs` that uses `clap` to expose `gen` and `run` subcommands. The `gen` command will accept file count, maximum depth, symlink count, random size toggle, optional RNG seed, destination root, and a `--force` flag to clear existing output. Implement deterministic yet varied directory generation by sampling depth and path segments, ensuring both deep and wide layouts, hidden names, and randomized mtimes using `filetime`. File sizes will be assigned via `set_len` to avoid heavy writes while still spanning bytes to ~1GB when random sizes are requested. Symlink creation must target existing files or directories and remain deterministic when a seed is provided. The `run` subcommand will accept `--cases` and `--out` arguments, emit a structured JSON report stub, and fail fast on unsupported cases to preserve the future contract for real metrics. Update `Cargo.toml` with required dependencies (`rand`, `filetime`) and keep all changes gated to the new binary so existing CLI behavior remains untouched.

## Concrete Steps

Run the following from the repository root:

1. Add `rand` and `filetime` to `[dependencies]` in `Cargo.toml`.
2. Create `src/bin/printree_bench.rs` implementing `clap`-based argument parsing for `gen` and `run` with the behaviors described above.
3. Ensure `gen` can clear the output directory with `--force`, generate directories/files with randomized depth/width, assign mtimes and optional sparse sizes, and create the requested number of symlinks pointing at generated entries.
4. Implement `run` to write a JSON stub report to the requested path, making it obvious that metrics are pending while keeping the interface stable.
5. Format and test with `cargo fmt` and `cargo test` to ensure the new binary compiles and does not disturb existing behavior.

## Validation and Acceptance

After implementing the generator, run:

- `cargo run --bin printree-bench -- gen --files 1000 --depth 5 --symlinks 10 --random-sizes --root /tmp/ptree-gen --seed 42 --force`

Expect a populated `/tmp/ptree-gen` tree containing hidden files, varying directory depths, and symlinks. File metadata should show varied sizes (including sparse allocations) and mtimes spanning different timestamps. The command should complete without panics even at large scales (smoke-tested with smaller counts for local sanity). The `run` subcommand should write a JSON stub when invoked via `cargo run --bin printree-bench -- run --cases all --out /tmp/bench.json`.

## Idempotence and Recovery

The `--force` flag allows safe regeneration by clearing the target root before writing. Without `--force`, generation should refuse to overwrite existing data to avoid accidental loss. Using a fixed `--seed` yields reproducible directory layouts and symlink choices. If generation fails mid-way, re-run with `--force` to start clean.

## Artifacts and Notes

- Placeholder: add command transcripts and example JSON outputs once the generator and runner exist.

## Interfaces and Dependencies

- Add `rand` (StdRng with optional seed) for deterministic randomization.
- Add `filetime` to set modified times reliably across platforms.
- The new binary should remain self-contained under `src/bin/printree_bench.rs` without altering existing modules.

