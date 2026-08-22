# Phase 3 — Herdr Context and Read-only Inspection

## Objective

Implement fail-closed Herdr detection, exact binary selection, bounded CLI
transport, live caller resolution, session snapshot parsing, and pane process
inspection. This phase may observe Herdr but must not change focus or create any
resource.

## Prerequisites

- [x] [Agent] Phase 2 gate has passed.
- [x] [Agent] Re-read system-design sections 5, 8–10, 14–15, and 17–18.
- [x] [Agent] Verify the official Herdr 0.8.2 CLI and schema fields used by the
      implementation before coding the transport types.

## Work items

### 1. Caller context and executable validation

- [x] [Agent] Read caller environment values through `EngineInterface`, not
      the plugin process's stale environment.
- [x] [Agent] Distinguish absent `HERDR_ENV`, exact string `1`, and malformed
      present values exactly as specified.
- [x] [Agent] Require the complete Herdr context, including injected IDs,
      `HERDR_SOCKET_PATH`, and `HERDR_BIN_PATH`.
- [x] [Agent] Canonicalize the injected binary, allow a symlink, and require an
      absolute regular executable target.
- [x] [Agent] Never search `PATH` for a Herdr binary.

### 2. Bounded synchronous CLI runner

- [x] [Agent] Execute the exact binary with separate argv values and no shell.
- [x] [Agent] Refresh caller `HERDR_*` variables for every child invocation,
      explicitly pass `HERDR_SOCKET_PATH`, and remove `HERDR_SESSION`.
- [x] [Agent] Enforce the 2-second read-operation timeout, 4 MiB response cap,
      process termination/reaping, and interruption hook.
- [x] [Agent] Sanitize bounded error details without dumping environments or
      complete responses; the local Herdr socket path may appear.

### 3. Typed inspection operations

- [x] [Agent] Implement `herdr api snapshot` and validate server version,
      protocol metadata, required result kinds, and required fields while ignoring
      unknown fields.
- [x] [Agent] Implement `herdr pane current --current`; use its live IDs rather
      than launch-time environment IDs.
- [x] [Agent] Require the live caller to exist in the same snapshot and surface
      a typed stale-state result for later recomputation.
- [x] [Agent] Implement bounded `herdr pane process-info --pane <id>` only for
      exact-path shell candidates that need foreground proof.
- [x] [Agent] Map transport records into phase-2 domain types without leaking
      raw JSON models across the boundary.

### 4. Fake-CLI verification

- [x] [Agent] Verify exact executable selection, argv boundaries, and child
      environment handling with a fake executable.
- [x] [Agent] Cover valid responses, unknown fields, missing fields, wrong
      result kinds, mismatched IDs, nonzero exits, malformed JSON, oversized output,
      timeout, and interruption.
- [x] [Agent] Cover incomplete process evidence, `not_found`, and stale live
      caller/snapshot identity as distinct outcomes.

## User actions and confirmation

- [x] [User action] No live Herdr session is required for the automated suite.
- [x] [User confirmation] Review evidence that this phase is read-only, uses
      only the injected binary, and fails closed on ambiguous state.

## Phase gate

- [x] [Agent] All required read-only Herdr data maps into typed domain inputs.
- [x] [Agent] No focus, create, close, move, delete, or input action exists in
      this phase.
- [x] [Agent] Formatting, Clippy, and fake-CLI tests pass.
- [x] [User confirmation] The inspection boundary review is approved.

## Out of scope

- Exact pane focus and socket requests.
- Tab or workspace creation.
- Caller `$env.PWD` mutation.
- Full decision orchestration and the one-recomputation policy.
