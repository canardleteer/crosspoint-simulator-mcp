---
number: 1
title: Expose bidirectional simulator control to MCP through an RPC listener
date: 2026-08-16
status: proposed
tags:
- mcp
- rpc
- architecture
links:
- target: 2
  kind: relatesto
- target: 4
  kind: relatesto
- target: 5
  kind: relatesto
---

# 1. Expose bidirectional simulator control to MCP through an RPC listener

Date: 2026-08-16

## Status

Proposed

## Context

We want agents, through a host application, to control and observe a running eBook firmware simulator that has a UI, as a human at the device would. We also want an agent to observe a person using that simulator, so a session can be used for rapid debugging of human-driven interaction, not only remote control. The simulator should not speak MCP.

MCP is a client-host-server protocol. Hosts are LLM applications that initiate connections. Clients live in the host and each client talks to exactly one server. Servers expose tools, resources, and prompts. Standard transports are stdio (the client launches the server as a subprocess) and Streamable HTTP (the client posts to a single MCP endpoint).

## Decision

Build an RPC listener so an eBook firmware simulator (with UI) can be bi-directionally controlled by MCP, and so an agent can observe a person using that simulator. Our MCP server should be able to work via stdio or Streamable HTTP.

## Consequences

This repository is an MCP server. A host application creates an MCP client that connects to it, one client per server.

On stdio, that client launches this process as a subprocess. On Streamable HTTP, that client posts to this process's MCP endpoint.

The eBook firmware simulator is not an MCP peer. It talks RPC to this process. The same session can carry remote injects and reports of a person using the UI. MCP tools, resources, and prompts stay here.
