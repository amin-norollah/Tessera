# Changelog

Notable changes to Tessera, loosely following
[Keep a Changelog](https://keepachangelog.com/). There are no releases yet, so
everything is under Unreleased.

## [Unreleased]

### Command line

- Run a simulation from a TOML config file with `tessera --config run.toml`. The
  config covers the topology, router settings, routing, selection, allocators
  and traffic. Settings the engine can't honor are rejected with a clear error.
- Watch power, temperature and network health during a run: `--metrics` writes a
  per-router time series (CSV, or Parquet with the `parquet` feature), and
  `--verify` checks invariants and fails the run on a violation, deadlock or
  livelock.
- An example config in `examples/`.

### Recording and analysis

- Record a run and jump to any cycle. Tessera saves periodic snapshots and
  re-simulates forward from the nearest one, which works because runs are
  deterministic.
- Event log in Parquet, a SQLite catalog of runs, and SQL queries over runs
  through DuckDB, including comparing two runs.
- Optional gzip compression for snapshots, with a benchmark of the space/speed
  trade-off.

### Traffic

- Dependency-driven trace replay: a message is sent only after the messages it
  depends on have arrived.
- Markov-model traffic in the style of SynFull.
- Noxim traffic tables, a closed-loop injection limit, and a trace summary tool.

### Power, area and thermal

- A parameter database where every value cites its source, with 22 nm and 45 nm
  profiles.
- Native models for dynamic energy, leakage, link energy and router area.
- A compact thermal model of the chip, with temperature feeding back into
  leakage, and a hook for thermal throttling policies.
- Deadlock, livelock, congestion and starvation detectors.
- Placeholder crates for DSENT, ORION, McPAT and HotSpot bindings (not linked
  yet).

### Router and routing

- Multi-VC router with round-robin, matrix and iSLIP allocators.
- 2D torus with dateline virtual channels.
- West-first, north-last, negative-first, odd-even, torus and table-based
  routing, plus Noxim routing-table import.
- First-available, buffer-level, round-robin and random selection.
- Bimodal packet sizes and adversarial traffic patterns.

### Plugins

- A plugin registry, a WASM sandbox for routing plugins, and conformance tests
  that any plugin has to pass.

### Core

- Deterministic cycle-accurate kernel, 2D mesh, XY routing, uniform traffic,
  and a latency-throughput sweep runner.

### Fixed

- Turn-model routing (west-first, north-last, negative-first, odd-even) could
  deadlock when paired with some selection policies.
