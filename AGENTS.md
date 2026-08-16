# AGENTS.md

This file is a README for agents: the extra context that helps coding agents
work in this repository.

## What this repo is

An MCP server that lets a host application control and observe an eBook
firmware simulator (with UI), and observe a person using that simulator.
The simulator is not an MCP peer. The first integration path is an
already-running simulator that dials in; spawning a known simulator
remains allowed and is not the first path.

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

## Protobuf, buffa, and Buf

Protobuf is our IDL. This crate uses the
[buffa](https://github.com/anthropics/buffa) ecosystem for generated types
and the runtime. Do not introduce prost, protobuf-rs, or another protobuf
runtime unless a change explicitly requires it.

The first RPC listener binding is gRPC. The eBook firmware simulator talks
gRPC to this process.

ConnectRPC is not excluded, and it is not implemented yet. Using buffa and
Buf does not exclude ConnectRPC or other transports and serialization the
IDL can emit.

Generate Rust from `.proto` files with Buf, not `buffa-build` or a
system `protoc`. Resolve the `buf` binary through the
[`buf-tools`](https://crates.io/crates/buf-tools) crate
(`buf_tools::buf_bin_path()`), then run `buf generate`. Do not assume `buf`
is installed on `PATH`. The first `buf-tools` build downloads and verifies
official Buf release binaries.

Follow buffa's recommended [`buf generate` pattern](https://github.com/anthropics/buffa#using-buf-generate-recommended):

- `buf.gen.yaml` version `v2` with the remote plugin
  `buf.build/anthropics/buffa` (no local `protoc-gen-buffa` install)
- `out: src/gen`
- plugin opts `file_per_package=true` and `json=true`
- one `<dotted.package>.rs` file per proto package
- a hand-written `src/gen/mod.rs` that `include!`s those files into a
  `pub mod` tree

When pinning the remote plugin, match it to the `buffa` crate version in
`Cargo.toml`.

## Agent Documentation Standards

Maintain this file according to the [AGENTS.md standard](https://agents.md/),
and keep it portable across compatible agent clients, without assumptions
about user-specific paths or session state.
