# Tessera

A cycle-accurate network-on-chip (NoC) simulator written in Rust.

Tessera is a clean-room successor to PAT-Noxim. It keeps the parts that made
PAT-Noxim useful for research (a detailed router model plus power, area and
thermal estimates) and drops the SystemC baggage: global state, sizes fixed at
compile time, and no way to snapshot or replay a run. Every run is deterministic
for a given seed, so you can record it once and jump back to any cycle later.

> **Status:** early and moving fast. The simulator, power/thermal models,
> realistic traffic and run recording all work and are tested. The kernel still
> runs on a single thread, and the web UI hasn't been built yet. There are no
> releases; build from source. See [what's done and what's next](guide/roadmap.md).

## What it can do today

- **Router and network.** Multi-VC, credit-based routers with an
  `RC → VA → SA → ST` pipeline, on 2D mesh and torus (with dateline VCs).
- **Routing.** XY, west-first, north-last, negative-first, odd-even,
  dimension-order on a torus, and table-based routing (including Noxim routing
  tables).
- **Selection and arbitration.** First-available, buffer-level, round-robin and
  random selection. Round-robin, matrix and iSLIP allocators for both VC and
  switch allocation.
- **Traffic.** Uniform, hotspot, transpose, bimodal packet sizes, Noxim traffic
  tables, dependency-driven trace replay (Netrace-style) and Markov-model traffic
  (SynFull-style).
- **Power, area and thermal.** Native models driven by a parameter database where
  every value carries a citation (22 nm PTM and 45 nm ITRS profiles ship in
  [`params/`](params/)). Includes a compact RC thermal model with temperature-dependent
  leakage.
- **Diagnostics.** Deadlock, livelock and congestion detectors, plus an optional
  invariant checker (`--verify`).
- **Recording and analysis.** Keyframes plus deterministic re-simulation to seek
  to any cycle, an Arrow/Parquet event log, a SQLite run catalog, and DuckDB
  queries to compare runs.
- **Plugins.** A stable v1 plugin API for routing and selection, with a registry,
  a WASM sandbox (wasmtime) and a conformance test harness.

## Quick start

You need [Rust](https://rustup.rs/) (stable, 1.97 or newer).

```sh
git clone https://github.com/amin-norollah/Tessera.git
cd Tessera

# Sweep offered load on an 8x8 mesh and print a latency-throughput curve
cargo run --release -p tessera-cli

# Run one point from a config file
cargo run --release -p tessera-cli -- --config examples/mesh_8x8_uniform.toml

# Same run, plus a per-router power/thermal time series and invariant checks
cargo run --release -p tessera-cli -- --config examples/mesh_8x8_uniform.toml \
    --metrics run.csv --verify

# All options
cargo run --release -p tessera-cli -- --help
```

[`examples/mesh_8x8_uniform.toml`](examples/mesh_8x8_uniform.toml) is a small,
commented config to start from. The full schema lives in
[`crates/tessera-config/src/schema.rs`](crates/tessera-config/src/schema.rs).

### Tests

```sh
cargo test --workspace
```

Some heavier features are behind Cargo features that are off by default:

| Feature | What it adds | Test it with |
|---|---|---|
| `tessera-stats/parquet` | Parquet metric export | `cargo test -p tessera-stats --features parquet` |
| `tessera-cli/parquet` | `--metrics run.parquet` in the CLI | `cargo test -p tessera-cli --features parquet` |
| `tessera-record/events,catalog` | Parquet event log, SQLite run catalog | `cargo test -p tessera-record --features events,catalog` |
| `tessera-record/query` | DuckDB queries and run comparison | `cargo test -p tessera-record --features events,query` |

The `query` feature builds DuckDB from source. The first build is slow and needs
several GB of disk space.

### npm scripts (optional)

The repo is also an npm workspace, used for formatting hooks and the (future)
Angular front-end in [`web/`](web/). If you have Node.js installed, `npm install`
once at the root and you get shortcuts such as `npm run sim:test`,
`npm run sim:sweep` and `npm run verify` (format check, clippy and tests). They
are thin wrappers around the `cargo` commands above.

## Reading the results

A sweep prints the classic latency-throughput ("hockey stick") curve. With
`--out curve.csv` it also writes the columns `offered_pkt_rate,
offered_flit_rate, accepted_flit_rate, avg_packet_latency, injected_packets,
ejected_packets`. As you raise the offered load you should see three regimes:

- **Below saturation**, accepted load tracks offered load and latency stays low
  and nearly flat. The network keeps up.
- **At the knee**, accepted load stops following offered load and latency turns
  sharply upward. That is the saturation point.
- **Past saturation**, accepted load flattens out at the network's capacity
  (about 0.29 flits/node/cycle for the default 8x8 mesh) while latency keeps
  growing.

Throughput saturates because uniform traffic is limited by the mesh's bisection,
the central links that every crossing packet has to share. Latency grows without
bound because injection is open-loop, so the extra load piles up in the source
queues. Zero-load latency, the left edge of the curve, is roughly the average hop
count times the per-hop cost. In a correct run, `injected_packets` equals
`ejected_packets` on every row: no flits are lost and the network fully drains.

## Project status

Most of the simulator works: the router and routing families, power and thermal
models, realistic traffic, and run recording. Two things are worth knowing up
front:

- **The engine is single-threaded for now.** It's designed for parallel
  execution, but the parallel scheduler isn't built yet.
- **The power numbers aren't validated against a reference tool yet.** The
  models are tested for the right behavior, but absolute accuracy against
  DSENT/HotSpot needs those tools linked first.

The [roadmap](guide/roadmap.md) has the phase-by-phase picture.

## Repository layout

```
crates/
  tessera-core      kernel: handles, arenas, SoA storage, RNG, the cycle loop
  tessera-model     topologies (mesh, torus)
  tessera-routing   routing, selection and allocator algorithms
  tessera-traffic   synthetic, table, trace and Markov traffic
  tessera-config    TOML config schema, validation, Noxim import
  tessera-plugin    plugin registry, WASM host, conformance tests
  tessera-params    cited power/area/thermal parameter database
  tessera-power     native power and area models
  tessera-thermal   compact RC thermal model, leakage feedback loop
  tessera-diag      deadlock/livelock/congestion detectors, invariant checks
  tessera-stats     metric collection, CSV and Parquet export
  tessera-record    keyframes, replay, event log, run catalog, DuckDB queries
  tessera-cli       the `tessera` command-line runner
  *-sys             bindings to DSENT, ORION, McPAT, HotSpot (not linked yet)
params/             power/thermal parameter profiles
examples/           example run configs
guide/              short project documentation
web/                future browser UI (scaffold only)
```

## Documentation

- [What Tessera is, and why](guide/overview.md)
- [How it's put together](guide/architecture.md)
- [Roadmap](guide/roadmap.md)

## Contributing

Issues and pull requests are welcome. Please read
[`CONTRIBUTING.md`](CONTRIBUTING.md) first.

## License

Licensed under either of [Apache License 2.0](LICENSE-APACHE) or
[MIT license](LICENSE-MIT), at your option.

Unless you explicitly state otherwise, any contribution you intentionally submit
for inclusion in this project, as defined in the Apache-2.0 license, is dual
licensed as above, without any additional terms or conditions.
