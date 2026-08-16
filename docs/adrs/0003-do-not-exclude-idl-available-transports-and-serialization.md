---
number: 3
title: Do not exclude IDL-available transports and serialization
date: 2026-08-16
status: accepted
tags:
- rpc
- idl
- architecture
links:
- target: 2
  kind: relatesto
---

# 3. Do not exclude IDL-available transports and serialization

Date: 2026-08-16

## Status

Proposed

## Context

An IDL can usually emit more than one transport and serialization. Choosing one binding for the first listener should not close the others.

## Decision

Avoid excluding additional transport and serialization mechanisms that are available from our IDL.

## Consequences

A first listener may use one binding. Other transports and serialization the IDL can emit stay in scope. Adding a binding later should not require a new contract.
