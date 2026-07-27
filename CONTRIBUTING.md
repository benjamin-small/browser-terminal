# Contributing

Open an issue before substantial changes so scope and expected behavior are clear. Keep pull requests focused and include tests or documentation for changed behavior.

## Local setup

```sh
npm ci
```

## Validation

Run the checks that apply before opening a pull request:

```sh
cargo fmt --all --check
cargo clippy --all-targets --all-features
cargo test --all-features
```
