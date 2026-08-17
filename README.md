# crosspoint-simulator-mcp

`crosspoint-simulator-mcp-proxy` is an MCP server. A host attaches over
stdio (default) or Streamable HTTP. The same process listens for inbound
simulator `Session` streams over gRPC (plaintext). A session appears when
a simulator dials in, or when `start_instance` launches `--simulator`.
Default Session address is `127.0.0.1:50051`. Clients discover tools and
spawn limits from initialize instructions, `tools/list`, and
`csm://capabilities`.

On stdio, stdout is JSON-RPC only. Logs go to stderr via `tracing`.
`RUST_LOG` selects the filter; when it is unset, the default is
`csm_proxy=info`. Spawned simulator children still inherit stderr, so
`[SIM]` / firmware serial from the subprocess can appear next to proxy
lines. `--mcp-http` selects Streamable HTTP at `/mcp` instead of stdio.

Inject tools and `request_snapshot` wait on the session by default
(`InputAck`, or `SnapshotFrame` / `SnapshotError` as MCP image content).
Pass `wait: false` to enqueue only. `observe` honors the last
`SetSessionView` mask (SimToServer payload names). It can wait with
`until_log` and/or `until_generation_gt` until any condition matches
or `wait_ms` elapses. Tools that target a
session still require an instance id unless `--default-instance` is set.
`start_instance` defaults to copying the committed sample EPUB
(`CrossPoint-Reader.epub`, from the CrossPoint Reader README) into
`fs_/books/`. Pass `sample_book: false` for an empty library.
`auto_sleep` defaults to false and seeds never-sleep firmware settings
(`sleepTimeoutMinutes` 31); pass true to keep the 10-minute idle sleep.

| Flag | Env | Role |
| --- | --- | --- |
| `--listen` | `CSM_LISTEN` | gRPC Session listen address (default `127.0.0.1:50051`) |
| `--mcp-http` | `CSM_MCP_HTTP` | Streamable HTTP MCP listen address (default `127.0.0.1:8765` when the flag is present with no value) |
| `--default-instance` | `CSM_DEFAULT_INSTANCE` | Explicit instance id (1–64 bytes) a tool may use when it does not pass one |
| `--simulator` | `CSM_SIMULATOR` | Prebuilt simulator binary executed only by `start_instance` |
| `--simulator-arg` | `CSM_SIMULATOR_ARGS` | Extra argv for `start_instance` (repeatable; env is comma-separated) |
| `--auto-sleep` | `CSM_AUTO_SLEEP` | Process default for `start_instance.auto_sleep` (default false = never-sleep settings) |
| `--observe-wait-ms` | `CSM_OBSERVE_WAIT_MS` | Default `observe` timeout when an until-condition is set and `wait_ms` is omitted (default 8000) |
| | `RUST_LOG` | `tracing` filter (default `csm_proxy=info` when unset) |

The `crosspoint-simulator` submodule is a **library** the consuming
firmware links. It is not a `program` this server can exec. `start_instance`
needs a firmware binary built with `-DCROSSPOINT_SIM_GRPC` against protobuf
35 / grpc++ 1.83. The README `cargo run` lines below only start the MCP
and Session listeners; they do not build firmware or set `--simulator`.

Local Session testing (HTTP MCP, `--simulator` when `--firmware` names a
firmware tree or `program`) is:

```bash
# Build csm-proxy if needed and exec the test proxy
cargo xtask start-csm-proxy

# Operator-local display, grpc++ libs, and firmware (not stored in git)
cargo xtask start-csm-proxy \
  --firmware /path/to/firmware \
  --display "$DISPLAY" \
  --ld-library-path /path/to/grpc/lib

# Host-launched stdio MCP: xtask builds, then execs the proxy so cargo
# never owns stdout. JSON-RPC stays on stdout; logs stay on stderr.
cargo xtask start-csm-proxy --mode=stdio
```

`--mode` is `http` (default, `http://127.0.0.1:8765/mcp`) or `stdio`.
`--firmware`, `--display` / `CSM_DISPLAY`, and `--ld-library-path` /
`CSM_LD_LIBRARY_PATH` are operator-local. They apply only to the exec'd
proxy, have no implied machine values, and must not be committed.
`--grpc-prefix` is only for `pio` `PKG_CONFIG_PATH` when this task builds
firmware. Pass extra proxy flags after `--` (for example `-- --auto-sleep`).

Inbound-only (no firmware, no `start_instance`):

```bash
# Host-launched MCP (stdio) plus Session listener
cargo run -p csm-proxy -- --listen 127.0.0.1:50051

# Streamable HTTP MCP at http://127.0.0.1:8765/mcp
cargo run -p csm-proxy -- --mcp-http

# More proxy logs (still stderr)
RUST_LOG=csm_proxy=debug cargo run -p csm-proxy -- --mcp-http
```

## Cloning this repository

This repository includes the `crosspoint-simulator` git submodule. Clone with
`--recurse-submodules` so that checkout is populated:

```bash
git clone --recurse-submodules https://github.com/canardleteer/crosspoint-simulator-mcp.git
```

If you already cloned without that flag, initialize and fetch the submodule
from inside the clone:

```bash
git submodule update --init --recursive
```
