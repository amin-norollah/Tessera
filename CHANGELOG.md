# Changelog

Notable changes to Tessera, loosely following [Keep a Changelog](https://keepachangelog.com/).

## [Unreleased]

- Run observation in the CLI (OUT-11, P3-F14/F15/F16): `tessera_cli::observe::Observer`
  samples a live run every N cycles over observable state only. Each sample feeds
  the interval's per-router activity (new `ActivityCounts::since`) to a
  `TeiLoop<RcGrid, NoThrottle>`, so power is dynamic plus temperature-dependent
  leakage. It streams one `MetricRow` per router plus a network row (fixed-order
  reductions) and runs the deadlock, livelock and congestion detectors, plus the
  invariant checker in verify mode. `run_configured_with` adds a read-only per-tick
  hook to the configured harness. The CLI gains `--metrics PATH` (CSV, or
  `.parquet` behind tessera-cli's new off-by-default `parquet` feature),
  `--params`, `--interval`, `--diag-window` and `--verify` (non-zero exit on a
  violation, deadlock or livelock), all used with `--config`. Per-router `latency`
  is `NaN` because the kernel measures latency network-wide only. Starvation is not
  run because the kernel exposes no per-flow grant observables. Tested in
  `crates/tessera-cli/tests/observe.test.rs`: rows reconcile exactly with the
  kernel's ejected-flit and hop totals, reported energy covers the dynamic energy
  of the same activity plus leakage, output is deterministic, observing leaves the
  run's result unchanged, and the binary writes the CSV and passes `--verify`.

- Config → engine wiring (OUT-13, OUT-26): `tessera_cli::build`
  (`build_from_config`/`build_from_path`/`run_configured`) turns a validated
  `SimConfigFile` into a live `Simulation` that honors every component choice:
  topology (mesh, or torus with `TorusDimensionOrderXy`), routing and selection by
  registry key, VA/SA allocators and `alloc_iters`, and every `TrafficKind`
  (uniform, Noxim table, Netrace, SynFull) with `PacketSizeCfg` applied. Relative
  traffic paths resolve against the config's directory, and load failures return
  actionable `BuildError`s instead of panicking. The plugin `Registry`'s frozen v1
  factories stay infallible; file-backed sources use this separate path. The CLI
  gains `tessera --config run.toml`.
  - `tessera-traffic::table::TableTraffic`: a Noxim table-based generator (rows
    active while `cycle % t_period ∈ [t_on, t_off)`, a cumulative-PIR injection
    threshold, POR on the cycle after a shot, burst state keyframed).
  - `tessera-plugin`: `buffer-level`, `round-robin` and `random` selection are
    registered alongside `first-available`, with a registry-wide selection
    conformance test.
  - `tessera-config::validate`: `alloc_iters` must be `>= 1`, and `> 1` requires an
    `i_slip` VA or SA phase.
  - Unsupported choices are rejected, not ignored: non-DOR routing on a torus, and
    `vc_classes` (Duato is OUT-14).
  - Tested in `crates/tessera-cli/tests/config_build.test.rs`: an identical
    per-cycle event trace vs a hand-assembled `Simulation` proves that the
    allocator, selection and routing mapping is correct.

- Phase 3 (in progress): native power/area/thermal + diagnostics + streaming stats.
  - `tessera-params` — the cited parameter database (`Param`/`Citation`, TOML
    load/validate/round-trip, provenance stamp, `Strict`/`Lenient` provenance policy)
    with shipped `params/ptm_22nm.toml` and `params/itrs_45nm.toml` profiles (P3-F6/F7/F8).
  - `tessera-power` — native analytical models behind `PowerModel`/`AreaModel` traits:
    per-event dynamic energy `E=α·C·V²` (P3-F1), two leakage laws (classic superlinear
    subthreshold + TEI-aware, P3-F2), distance-dependent link energy (P3-F3), and router
    area (P3-F5).
  - `tessera-thermal` — native HotSpot-style compact RC network `C·dT/dt = P − G·(T−T_amb)`
    over a tile grid with lateral coupling, derived from cited material/geometry params;
    stability-safe sub-stepped forward-Euler integration (P3-F4).
  - `tessera-diag` — deadlock / livelock / congestion / starvation detectors over
    observable state, plus verify-mode invariant checks (flit conservation, monotonic
    counters) (P3-F15/F16).
  - `tessera-stats` — bounded-memory streaming metric time-series with a pluggable
    `BatchSink` (Vec/CSV, plus an Arrow/Parquet `ParquetSink` behind the off-by-default
    `parquet` cargo feature so default builds stay free of the Arrow dependency tree) and
    fixed-order deterministic reductions (P3-F14/N2/N3, G3.4).

  - TEI feedback loop + RTM policy (`tessera-thermal::tei`, P3-F12/F13): each
    configurable interval (default 100k cycles) recomputes per-router power as
    `scale·(dynamic + leakage(T))`, re-solves the thermal network, and lets a
    `ThermalPolicy` set the next interval's per-router scale from the new
    temperatures — closing the temperature→leakage→power→temperature ring.
    Ships `NoThrottle` and `ThresholdThrottle`; tests show the loop converges to a
    stable fixed point, leakage feedback lifts steady temperature above an
    ambient-leakage estimate, and throttling demonstrably lowers steady
    temperature under thermal stress (G3.3).
  - kernel wiring: the `Simulation` stage kernels now accumulate cheap per-router
    `ActivityCounts` (buffer read/write, crossbar, VA/SA arbitration, link flits, clock)
    and cumulative `total_hops`, and expose `activity()`/`network_activity()`/
    `router_occupancy()` observables — no upward dependency on `tessera-power`
    (P3-F1, hot-path accumulation only). `tessera-power` gains `EnergyBreakdown` +
    `energy_from_counts`/`average_power` to convert counts to energy off the hot path.
    End-to-end tests tie the chain together: activity→energy→power→temperature on a
    live mesh run, count self-consistency + determinism, and diagnostics built from
    observable state (healthy run trips nothing, frozen snapshot trips the deadlock
    detector, invariant checker catches injected flit loss).
  - calibrated mode (`tessera-power::calibrate`, P3-F10): closed-form least-squares
    `fit_dynamic_scale` of a single energy-coefficient correction against reference
    operating points, and `apply_dynamic_scale` which persists the fit as a renamed,
    still-fully-cited, TOML-round-trippable profile with the calibration in its provenance.
  - FFI seam crates (P3-F9/F11): `dsent-sys`, `orion-sys`, `mcpat-sys` (power/area) and
    `hotspot-sys` (thermal) implement the same `PowerModel`/`AreaModel`/`ThermalModel`
    traits as the native models. The external C/C++ libraries bind behind an opt-in
    `link` feature; without them (the default) each reports **degraded** via `try_new`
    so runs fall back to native. McPAT additionally exposes `MCPAT_WIRING_VERIFIED = false`
    and stays degraded until its wiring is audited against source (P3-F11). Real bindings
    are pending the libraries (trait bodies are `todo!`); the seam is compile-checked.
    (Grouped as top-level `crates/*-sys` rather than `crates/tessera-ffi/*` from
    INITIAL_PRD §20.1, to fit the workspace member glob.)
  - docs: [`docs/POWER_THERMAL_MODELING.md`](docs/POWER_THERMAL_MODELING.md) — the
    native-model formulas + citations, parameter-DB, TEI loop, diagnostics, calibration,
    and stats-export guide, with a Phase 3 requirement-coverage table.

  All models are behaviour-tested (V² scaling, leakage-law shapes incl. TEI inversion,
  link distance scaling, area monotonicity, analytic RC transient/steady-state, detector
  firing on crafted cases, `O(batch)` memory over a 100k-row run, calibration recovery,
  Parquet round-trip). Still outstanding for Phase 3: the FFI `*-sys` crates
  (DSENT/ORION/HotSpot/McPAT, P3-F9/F11) and the absolute-accuracy regression gate that
  depends on them (P3-N1, G3.1). Added RESEARCH_NOTES D6 (ASU PTM) and D7 (ITRS/IRDS) for
  the concrete parameter-value provenance.
- Phase 3.5 (traffic realism): `tessera-traffic` gains Netrace dependency-driven trace
  replay and SynFull Markov-model synthetic traffic, both native `TrafficGenerator`s
  requiring no core change to run (P3.5-N3).
  - kernel: `TrafficGenerator` gains an additive `on_deliver(cycle, Delivery)` hook
    (default no-op, so no Phase 0-2 generator needs a change) fired from `Simulation`'s
    `eject()` when a packet's tail flit is delivered; `PacketDesc`/`FlitHeader` gain an
    opaque `tag: u64` correlation id, round-tripped from injection to delivery, so a
    stateful generator can recognize which of its own packets just arrived.
  - `reorder::ReorderBuffer<T>` — a generic per-source reassembly buffer for
    non-deterministic (adaptive) routing's out-of-order delivery (P3.5-F5); every
    arrival-permutation of a run is exercised as a property test (G3.5.3).
  - `closed_loop::ClosedLoop<G>` — wraps any `TrafficGenerator` to cap outstanding
    (injected, undelivered) packets per node (P3.5-F4), freed via `on_deliver`; composes
    with `Bimodal` and with dependency-gated sources.
  - `netrace::Netrace` — reads a documented, versioned trace interchange format ("TNI
    v1"; **not** upstream `libnetrace`'s binary format, see `docs/OUTSTANDING_PRD.md`
    OUT-24), validates the dependency graph is a DAG on load (rejects cycles/unknown
    deps), and releases a message at its source only once every dependency predecessor
    has delivered (P3.5-F1), preserving the workload's critical path. Supports region
    selection (skip/warm-up, P3.5-F1) and a node-count remap policy (identity/modulo,
    P3.5-F6). Reassembles each source's deliveries into true trace order via
    `ReorderBuffer` regardless of network delivery order (P3.5-F5), and stamps trace
    identity/parameters via `TraceProvenance::stamp` (P3.5-F7).
  - `synfull::SynFull` — a per-node Markov-chain generator sampled from a Tessera-native
    TOML model (`SynFullModel::from_toml`, P3.5-F3): each node independently walks the
    shared model's states from its own `NodeRng` stream, sampling injection, destination
    (uniform or hotspot-weighted), bimodal size, and the next-state transition.
  - `summary::summarize` — trace summary tool (P3.5-F8): message count, size mix, max
    dependency depth (stack-safe topological pass), per-node injection profile.
  - `tessera-config`: `TrafficKind::Netrace{path, region, node_mapping}` and
    `TrafficKind::SynFull{model_path}` schema variants + validation rules (non-empty
    path, well-formed region).
  - Tested over a real `Simulation` (`crates/tessera-cli/tests/traffic_realism.test.rs`):
    a fan-in dependency trace proves a consuming message injects strictly after both its
    producers deliver (G3.5.1), replay is bit-for-bit deterministic under a fixed seed
    (P3.5-N1), and SynFull composes into `Simulation::new` with no core change (P3.5-N3).
  - Outstanding: real upstream Netrace/SynFull format ingestion (OUT-24), bounded-memory
    streaming for multi-gigabyte traces (OUT-25, P3.5-N2), `TrafficCfg`→engine/CLI wiring
    for every traffic kind including these (OUT-26), and SynFull fidelity validation
    against published reference statistics (OUT-27, G3.5.2).
- Phase 4 (in progress), Milestone A — deterministic keyframe/re-simulation spine
  (G4.1/G4.2; recorder→Arrow/Parquet event stream, DuckDB query layer, SQLite catalog,
  and run comparison are follow-on milestones, tracked as OUT-17):
  - kernel: `Simulation` gains an additive, off-by-default `Event` stream (`Injected`,
    `Routed{cand_mask,chosen}`, `VcAlloc`, `SaGrant`, `Traversed`, `Delivered{latency}`,
    plus schema-complete-but-unemitted `Dropped`/recorder-sampled `BufOccupancy`/
    `ThrottleOn`) pushed from the stage kernels only when `record_events(true)` is set —
    a single well-predicted branch and no allocation on the non-recording path (P4-F4).
  - kernel: `Simulation::snapshot()`/`restore()` capture/restore a plain-data
    `EngineSnapshot` (arena, buffer pool, credit/reservation state, RNG stream states,
    cycle/metrics) behind a new off-by-default `tessera-core` `snapshot` cargo feature
    (P4-F1). Trait objects (topology/routing/selection/allocators/traffic) are **not**
    serialized — a keyframe is only meaningful restored onto a `Simulation` freshly
    reconstructed from the same config; the recorder-side seam for that reconstruction is
    `tessera_record::SimSource`.
  - `TrafficGenerator` and `Allocator` each gain additive `snapshot`/`restore` methods
    (default no-op/`None`, so no existing implementation needs a change). Both `Netrace`
    and `SynFull` implement them, since their internal state (dependency ready-set/
    reorder-buffer state; per-node Markov index) is path-dependent and not derivable from
    RNG-stream restore alone. Likewise `RoundRobin`/`Matrix`/`ISlip` implement them for
    their fairness/priority pointers — a gap the replay-equivalence test itself caught:
    restoring a keyframe onto fresh, config-reconstructed allocators silently reset their
    round-robin pointers and produced a diverging (but each individually still
    "deterministic") arbitration order a few grants after restore.
  - new crate `tessera-record`: `keyframe::{KeyframeWriter, KeyframeReader}` (one
    bincode-encoded file per keyframe cycle — bincode rather than the PRD's indicative
    rkyv, since `EngineSnapshot` is already plain `serde`-derivable data and keyframing
    isn't on the per-cycle hot path, so round-trip simplicity mattered more here than
    zero-copy reads), `seek::SeekEngine` (`seek_to`/`resimulate`, nearest-keyframe +
    replay or from-seed fallback, P4-F2/F3), and `replay::{run_collecting_events,
    first_divergence}` (the event-trace diff helpers behind the replay-equivalence gate).
    Sits below `tessera-config`/`tessera-plugin`/`tessera-cli` in the dependency
    direction via the `SimSource` trait, which the owning crate implements with its own
    config→registry reconstruction.
  - Design decision (documented deviation from a literal PRD reading): "keyframe + delta
    each cycle" is implemented as periodic keyframes **plus deterministic
    `tick()`-replay**, not a stored per-cycle state-diff format — a real diff format would
    force allocation/branching inside the per-cycle stage kernels for zero correctness
    benefit, since determinism already guarantees resim reproduces the recorded run. The
    `Event` stream is both the analytics payload and the bit-for-bit replay-equivalence
    oracle (G4.1's gate), diffed against re-simulation rather than raw engine internals
    (a stable columnar contract vs. implementation detail that a legitimate refactor could
    reorder).
  - Tested over real `Simulation`s (`crates/tessera-cli/tests/replay_equivalence.test.rs`)
    across deterministic (dimension-order + round-robin) and adaptive/RNG-driven
    (`OddEven` + `Random` selection) routing, plus Netrace and SynFull traffic: both
    from-seed re-simulation and nearest-keyframe restore-and-replay reproduce a reference
    run's event trace bit-for-bit (G4.1), and `SeekTo` via an on-disk keyframe store
    matches a from-seed resimulation to the same cycle (G4.2).
  - Milestone B — Arrow/Parquet event stream + SQLite run catalog (P4-F4, P4-F7, gate
    G4.5):
    - `tessera-stats`: `BatchSink`/`StreamingRecorder` are now generic over the row type
      (`BatchSink<R>`, `StreamingRecorder<R, S: BatchSink<R>>`) instead of monomorphic to
      `MetricRow`, so the same bounded-memory streaming pattern serves both Phase 3 stats
      and Phase 4 events without a second implementation. `arrow`/`parquet` moved to
      `[workspace.dependencies]` for version-unification (P3-F14 unaffected: `VecSink`/
      `CsvSink`/`ParquetSink`'s existing behavior is unchanged, just generalized).
    - `tessera-record::event` — an Arrow schema flattening the `Event` enum into one wide,
      nullable-per-variant table (`kind` + every variant's fields), and
      `EventParquetSink: BatchSink<Event>` (same bounded-memory/latched-error shape as
      `tessera-stats`'s `ParquetSink`), behind a new off-by-default `events` feature.
    - `tessera-record::catalog::Catalog` — a SQLite-backed run registry (`register_run`/
      `get_run`/`list_runs`): a run's config travels as opaque text (no dependency on
      `tessera-config`, staying below it in the dependency direction) alongside its seed
      and keyframe-dir/event-store paths, behind a new off-by-default `catalog` feature
      (`rusqlite`'s `bundled` SQLite amalgamation compiled cleanly on this Windows/MSVC
      dev environment — no build pain, unlike the DuckDB risk flagged for OUT-30).
    - `tessera-record::Recorder` — ties keyframe cadence and event streaming into one
      per-cycle call: `start` turns on event recording and captures the cycle-0 keyframe,
      `observe` (called once per cycle after `tick()`) writes a keyframe on the configured
      interval and drains/streams that cycle's events, `finish` flushes and closes.
    - Tested over a real `Simulation` (`crates/tessera-cli/tests/recording.test.rs`): a
      recorded run's Parquet row count matches an independent from-seed run collected
      in-memory, keyframes land on the configured cadence, `SeekTo` via the on-disk
      keyframes still matches resimulation once a real run is streaming to disk (not just
      the in-memory scenario Milestone A's test covers), and a recorded run registers into
      the catalog with its provenance intact (G4.5).
  - Milestone C — DuckDB query layer + run comparison (P4-F5/F6, gate G4.3):
    - `tessera-record::query::Query` — an in-process DuckDB connection registering a run's
      event Parquet file as a view (`read_parquet`, no import step), behind a new
      off-by-default `query` feature. `duckdb`'s `bundled` build (the flagged Windows/MSVC
      risk in OUT-30) compiled cleanly here, but needs its `parquet` Cargo feature enabled
      too (`libduckdb-sys/parquet`, statically linking DuckDB's own Parquet reader):
      without it, `read_parquet` tries to **autoload the extension over the network** at
      query time and fails offline — not just slow, silently wrong for a reproducibility
      tool if it ever half-succeeded. Implementation note: each distinct cargo
      profile/feature combination (`dev`/`test`/`clippy`) triggers its own from-scratch
      compile of DuckDB's C++ amalgamation (multiple GB each); budget disk accordingly
      when touching this dependency.
    - `tessera-record::compare` — `compare_summary` (named scalar-metric deltas from
      caller-supplied SQL) and `compare_timeseries` (aligned per-bucket time-series diff,
      buckets present in only one run aligned against `0.0` on the other side rather than
      truncating to the shorter run), both built on `Query` rather than a second
      data-access path.
    - Tested over real `Simulation`s
      (`crates/tessera-cli/tests/query_and_compare.test.rs`): a recorded run's DuckDB
      aggregates (count, average latency) match an independently-collected in-memory
      event trace (G4.3 "query correctness"), and comparing two runs differing only in
      injection rate produces a correct, monotonic diff in both summary metrics and an
      aligned per-cycle-bucket time series (G4.3 "run comparison").
  - Milestone D (the last one) — keyframe-interval/compression tuning + reproducibility-
    bundle stub (P4-F8/F9, gate N3):
    - `tessera-record::keyframe`: `KeyframeWriter`/`KeyframeReader` gain an optional
      `Compression` (`None`/`Gzip`, self-describing via a 1-byte on-disk tag, so a reader
      never needs the write-side choice out of band).
    - `crates/tessera-cli/examples/keyframe_tuning.rs`: a benchmark sweeping interval ×
      compression on a fixed 8×8-mesh/uniform-traffic scenario, reporting the tradeoff
      (per `docs/AGENTS.md`'s "every optimization claim is gated behind a benchmark," not
      asserted) — see `docs/OUTSTANDING_PRD.md`'s Phase 4 section for a captured run: gzip
      shrinks keyframe storage ~6× at every interval tested, at roughly 2× the per-
      keyframe read cost; total bytes scale with keyframe density as expected, and average
      replay-tick count tracks `interval / 2`.
    - `tessera-record::bundle::export_bundle` (P4-F9): writes a `manifest.toml` +
      `config.toml` for a registered `Catalog` run — referencing the run's keyframe/event-
      store paths rather than copying those artifacts, since the full paper-ready bundle
      (code snapshot, copied results, figures) is Phase 5's job.
  - **Phase 4 is complete**: all four milestones land, and the phase's exit gate
    (G4.1–G4.5) is fully met.
- Phase 1 backfill: `tessera-config` (schema, `validate()`, Noxim traffic/routing-table
  import); a generic `Allocator` trait unifying VC allocation and both switch-allocation
  phases (round-robin/matrix/iSLIP arbiters); `Torus2D` topology with dateline VC
  handling; odd-even and torus-aware dimension-order routing; a congestion-aware
  `SelectionPolicy` family (buffer-level, round-robin, random); table-based routing;
  bimodal packet sizes; credit-conservation property tests; analytical validation
  (bisection-bandwidth bound, adaptive-vs-DOR ordering under transpose traffic)
  substituting for a BookSim/Noxim differential comparison. Fixed a deadlock bug where
  west-first/north-last/negative-first/odd-even were only safe with the specific
  selection policy every prior test happened to use. Duato escape-VC routing deferred
  (see `docs/phases/PHASE_1_PRD.md` §7.1).
- Phase 2 plugin API: component registry, wasmtime WASM sandbox, conformance harness.
- Phase 0 core prototype: deterministic BSP kernel, 2D mesh, XY routing, uniform
  traffic, headless latency-throughput runner.
- Repo scaffolding: README.md, contributing guide, dual MIT/Apache-2.0 license.
- Design docs (initial PRD, phase PRDs, research notes).
