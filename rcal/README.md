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
rcal = { git = "https://github.com/tclarke/rcal" }
```

By default rcal generates Rust types from the bundled UCI 2.5.0 XSD at build time. To use a custom schema,
set `RCAL_SCHEMA_PATH` to your `.xsd` file path (in the environment or in `.cargo/config.toml`).

See the [`examples/`](examples/) directory for usage patterns.

## Crates in this workspace

| Crate | Description |
|-------|-------------|
| `rcal` | This crate — core CAL library |
| [`rcal_macros`](rcal_macros/) | Procedural macros (`#[rcal_main]`, etc.) |
| [`rcal-xsd-subset`](xsd-subset/) | Dev tool for subsetting UCI XSD files |
| [`simple_two_service`](examples/simple_two_service/) | Two-service example application |
