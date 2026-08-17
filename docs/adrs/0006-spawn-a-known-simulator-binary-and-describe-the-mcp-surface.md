---
number: 6
title: Spawn a known simulator binary and describe the MCP surface
date: 2026-08-16
status: proposed
tags:
- mcp
- process-model
- architecture
links:
- target: 5
  kind: relatesto
---

# 6. Spawn a known simulator binary and describe the MCP surface

Date: 2026-08-16

## Status

Proposed

## Context

A host can already attach to a simulator that dials this process. Operators also want an MCP client to start a Session-capable firmware binary that this server already knows, without turning the proxy into a firmware build system. MCP clients need to discover that surface from the protocol itself.

## Decision

When `--simulator` is set, `start_instance` executes that known prebuilt binary with Session argv this server controls. The MCP client does not pass a binary path, free-form argv, PlatformIO flags, or board macros. Board and firmware compile options stay in the binary; `Register` reports them after connect. `start_instance` may seed a committed sample EPUB (the CrossPoint Reader README) into the instance `fs_/books/` tree; the client only passes a boolean, not a book path or bytes. Clients discover the surface through initialize instructions, `tools/list` schemas, and the `csm://capabilities` resource. Inbound dial remains valid. This server does not auto-start a simulator on boot.

## Consequences

An MCP client can start a headless instance, optionally with a known sample book, then inject, snapshot, and shut it down when the operator configured a binary. A client that only speaks MCP can learn required instance ids, spawn limits, the sample-book flag, and tools without reading this repository. Firmware rebuilds, board selection, and arbitrary book uploads stay outside this process.
