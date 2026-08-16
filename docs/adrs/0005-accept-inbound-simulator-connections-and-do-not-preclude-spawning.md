---
number: 5
title: Accept inbound simulator connections and do not preclude spawning
date: 2026-08-16
status: proposed
tags:
- process-model
- architecture
links:
- target: 1
  kind: relatesto
---

# 5. Accept inbound simulator connections and do not preclude spawning

Date: 2026-08-16

## Status

Proposed

## Context

An eBook firmware simulator may already be running, or this server may know how to start one. Those are complementary ways to get a session.

## Decision

Support accepting inbound connections from the eBook firmware simulator, and do not preclude offering a mechanism to spawn a simulator when one is known.

## Consequences

MCP tools can work with a simulator that has connected, and with a simulator this server started, when it knows how. Session setup is not limited to one of those mechanisms.
