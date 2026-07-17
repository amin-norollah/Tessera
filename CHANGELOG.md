# Changelog

Notable changes to Tessera, loosely following [Keep a Changelog](https://keepachangelog.com/).

## [Unreleased]

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
