# Contributing to Tessera

Thanks for taking a look. Tessera is young, so the most useful contributions
right now are bug reports, test cases from your own NoC setups, and the items on
the [roadmap](guide/roadmap.md).

## Finding something to work on

- Issues labeled
  [`good first issue`](https://github.com/amin-norollah/Tessera/labels/good%20first%20issue)
  are small and self-contained.
- Issues labeled
  [`help wanted`](https://github.com/amin-norollah/Tessera/labels/help%20wanted)
  are bigger pieces where I'd especially welcome help.
- Questions, half-formed ideas and "would this be useful?" go in
  [Discussions](https://github.com/amin-norollah/Tessera/discussions).

If you pick up an issue, leave a comment so others know it's taken.

## Before you start

- For anything bigger than a small fix, open an issue first so we can agree on
  the approach before you write a lot of code.
- Have a look at [how Tessera is put together](guide/architecture.md). A few
  rules keep the engine fast and deterministic, and changes need to respect them:
  - No global state. A `Simulation` owns everything it uses.
  - Refer to routers, flits and packets by their small numeric handles, not by
    pointers or `Rc`/`Arc`.
  - No allocation inside the per-cycle stages once a run is going.
  - Randomness comes from the per-node RNG streams, never a global RNG.
  - Lower crates never depend on higher ones (core first, CLI last).
- New routing, selection or allocation code comes with tests. Anything that
  claims determinism needs a test that reruns and compares.

## Workflow

1. Fork the repo and branch off `main`.
2. Keep pull requests small: one change per PR.
3. Before you push, run:

   ```sh
   cargo fmt --all
   cargo clippy --workspace --all-targets -- -D warnings
   cargo test --workspace
   ```

   If you have Node.js, `npm run verify` runs all three plus the formatting check.
4. In the PR description, say what changed, why, and how you tested it.
5. Tidy your commits before asking for review. Nobody needs five `fix typo`
   commits.

## Commit messages

We use [Conventional Commits](https://www.conventionalcommits.org/) prefixes:
`feat`, `fix`, `docs`, `test`, `style`, `chore`, with an optional crate scope.
Write the summary in the imperative mood and keep it short:

```
feat(routing): add odd-even routing
fix(core): reset the dateline flag on a DOR axis change
```

Use the body to explain *why* if the diff doesn't make it obvious.

## Review

- Changes on a per-cycle or per-flit path should come with benchmark numbers.
  We don't merge performance claims without them.
- Changing a plugin trait after Phase 2 is a breaking change and needs a
  discussion first, not just an approval.

## Bugs and feature requests

Open an issue. Tell us what you expected, what happened instead, and how to
reproduce it. A config file and seed that trigger the problem help a lot; since
runs are deterministic, that's usually enough for us to see the same thing.

## Code of conduct

Please be kind. The [code of conduct](CODE_OF_CONDUCT.md) is short and worth a
read.

## License

By contributing, you agree that your contributions are dual licensed under the
MIT and Apache-2.0 licenses, the same as the rest of the project.
