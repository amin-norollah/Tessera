# Tessera

A modern, high-performance NoC (network-on-chip) simulator, written in Rust.

Tessera is a clean-room successor to PAT-Noxim. The goal is an engine that runs
in parallel, replays deterministically so you can scrub back and forth through a
run, and keeps the power/area/thermal modeling that made PAT-Noxim useful for
research; without the SystemC baggage (single-threaded kernel, global state,
compile-time-fixed sizes, no snapshot/replay).

**Status: Phase 0 (core prototype).** The Rust engine spine runs: a deterministic,
double-buffered BSP kernel on a 2D mesh with XY routing, single-VC credit flow
control, uniform traffic, and a headless latency-throughput runner. The Angular
front-end is scaffolded in [`web/`](web/) but not yet built out.

## Build, run, test

Prerequisites: [Rust](https://rustup.rs/) (stable toolchain) and
[Node.js](https://nodejs.org/) (LTS). This repo is one npm workspace, so a single
`npm install` at the root installs all front-end and tooling dependencies (there is
no per-package `node_modules`).

```sh
npm install              # once: tooling + web deps (hoisted to the root)

# Rust engine (Phase 0 simulator)
npm run sim:build        # compile the workspace
npm run sim:test         # run the test suite (incl. the Phase 0 gate checks)
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

## Docs

Everything lives in [`docs/`](docs/); start there:

- [`docs/INITIAL_PRD.md`](docs/INITIAL_PRD.md); the main design document:
  rationale, prior-art comparison, architecture, tech stack, component spec.
- [`docs/phases/`](docs/phases/); a separate PRD per implementation phase, each
  with its own scope, requirements, and exit gate.
- [`docs/RESEARCH_NOTES.md`](docs/RESEARCH_NOTES.md); the domain research (papers,
  tools, simulators) behind the design decisions, with sources.

We're working toward Phase 0 (the core prototype) now.

## License

MIT or Apache-2.0, at your option; the usual Rust convention.
