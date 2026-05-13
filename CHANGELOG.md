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
    `BatchSink` (Vec/CSV; Arrow/Parquet writer is the drop-in next step) and fixed-order
    deterministic reductions (P3-F14/N2/N3).

  All models are behaviour-tested (V² scaling, leakage-law shapes incl. TEI inversion,
  link distance scaling, area monotonicity, analytic RC transient/steady-state, detector
  firing on crafted cases, `O(batch)` memory over a 100k-row run). Absolute-accuracy
  validation vs DSENT/HotSpot and the Arrow/Parquet serializer are deferred to the FFI +
  calibrated-mode work (P3-F9/F10, gates G3.1/G3.4). Added RESEARCH_NOTES D6 (ASU PTM)
  and D7 (ITRS/IRDS) for the concrete parameter-value provenance.
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
