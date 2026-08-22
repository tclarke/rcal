[![CI](https://github.com/tclarke/rcal/actions/workflows/ci.yml/badge.svg)](https://github.com/tclarke/rcal/actions/workflows/ci.yml)
[![CodeQL](https://github.com/tclarke/rcal/actions/workflows/github-code-scanning/codeql/badge.svg)](https://github.com/tclarke/rcal/actions/workflows/github-code-scanning/codeql)
[![DevSkim](https://github.com/tclarke/rcal/actions/workflows/devskim.yml/badge.svg)](https://github.com/tclarke/rcal/actions/workflows/devskim.yml)

# rcal

OMS Critical Abstraction Layer (CAL) implementation for Rust. Implements the CERT CAL- requirements with
inspiration from the CERT CXX- requirements, using idiomatic Rust where it improves on the C++ design.

## Features

- `service` (default) — `AbstractService` lifecycle management and UCI message pub/sub
- `zmq` (default) — ZMQ-based Abstract Service Bus (`ZmqAsb`) via `omq-tokio`

## Quickstart

Add to `Cargo.toml`:

```toml
[dependencies]
rcal = "1"
```

See the [`examples/`](examples/) directory for usage patterns.

## Build-time configuration

rcal generates Rust UCI message types from an XSD schema at build time. Control this via environment
variables (set in the shell or in `.cargo/config.toml`):

| Variable | Purpose |
|----------|---------|
| `RCAL_CALCONFIG_PATH` | Path to a `CALConfig.toml`. rcal reads the declared topics and generates **only** the UCI types your services actually use, shrinking compile times and binary size. |
| `RCAL_CALCONFIG_SERVICES` | Comma-separated service names within `RCAL_CALCONFIG_PATH` to further restrict which topics are included. |
| `RCAL_XSD_PATH` | Path to a custom or subsetted XSD file. Use when you need non-standard types or have pre-subsetted the schema with `rcal-xsd-subset`. Defaults to the bundled UCI 2.5.0 schema. |
| `RCAL_SCHEMA_VERSION` | Override the version string embedded in generated code (defaults to the `version=` attribute in the XSD). |

Example `.cargo/config.toml`:

```toml
[env]
RCAL_CALCONFIG_PATH = "/path/to/CALConfig.toml"
```

## Crates in this workspace

| Crate | Description |
|-------|-------------|
| `rcal` | This crate — core CAL library |
| [`rcal_macros`](rcal_macros/) | Procedural macros (`#[rcal_main]`, etc.) |
| [`rcal-xsd-subset`](xsd-subset/) | Dev tool for subsetting UCI XSD files |
| [`simple_two_service`](examples/simple_two_service/) | Two-service example application |
