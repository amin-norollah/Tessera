# How Tessera is put together

A short tour, not a reference. The code is the real documentation; this is the
map you'd want before opening it.

## The simulation loop

Every cycle, each router goes through the classic pipeline: work out where a
packet should go (route compute), claim a virtual channel on that output (VC
allocation), win the crossbar (switch allocation), then move the flit across
(switch and link traversal). Credits flow back so a router never sends into a
full buffer.

The kernel is written in a bulk-synchronous style. Each stage reads the state from
the previous cycle and writes the next one, and routers never reach into each
other's state mid-cycle. That's what keeps runs deterministic, and it's also what
will let the stages run in parallel later.

Some other choices follow from the same idea:

- Everything is referred to by small numeric handles (router 12, flit 4031)
  instead of pointers, and stored in flat arrays.
- Every node gets its own random-number stream, so randomness doesn't depend on
  the order things happen in.
- No global state. A `Simulation` owns everything, so you can run several at once.

## The crates

The workspace is split by job, and dependencies only point one way: from the
core outward.

| Crate | What it does |
|---|---|
| `tessera-core` | The kernel: handles, storage, RNG, the cycle loop |
| `tessera-model` | Topologies (mesh, torus) |
| `tessera-routing` | Routing, selection and allocation algorithms |
| `tessera-traffic` | Traffic generators and trace readers |
| `tessera-config` | The TOML config format and its validation |
| `tessera-plugin` | Plugin registry, WASM sandbox, conformance tests |
| `tessera-params`, `tessera-power`, `tessera-thermal` | Power, area and thermal models |
| `tessera-diag` | Deadlock, livelock and congestion detectors |
| `tessera-stats` | Metric collection and export |
| `tessera-record` | Recording, replay and SQL queries over runs |
| `tessera-cli` | The `tessera` command |

The `*-sys` crates are placeholders for bindings to the established C/C++ tools
(DSENT, ORION, McPAT, HotSpot). They aren't linked yet.

## Power and temperature

The router counts what it does (buffer reads and writes, crossbar traversals,
arbitrations, link flits). Every so often those counts are turned into energy
using coefficients from a parameter file, the energy becomes power, and a simple
thermal model of the chip turns power into temperature. Temperature feeds back
into leakage, which feeds back into power. All of this happens outside the hot
loop, so it costs very little when you don't need it.

## Recording and replay

Because a run is deterministic, Tessera doesn't need to store every cycle. It
saves a snapshot every so often, and to look at cycle N it loads the nearest
snapshot before N and simulates forward. The events of a run (packets injected,
routed, delivered) can also be written to Parquet files, and a small SQLite
catalog keeps track of what was run with which config and seed.
