# Testing and coverage

The test suite covers the Rust shell engine, its JavaScript/WebAssembly
boundary, the browser-facing package, and the deployed demo artifact at
different layers.

## Automated test scope

- `cargo test --workspace` exercises the native Rust engine and CLI, including
  lexing, parsing, expressions, built-in commands, pipelines, rendering,
  cancellation, line editing, sessions, panes, and multiplexer layout.
- `just test-wasm` runs the `wasm-bindgen` boundary suite under Node. It checks
  Rust/JavaScript value conversion, registered JavaScript commands and regexes,
  progressive output, errors, cancellation, variables, and the public `run()`
  result shape.
- `just typecheck` builds the WebAssembly package and type-checks the vanilla
  TypeScript demo against the generated declarations.
- `just test-e2e` runs Playwright against the demo in Chromium. It exercises
  structured pipelines, selectors, diagnostics and escape handling, panel and
  session behavior, streaming, cancellation, and host/session variables.
- `scripts/verify-site.mjs` boots the assembled GitHub Pages artifact, while
  `scripts/verify-tarball.mjs` installs and boots the packed npm artifact as an
  external consumer would.

The manual browser checklist lives in
[`packages/demo/TESTING.md`](../packages/demo/TESTING.md).

## Coverage status

The project does not currently publish a numeric line or branch coverage
percentage. CI reports the pass/fail result for each boundary above, so this
document records what is exercised without claiming an unmeasured percentage.
Adding a coverage collector and a stable baseline remains separate work; until
then, changes should add focused tests in the layer whose behavior they alter.

## Local validation

Run the repository's standard validation before opening a pull request:

```sh
cargo fmt --all --check
cargo clippy --all-targets --all-features
cargo test --all-features
```

For the WebAssembly and browser boundaries, install the development
prerequisites from the root README and run `just test-wasm` and
`just test-e2e`.
