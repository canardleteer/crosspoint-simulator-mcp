# AGENTS.md

This file is a README for agents working under `protos/`: layout, lint,
and breaking policy. Choosing buffa and how this repository generates
Rust bindings live in the root [`AGENTS.md`](../AGENTS.md).

## Layout

This directory is the Buf workspace (`buf.yaml`). Packages live under
`<org>/<area>/<surface>/<version>/`, matching
`crosspoint.sim.control.v1alpha1` and later siblings.

`crosspoint/sim/control/v1alpha1/session.proto` holds
`SimulatorControlService`, the bidirectional `Session` RPC, and the
`SimToServer` / `ServerToSim` envelopes. Payload messages and enums live
in `simulator_control.proto`. Put later unary RPCs in a file other than
`session.proto` so they stay under request/response name lint.

## Lint and comments

From this directory, run `buf lint`. The workspace uses `STANDARD` plus
`COMMENTS`.

Every service, RPC, message, field, oneof, enum, and enum value needs a
non-empty **leading** comment. Trailing `//` on the same line does not
count. When you change a field's meaning, default, or allowed values,
update that comment in the same change. Stale comments fail the intent of
`COMMENTS` even if `buf lint` still passes.

Service names use the `Service` suffix. `RPC_REQUEST_STANDARD_NAME` and
`RPC_RESPONSE_STANDARD_NAME` are ignored only for the bidirectional
Session envelopes in `session.proto`. Unary RPCs must use `MethodRequest`
/ `MethodResponse`.

## Breaking changes

`v1alpha1` is not locked, which is why breaking checks ignore unstable
packages on that module only. Flip the module's
`ignore_unstable_packages` to start enforcing `v1alpha1`, and add a
sibling module without the flag for `v1alpha2`. At least one other
software package consumes this IDL, so rebuild those packages when making
a breaking change.

## Generate

This directory is the Buf workspace. Run `buf lint` and `buf generate`
from here. Do not check in generated stubs as the source of truth; the
`.proto` files are. How this repository invokes Buf and which plugin
emits Rust is in the root [`AGENTS.md`](../AGENTS.md) under
**Generating protobuf bindings**.

## Agent Documentation Standards

Maintain this file according to the [AGENTS.md standard](https://agents.md/),
and keep it portable across compatible agent clients, without assumptions
about user-specific paths or session state.
