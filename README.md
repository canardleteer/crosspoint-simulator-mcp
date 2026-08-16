# crosspoint-simulator-mcp

`crosspoint-simulator-mcp-proxy` is an MCP server. A host attaches over
stdio (default) or Streamable HTTP. The same process listens for inbound
simulator `Session` streams over gRPC (plaintext). A session appears when
a simulator dials in. Default Session address is `127.0.0.1:50051`.

On stdio, stdout is JSON-RPC only. Use stderr for logs. `--mcp-http`
selects Streamable HTTP at `/mcp` instead of stdio.

Inject tools and `request_snapshot` wait on the session by default
(`InputAck`, or `SnapshotFrame` / `SnapshotError` as MCP image content).
Pass `wait: false` to enqueue only. `observe` honors the last
`SetSessionView` mask (SimToServer payload names). Tools that target a
session still require an instance id unless `--default-instance` is set.

| Flag | Env | Role |
| --- | --- | --- |
| `--listen` | `CSM_LISTEN` | gRPC Session listen address (default `127.0.0.1:50051`) |
| `--mcp-http` | `CSM_MCP_HTTP` | Streamable HTTP MCP listen address (default `127.0.0.1:8765` when the flag is present with no value) |
| `--default-instance` | `CSM_DEFAULT_INSTANCE` | Explicit instance id (1–64 bytes) a tool may use when it does not pass one |
| `--simulator` | `CSM_SIMULATOR` | Path of a known simulator for a later spawn (not executed) |
| `--simulator-arg` | `CSM_SIMULATOR_ARGS` | Extra argv for that later spawn (repeatable; env is comma-separated; not executed) |

```bash
# Host-launched MCP (stdio) plus Session listener
cargo run -p csm-proxy -- --listen 127.0.0.1:50051

# Streamable HTTP MCP at http://127.0.0.1:8765/mcp
cargo run -p csm-proxy -- --mcp-http
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
