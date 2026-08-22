# Phase 4 — Herdr Focus and Creation Actions

## Objective

Implement the three approved Herdr mutations behind narrow typed interfaces:
exact pane focus, focused tab creation, and focused workspace creation. Verify
their security, timeout, response, and partial-failure behavior without wiring
the complete `hcd` decision loop.

## Prerequisites

- [x] [Agent] Phase 3 gate has passed.
- [x] [Agent] Re-read system-design sections 12 and 14–18.
- [x] [Agent] Verify `pane.focus` against the official Herdr 0.8.2 schema.

## Work items

### 1. Exact-pane focus transport

- [x] [Agent] Require an absolute existing Unix socket owned by the effective
      user and reject a regular file or foreign-owned socket.
- [x] [Agent] Open one synchronous connection per focus action and send one
      newline-delimited `pane.focus` request with a unique request ID and exact
      `pane_id`.
- [x] [Agent] Enforce the 2-second timeout, 4 MiB response cap, request-ID
      matching, typed success validation, Herdr error handling, and interruption.
- [x] [Agent] Close the socket after one response and never fall back to tab or
      workspace focus.

### 2. Creation operations

- [x] [Agent] Implement `tab create` with explicit workspace ID, canonical
      target cwd, `--focus`, and no label.
- [x] [Agent] Implement `workspace create` with canonical target cwd,
      `--focus`, and no label.
- [x] [Agent] Enforce the 5-second creation timeout and validate returned
      workspace, tab, and root-pane identities, including workspace and tab
      association on the created root pane. Missing or mismatched IDs are
      protocol errors.
- [x] [Agent] Do not change the calling pane's cwd during any Herdr action.

### 3. Failure and safety behavior

- [x] [Agent] Return typed `not_found`, timeout, transport, protocol, and Herdr
      action failures for phase-5 orchestration.
- [x] [Agent] Report that a create may have partially completed when success is
      ambiguous after dispatch.
- [x] [Agent] Never roll back by closing a pane, tab, or workspace.
- [x] [Agent] Confirm that no action path can move, delete, overwrite, close,
      or send input to an existing resource.

### 4. Transport verification

- [x] [Agent] Use a temporary fake Unix socket server to cover the exact focus
      request, response matching, errors, malformed JSON, size limit, timeout, and
      interruption.
- [x] [Agent] Use the fake CLI to verify exact create argv, focus flags, omitted
      labels, response validation, timeout, and ambiguous completion.

## User actions and confirmation

- [x] [User action] No live Herdr mutation is required by the automated tests.
- [x] [User confirmation] Review the exact-pane focus protocol, create argv,
      and evidence that failures never trigger destructive rollback.

## Phase gate

- [x] [Agent] All three Herdr actions are available through narrow typed APIs.
- [x] [Agent] Socket and CLI action tests pass without a live Herdr session.
- [x] [Agent] Formatting, Clippy, and tests pass.
- [x] [User confirmation] The action safety review is approved.

## Out of scope

- The complete inspect-decide-act loop.
- Updating Nushell `$env.PWD`.
- Retry/recomputation policy across inspection and action layers.
- GitHub Actions and release packaging.
