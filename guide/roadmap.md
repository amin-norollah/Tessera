# Roadmap

Tessera is being built in phases. Each one adds a layer on top of the last.

| Phase | Goal | Where it stands |
|---|---|---|
| 0 | A deterministic core that reproduces a classic latency-throughput curve | Done |
| 1 | A full virtual-channel router, routing and selection families, config files | Mostly done |
| 2 | A stable plugin API | Done |
| 3 | Power, area, thermal and network-health diagnostics | Native models done |
| 3.5 | Realistic traffic: trace replay and Markov models | Done |
| 4 | Recording, replay and analysis of runs | Done |
| 5 | A browser UI for watching and comparing runs, and export for papers | Not started |
| 6 | More topologies: chiplets, photonic links, irregular networks | Not started |
| 7 | Processor models, and running big simulations on GPUs or clusters | Not started |

## Next up

- **Parallel execution.** The design allows it, but the engine still runs on one
  thread. It will only count as done when a benchmark shows the speedup and the
  results stay identical.
- **Validating power against reference tools.** Linking DSENT, HotSpot and the
  others, then checking that Tessera's own models land close to them.
- **Finishing the router.** A few Phase 1 pieces are still missing: deeper
  pipeline options, speculative allocation, escape-channel adaptive routing, and
  a couple of selection policies.
- **Reading upstream trace formats** directly, instead of only Tessera's own.

After that, the web UI.
