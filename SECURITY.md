# Security

Tessera is a simulator that runs locally, so most problems are ordinary bugs and
belong in a normal issue. The exception is the WASM plugin sandbox: if you find
a way for a plugin to escape it, or for a config or trace file to make Tessera
read or write files it shouldn't, please don't open a public issue.

Instead, report it privately through
[GitHub's security advisory form](https://github.com/amin-norollah/Tessera/security/advisories/new).
I'll reply as soon as I can and credit you in the fix unless you'd rather stay
anonymous.

Only the latest `main` branch is supported. There are no releases yet.
