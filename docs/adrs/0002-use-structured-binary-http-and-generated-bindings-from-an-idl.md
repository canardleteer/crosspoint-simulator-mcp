---
number: 2
title: Use structured binary HTTP and generated bindings from an IDL
date: 2026-08-16
status: accepted
tags:
- rpc
- idl
- architecture
links:
- target: 1
  kind: relatesto
- target: 3
  kind: relatesto
---

# 2. Use structured binary HTTP and generated bindings from an IDL

Date: 2026-08-16

## Status

Proposed

## Context

This MCP server and the eBook firmware simulator need a shared contract that can be implemented in more than one language and carried over HTTP without inventing a one-off encoding.

## Decision

Use structured binary HTTP transports, with a deterministic, modern, and easy-to-use code generation path from an IDL.

## Consequences

The contract between this MCP server and the eBook firmware simulator is the IDL. Bindings for each side are generated from it. Which generator we use is a tooling choice, not this decision.
