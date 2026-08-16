---
number: 4
title: Support multiple connected simulator instances
date: 2026-08-16
status: accepted
tags:
- process-model
- architecture
links:
- target: 1
  kind: relatesto
---

# 4. Support multiple connected simulator instances

Date: 2026-08-16

## Status

Proposed

## Context

Operators may run more than one eBook firmware simulator. This server needs a rule for how those connections coexist.

## Decision

This server may have more than one simulator instance connected at a time. Each instance is identified when it registers.

## Consequences

When more than one simulator is connected, MCP tools must be able to select an instance. A single-instance workflow is the simple case, not a limit.
