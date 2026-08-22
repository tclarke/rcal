[![CI](https://github.com/tclarke/rcal/actions/workflows/ci.yml/badge.svg)](https://github.com/tclarke/rcal/actions/workflows/ci.yml)

# rcal-xsd-subset

Developer tool for creating trimmed subsets of UCI XSD schema files. Not part of the `rcal` library
build pipeline and not published to crates.io.

## Purpose

UCI schema files can be large. This tool strips unused types and elements so that `rcal`'s build script
generates only the Rust types your application actually needs, reducing compile times and binary size.

## Usage

```sh
cargo run --manifest-path rcal/xsd-subset/Cargo.toml -- <input.xsd> <output.xsd> [types...]
```

Set `RCAL_SCHEMA_PATH` to the output file path when building `rcal`:

```toml
# .cargo/config.toml
[env]
RCAL_SCHEMA_PATH = "/path/to/output.xsd"
```
