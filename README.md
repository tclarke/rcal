[![CI](https://github.com/tclarke/rcal/actions/workflows/ci.yml/badge.svg)](https://github.com/tclarke/rcal/actions/workflows/ci.yml)
[![CodeQL](https://github.com/tclarke/rcal/actions/workflows/github-code-scanning/codeql/badge.svg)](https://github.com/tclarke/rcal/actions/workflows/github-code-scanning/codeql)
[![DevSkim](https://github.com/tclarke/rcal/actions/workflows/devskim.yml/badge.svg)](https://github.com/tclarke/rcal/actions/workflows/devskim.yml)
[![Release](https://github.com/tclarke/rcal/actions/workflows/release.yml/badge.svg)](https://github.com/tclarke/rcal/actions/workflows/release.yml)

### `rcal` crate

[![Crates.io Version](https://img.shields.io/crates/v/rcal)](https://crates.io/crates/rcal)
[![docs.rs](https://img.shields.io/docsrs/rcal)](https://docs.rs/rcal)

### `rcal_macros` crate

[![Crates.io Version](https://img.shields.io/crates/v/rcal_macros)](https://crates.io/crates/rcal_macros)
[![docs.rs](https://img.shields.io/docsrs/rcal_macros)](https://docs.rs/rcal_macros)

# RCal: An OMS Cal implementation in rust
rcal is an Open Mission Systems (OMS) Critical Abstraction Layer (CAL) implementation for Rust. It implements the
"CERT CAL-" requirements and take inspiration from the "CERT CXX-" requirements. The C++ requirements are an
inspiration for the rust API and are used when they make sense while taking advantage of rust features.
RCal currently utilizes the 2020 edition and builds with stable.

# AI notes
One goal of developing this library was to experiment with Claude Code. I'd describe the AI contributions
as "collaborative vibe coding". I've given requirements, reviewed all code, and added some code as it makes
sense but Claude is treated like a junior engineer and performs most of the tasks with input from me.
I also make extensive use of Claude's ability to understand large and diverse documents. I've extracted
requirements from the PDFs, researched the best external crates to use, etc. I've also had claude do a lot
of the design work in Planning mode. I'll often have to make some changes to the design before implementation.

If you want to use claude code with this codebase I'd suggest at least the setup below.

## Quickstart
Just add as a dependency and build. This will generate from the UCI 2.5.0 schema. If you have a custom schema
that removes unneeded types and elements or adds non-standard types and elements, set the `RCAL_SCHEMA_PATH`
environment variable when you build. You can set this in your environment or create a `.cargo/config.toml`
file with:
```toml
[env]
RCAL_SCHEMA_VERSION="/path/to/schema.xsd"
````

The `RCAL_SCHEMA_VERSION` will be set to the basename of the schema file. If you want a different version
string, you can also set this in `config.toml`.

See the `examples/` folder for usage.

# Setting up claude
- install rtk  -- This drastically reduces token use by stripping unnecessary output from external commands
- install ripgrep  -- Required for some of the plugin capabilities
- Install caveman  -- Cavement automatically has Claude use "caveman" speach with is highly compressed and functional. It removes pleasantries, fluff, etc. and produces output that has high signal to noise
  - `claude plugin marketplace add JuliusBrussee/caveman`
  - `claude plugin install caveman@caveman`
- Install PonyTail -- Ponytail is a code auditing plugin which looks for unnecessary implementation layers and provides suggestions on how to minimize code changes and still meet requirements.
  - `claude plugin marketplace add DietrichGebert/ponytail`
  - `claude plugin install ponytail@ponytail`
- Optional: Install and configure OmniRoute -- OmniRoute allows you to send simple tasks (such as basic refactoring, creating unit tests, etc.) to a different model (often a free model) and only sending the more complex stuff to Claude.
  - `npm install -g omniroute`
  - `omniroute server`
- Optional: Install skills -- This skill provides information on common rust async patterns. Since RCal heavily uses async this can get a working implementation with less back and forth discussion
  - `npx skills add https://github.com/wshobson/agents --skill rust-async-patterns`

### MCP Servers
- rust-mcp-server: Rust cargo and rust commands with MCP instead of bash
  - cargo install rust-mcp-server
  - cargo install cargo-machete
  - cargo install cargo-deny
  - `claude mcp add --scope user rust-mcp-server -- ~/.cargo/bin/rust-mcp-server`
- filesystem: Perform filesystem tasks (read, write, cd, list directory, find, etc.) with MCP instead of bash commands
  - `claude mcp add filesystem -s user  -- npx -y @modelcontextprotocol/server-filesystem ~/rcal`

## Release Process

1. Bump version in rcal/Cargo.toml [workspace.package]
  (single edit propagates to all three crates via inheritance)
2. Commit: "chore: bump version to vX.Y.Z"
3. Close the milestone on GitHub
4. git tag vX.Y.Z && git push origin vX.Y.Z
5. Workflow fires — tests, publishes, creates GitHub Release
