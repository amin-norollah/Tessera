# Contributing

Tessera is still in the design phase (see the README's project status
section), so there's no real code to contribute to yet; but here's how
things will work once there is.

## Before you dive in

- Skim [docs/AGENTS.md](docs/AGENTS.md); that's the coding standard, and it
  applies whether you're a human or an AI assistant.
- Find the phase PRD for whatever you want to work on (`docs/phases/`). Each
  one spells out scope and what "done" means for that phase; try to stay
  inside those lines rather than jumping ahead to later phases.
- Anything bigger than a small fix: open an issue first so we can agree on
  direction before code gets written.

## Workflow

1. Branch off `main`.
2. Keep PRs small and focused; one change, one PR.
3. Before pushing:
   ```sh
   cargo fmt --all
   cargo clippy --workspace --all-targets -- -D warnings
   cargo test --workspace
   ```
4. In the PR description, say what changed, why, and how you tested it.
5. Clean up your commits before asking for review; nobody needs to see
   `fix typo` five times.

## Commit messages

Imperative mood, short summary line: `Add XY routing algorithm`, not `Added`
or `Adding`. Explain the _why_ in the body if it's not obvious from the diff.

## Review

Performance-sensitive changes (anything in a per-cycle or per-flit path)
should come with benchmark numbers; we don't take perf claims on faith.
Changes to a plugin trait after Phase 2 need an explicit breaking-change
discussion, not a rubber-stamp review.

## Bugs and feature requests

Open an issue. Say what you expected, what happened instead, and how to
reproduce it. Mention the phase if you know it.

## License

Contributions are licensed under the same dual MIT/Apache-2.0 terms as the
rest of the project.
