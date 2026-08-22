[![CI](https://github.com/tclarke/rcal/actions/workflows/ci.yml/badge.svg)](https://github.com/tclarke/rcal/actions/workflows/ci.yml)
[![CodeQL](https://github.com/tclarke/rcal/actions/workflows/github-code-scanning/codeql/badge.svg)](https://github.com/tclarke/rcal/actions/workflows/github-code-scanning/codeql)

# rcal_macros

Procedural macros for the [rcal](../) OMS Critical Abstraction Layer library.

## Macros

- `#[rcal_main]` — entry-point macro that initialises the Tokio runtime, builds the CAL config from
  `CALConfig.toml`, and wires up the root `slog` logger before calling `async fn main`.

## Usage

This crate is a dependency of `rcal` and is re-exported; you typically don't add it directly.
If you need the macros standalone:

```toml
[dependencies]
rcal_macros = { git = "https://github.com/tclarke/rcal", package = "rcal_macros" }
```
