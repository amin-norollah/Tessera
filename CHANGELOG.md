# Changelog

Notable changes to Tessera, loosely following [Keep a Changelog](https://keepachangelog.com/).

## [Unreleased]

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
