# What Tessera is, and why

Tessera is a simulator for networks-on-chip: the small packet-switched networks
that connect cores, caches and accelerators inside a modern chip. You describe a
network (topology, routers, routing rules, traffic) and Tessera runs it cycle by
cycle, telling you latency, throughput, power, and temperature.

## Why another NoC simulator

Noxim and its extended version, PAT-Noxim, are good research tools, but they
show their age. They're built on SystemC,
so a run is single-threaded and full of global state, sizes are fixed when you
compile, and once a run is over it's gone. If something odd happens at cycle
2,000,000 you rerun the whole thing with more logging and hope.

Tessera starts from scratch in Rust with a few goals:

- **Reproducible runs.** Same config and seed, same result, every time. This is
  what makes everything else possible.
- **Look back into a run.** Record it once, then jump to any cycle and see what
  the network was doing, or compare two runs side by side.
- **Keep the physics.** Power, area and thermal estimates were the main reason
  people used PAT-Noxim, so they stay. Every coefficient now carries a citation,
  so you can see where a number came from and change it.
- **Easy to extend.** Routing algorithms and selection policies are plugins,
  so trying a new idea doesn't mean forking the simulator.
- **Fast.** The engine is built so it can run in parallel later without losing
  determinism.

## What works today

- A detailed virtual-channel router on 2D mesh and torus networks, with the usual
  routing algorithms (XY, turn models, odd-even, table-based) and several
  selection and arbitration policies.
- Synthetic traffic, Noxim traffic tables, dependency-driven trace replay, and
  Markov-model traffic.
- Power, area and thermal models, including the feedback between temperature and
  leakage.
- Deadlock, livelock and congestion detection.
- Recording, seeking and replaying runs, with the event log stored as Parquet and
  queryable with SQL.

## What doesn't yet

- The engine runs on one thread.
- The power numbers haven't been checked against reference tools like DSENT and
  HotSpot. The models behave correctly, but absolute accuracy isn't validated.
- There's no graphical interface yet. Everything runs from the command line.

See [roadmap.md](roadmap.md) for what's next.
