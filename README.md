# Tessera

A modern, high-performance NoC (network-on-chip) simulator, written in Rust.

Tessera is a clean-room successor to PAT-Noxim. The goal is an engine that runs
in parallel, replays deterministically so you can scrub back and forth through a
run, and keeps the power/area/thermal modeling that made PAT-Noxim useful for
research; without the SystemC baggage (single-threaded kernel, global state,
compile-time-fixed sizes, no snapshot/replay).

**Status: Phase 1 (full router microarchitecture + config).** The Rust engine runs a
cycle-accurate, multi-VC, credit-flow-controlled router (RC→VA→SA→ST) over 2D mesh
and torus topologies, with a routing family (XY/XYZ, west-first/north-last/negative-
first/odd-even, torus-aware DOR, table-based/Noxim-imported), a selection family
(first-available, buffer-level, round-robin, random), and pluggable VA/SA arbiters
(round-robin, matrix, iSLIP) behind a shared `Allocator` trait. `tessera-config`
validates runtime configs and imports legacy Noxim traffic/routing tables. Fully-
adaptive + Duato escape-VC routing is a deferred, explicitly-documented follow-up
(see `docs/phases/PHASE_1_PRD.md` §7.1). The Angular front-end is scaffolded in
[`web/`](web/) but not yet built out.

## Build, run, test

Prerequisites: [Rust](https://rustup.rs/) (stable toolchain) and
[Node.js](https://nodejs.org/) (LTS). This repo is one npm workspace, so a single
`npm install` at the root installs all front-end and tooling dependencies (there is
no per-package `node_modules`).

```sh
npm install              # once: tooling + web deps (hoisted to the root)

# Rust engine (Phase 1 simulator)
npm run sim:build        # compile the workspace
npm run sim:test         # run the test suite (incl. the Phase 0/1 gate checks)
npm run sim:run -- --rate 0.1 --width 8 --height 8      # one operating point
npm run sim:sweep        # 8x8 latency-throughput sweep -> curve.csv
npm run sim:run -- --help                                # all options

# Angular front-end
npm run web:start        # dev server at http://localhost:4200
npm run web:build        # production bundle -> web/dist/

# Quality gate (formatting + clippy + tests)
npm run verify
```

The simulator prints a table and, with `--out`, writes a CSV with columns
`offered_pkt_rate, offered_flit_rate, accepted_flit_rate, avg_packet_latency,
injected_packets, ejected_packets`. Commits are auto-formatted by a Husky +
lint-staged pre-commit hook (Prettier for web/config, `rustfmt` for Rust).

The `sim:*` scripts call `cargo`, so `~/.cargo/bin` must be on your PATH (rustup
sets this up by default).

## Reading the results

### Sweep

A sweep produces the classic latency-throughput ("hockey-stick") curve. As you
raise the offered load you should expect three regimes:

- **Below saturation:** `accepted_flit_rate` tracks `offered_flit_rate` and latency
  stays low and roughly flat; the network keeps up with everything injected.
- **The knee:** accepted load stops matching offered and latency turns sharply
  upward; this is the saturation point.
- **Past saturation:** accepted load flatlines at the network's capacity (~0.29
  flits/node/cycle for the default 8x8 mesh) while latency grows without bound.

Why: throughput saturates because uniform traffic is limited by the mesh
**bisection** (the central links every crossing packet shares), and latency
diverges because injection is open-loop, so surplus load piles up in the source
queues. Zero-load latency (the curve's left edge) is just the average hop count
times the per-hop cost. A correct run also shows `injected_packets ==
ejected_packets` on every row (no flit loss; the network fully drains).

## Docs

Everything lives in [`docs/`](docs/); start there:

- [`docs/INITIAL_PRD.md`](docs/INITIAL_PRD.md); the main design document:
  rationale, prior-art comparison, architecture, tech stack, component spec.
- [`docs/phases/`](docs/phases/); a separate PRD per implementation phase, each
  with its own scope, requirements, and exit gate.
- [`docs/RESEARCH_NOTES.md`](docs/RESEARCH_NOTES.md); the domain research (papers,
  tools, simulators) behind the design decisions, with sources.

Phase 2 (the stable plugin API) is substantially in place already (registry,
wasmtime sandbox, conformance harness); Phase 3 (power/thermal + diagnostics) is
next, after closing the remaining Phase 1 item (Duato escape-VC routing).

## License

MIT or Apache-2.0, at your option; the usual Rust convention.
