# AGENTS.md

This file is a README for agents: the extra context that helps coding agents
work in this repository.

`csm` abbreviates **crosspoint-simulator-mcp**. Rust packages live under
`crates/<crate-name>` and use that prefix (`csm-proxy`, `csm-pb-bindings`).

## What this repo is

An MCP server that lets a host application control and observe an eBook
firmware simulator (with UI), and observe a person using that simulator.
The simulator is not an MCP peer. The first integration path is an
already-running simulator that dials in; spawning a known simulator
remains allowed and is not implemented yet.

Instance ids are required and cannot be empty: `Register.instance_id`
and every later selector must be 1–64 bytes. Do not omit an id when
exactly one simulator is connected, and do not infer a target from
connection count. MCP tools that target a session name an id, or use
the process default from `--default-instance` / `CSM_DEFAULT_INSTANCE`
when that flag is a valid id. Inbound `Session` is how a simulator
appears today. `--simulator` and `--simulator-arg` are reserved spawn
hints and must not start a process.

This process is the MCP peer (`rmcp`). Default MCP transport is stdio
(JSON-RPC only on stdout; logs go to stderr). `--mcp-http` /
`CSM_MCP_HTTP` selects Streamable HTTP instead, mounted at `/mcp`.
The gRPC `Session` listener stays up either way. The simulator is not
an MCP peer.

## Simulator submodule

Initial development includes a good target simulator as the
`crosspoint-simulator` git submodule, so a prototype RPC API can be
integrated rapidly. This is a development-time checkout, not a claim that
this repository is the simulator's home.

When working on simulator behaviour, HAL stubs, host platforms, or anything
under `crosspoint-simulator/`, read that checkout's
[`crosspoint-simulator/AGENTS.md`](crosspoint-simulator/AGENTS.md). That file
is the source of truth for the simulator's architecture, faithfulness limits,
and change rules.

The submodule tracks
`https://github.com/canardleteer/crosspoint-simulator` on branch
`dev/canardleteer/agent-docs-host-platforms`. After clone, initialize it with
`git submodule update --init --recursive`.

## Rust crates and CLI

Workspace members are under `crates/`. `csm-proxy` is the
`crosspoint-simulator-mcp-proxy` binary. `csm-pb-bindings` builds and
exposes the buffa message types and connectrpc `Session` stubs for this
repository's IDL.

Use [clap](https://docs.rs/clap) with clap's derive API (`Parser`,
`Args`, `Subcommand`, and related derives) for Rust CLI patterns. Do not
parse `std::env::args` by hand when clap can express the interface.

Workspace maintenance lives in the `xtask` crate. Run it as
`cargo xtask <command>` (see `.cargo/config.toml`).

## Coverage

Line coverage for this repository's own Rust must stay **over 90%**.
That measurement excludes generated bindings under `src/gen` and
`src/gen_connect`, and excludes the `xtask` crate. Check with:

```bash
cargo xtask coverage
```

`cargo xtask coverage --html` also writes an HTML report under
`target/llvm-cov/html`. `cargo xtask coverage --open` writes that
report and opens it in the default browser.

If coverage tools are missing, the xtask prints `cargo install` (and
`rustup component add`) commands. Do not land a change that drops line
coverage to 90% or below.

## MCP and RMCP

Use [rmcp](https://crates.io/crates/rmcp) for MCP support in this Rust
codebase. Do not introduce another MCP Rust stack unless a change
explicitly requires it.

When answering or implementing MCP protocol behavior, look for a
`model-context-protocol-reference` skill, or a similarly named MCP
specification skill. If one is present, use it. If none is present,
confirm MCP facts from
[the protocol repository](https://github.com/modelcontextprotocol/modelcontextprotocol)
or [modelcontextprotocol.io](https://modelcontextprotocol.io). Do not
treat memory of the protocol as sufficient for normative claims.

## Protobuf, buffa, and RPC

Protobuf is our IDL. Use the [buffa](https://github.com/anthropics/buffa)
ecosystem for generated message types and the runtime. Buffa encodes and
decodes protobuf; it is not an RPC server and does not emit service
stubs. Do not introduce prost, protobuf-rs, or another protobuf runtime
unless a change explicitly requires it.

The first `Session` binding is gRPC. The eBook firmware simulator talks
gRPC to this process.

The RPC server stack is [connectrpc](https://crates.io/crates/connectrpc)
(connect-rust). One handler can speak gRPC, Connect, and gRPC-Web.
Simulator clients use **gRPC**. Connect is available because the stack
speaks it, not because a client must. Do not introduce tonic unless
connectrpc cannot host this bidirectional stream.

Which transports actually carry `Session`:

- **gRPC** — the first binding; this is what a simulator client dials
- **Connect** — same handler; not required and not the simulator hop
- **gRPC-Web / browsers** — not a `Session` path. Bidirectional streams
  are poorly supported; a browser would need an adapter, not this stream
  as-is

Do not introduce prost as our protobuf runtime. connectrpc may pull
other HTTP crates; that does not change the IDL runtime.

For proto layout, lint (including `COMMENTS`), and breaking policy, read
[`protos/AGENTS.md`](protos/AGENTS.md).

## Generating protobuf bindings

Generate Rust with Buf, not `buffa-build`, `connectrpc-build`, or a
system `protoc`. Resolve the `buf` binary through the
[`buf-tools`](https://crates.io/crates/buf-tools) crate
(`buf_tools::buf_bin_path()`), then run `buf generate`. Do not assume
`buf` is installed on `PATH`. The first `buf-tools` build downloads and
verifies official Buf release binaries.

Follow buffa's recommended
[`buf generate` pattern](https://github.com/anthropics/buffa#using-buf-generate-recommended)
for **messages**, and a second remote plugin for **service stubs**:

- `buf.gen.yaml` version `v2` in `crates/csm-pb-bindings`
- `buf.build/anthropics/buffa` → `out: src/gen` (no local
  `protoc-gen-buffa` install)
- plugin opts `file_per_package=true` and `json=true`
- one `<dotted.package>.rs` file per proto package
- a hand-written `src/gen/mod.rs` that `include!`s those files into a
  `pub mod` tree, exposed as `csm_pb_bindings::generated` (`gen` is a
  reserved keyword in edition 2024)
- `buf.build/connectrpc/rust` → `out: src/gen_connect` (separate `out`
  so filenames do not collide)
- plugin opts `file_per_package` and `buffa_module=crate::generated`
- a hand-written `src/gen_connect/mod.rs`, exposed as
  `csm_pb_bindings::rpc`

`csm-pb-bindings`'s `build.rs` runs `buf generate` via `buf-tools`. When
pinning a remote plugin, match it to the corresponding crate minor in
the workspace `Cargo.toml` (`buffa` for the buffa plugin, `connectrpc`
for the connect-rust plugin). connectrpc 0.8 depends on buffa 0.8.x, so
this workspace stays on that buffa minor until connectrpc adopts 0.9.

Workspace-level generate rules (where to run Buf, what not to vendor)
are in [`protos/AGENTS.md`](protos/AGENTS.md).

## Commits

Prefer [Conventional Commits](https://www.conventionalcommits.org/):
`type(optional-scope): description`.

Use `feat`, `fix`, `docs`, `chore`, `refactor`, `test`, `build`, or
`ci`. `csm-proxy` and `csm-pb-bindings` are fine scopes. Keep the
subject imperative and focused on why the change exists.

## Agent Documentation Standards

Maintain this file according to the [AGENTS.md standard](https://agents.md/),
and keep it portable across compatible agent clients, without assumptions
about user-specific paths or session state.
