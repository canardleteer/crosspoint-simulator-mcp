# crosspoint-simulator-mcp

`crosspoint-simulator-mcp-proxy` listens for inbound simulator `Session`
streams over gRPC (plaintext). Default listen address is
`127.0.0.1:50051`. Override with `--listen` or `CSM_LISTEN`.

```bash
cargo run -p csm-proxy -- --listen 127.0.0.1:50051
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
