# Tessera

A modern, high-performance NoC (network-on-chip) simulator, written in Rust.

Tessera is a clean-room successor to PAT-Noxim. The goal is an engine that runs
in parallel, replays deterministically so you can scrub back and forth through a
run, and keeps the power/area/thermal modeling that made PAT-Noxim useful for
research; without the SystemC baggage (single-threaded kernel, global state,
compile-time-fixed sizes, no snapshot/replay).

**Status: design phase.** No engine code yet. The design is written up first, in
detail, so implementation can move fast once it starts.

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
