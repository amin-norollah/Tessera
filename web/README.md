# Tessera web UI

This is where Tessera's browser front-end will live: run replay, network
visualization (WebGPU), a config editor, and export of figures and data for
papers. It's planned for Phase 5 on the [roadmap](../guide/roadmap.md).

**Right now it's only an Angular 22 scaffold.** None of the UI has been built
yet, and nothing in the simulator depends on it.

## Running the scaffold

From the repository root (it's an npm workspace, so dependencies install there):

```sh
npm install
npm run web:start   # dev server at http://localhost:4200
npm run web:build   # production build in web/dist/
npm run web:test    # unit tests (Vitest)
```
