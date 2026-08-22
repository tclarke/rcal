[![CI](https://github.com/tclarke/rcal/actions/workflows/ci.yml/badge.svg)](https://github.com/tclarke/rcal/actions/workflows/ci.yml)

# simple_two_service

Example application demonstrating two rcal services communicating over ZMQ.

Shows:

- `AbstractService` activate/deactivate lifecycle
- Periodic `SystemStatus` and `ServiceStatus` broadcasting
- `ServiceStatusDataRequest` / `ServiceStatusDataRequestStatus` request-response pattern

## Running

Open two terminals in the `examples/simple_two_service/` directory and run one command in each:

```sh
cargo run -- TestService1 3
cargo run -- TestService2 3
```

`TestService1` binds on port 2000 and peers to `TestService2` on port 2001. Each sends
`3` `ServiceStatusDataRequest` messages (configurable via the second argument) then idles
until interrupted with Ctrl-C.

A `CALConfig.toml` and `ExampleConfig.toml` are included with ready-to-run transport settings.
