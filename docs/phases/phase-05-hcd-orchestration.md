# Phase 5 — Complete `hcd` Orchestration

## Objective

Connect the Nushell command boundary, canonical path handling, pure decision
engine, Herdr inspection, and Herdr actions into the complete approved `hcd`
behavior. Add bounded recomputation, total deadlines, cancellation, structured
errors, and behavior-level tests.

## Prerequisites

- [ ] [Agent] Phase 4 gate has passed.
- [ ] [Agent] Re-read system-design sections 6–17 and the decision flowchart.

## Work items

### 1. Nushell command boundary

- [ ] [Agent] Decode the required filepath and read caller cwd, home, and Herdr
  context through `EngineInterface`.
- [ ] [Agent] Validate and canonicalize the target before any state change.
- [ ] [Agent] Outside Herdr, update only the caller's `$env.PWD` to the
  canonical target and return `nothing`.
- [ ] [Agent] Inside Herdr, reject malformed or incomplete context without an
  ordinary-directory-change fallback.

### 2. Inspect, decide, and act

- [ ] [Agent] Resolve the live caller and collect the typed session view.
- [ ] [Agent] Execute the pure decision algorithm in its exact approved order:
  no-op, same-workspace pane, strict cwd descent, nearest workspace pane, tab
  create, then workspace create.
- [ ] [Agent] Inspect process information only for exact-path shell candidates
  whose idle state is not already determined by agent status.
- [ ] [Agent] Execute exactly the returned domain action and keep successful
  output silent.
- [ ] [Agent] Confirm Herdr navigation and create actions never change the
  caller pane's cwd.

### 3. Recompute, deadline, and cancellation

- [ ] [Agent] Recompute from a fresh live caller and snapshot once after a
  candidate `not_found` or stale caller/snapshot mismatch.
- [ ] [Agent] Recompute once immediately before creation and execute the new
  decision instead if a reusable pane or better workspace has appeared.
- [ ] [Agent] Never recompute more than once for one invocation.
- [ ] [Agent] Enforce the 10-second total deadline in addition to operation
  timeouts.
- [ ] [Agent] On Nushell interruption, terminate/reap a child or close a socket,
  skip remaining work, and return an interruption error.

### 4. Structured errors and partial failure

- [ ] [Agent] Map failures into the approved internal error categories and
  Nushell `LabeledError` spans.
- [ ] [Agent] Label path failures at the filepath argument and Herdr/context
  failures at the command head.
- [ ] [Agent] Sanitize Herdr details and never dump an environment, socket path,
  snapshot, or unbounded stdout/stderr.
- [ ] [Agent] Preserve the no-fallback and no-rollback rules for every Herdr
  failure.

### 5. Behavior verification

- [ ] [Agent] Add adapter and orchestration tests for every decision outcome,
  retry trigger, timeout boundary, cancellation path, and error category.
- [ ] [Agent] Use fake transports; the normal suite must not require a live
  Herdr session or modify the user's Nushell plugin registry.
- [ ] [Agent] Verify successful calls return `nothing` and only
  `ChangeDirectory` mutates `$env.PWD`.
- [ ] [Agent] Verify two concurrent create decisions may still race without
  introducing global locking or hidden persistent state.

## User actions and confirmation

- [ ] [User action] A real Nushell/Herdr end-to-end run is optional and is not a
  normal automated-test prerequisite.
- [ ] [User confirmation] Review the behavior matrix and, if performed, the
  optional end-to-end evidence before approving the complete command.

## Phase gate

- [ ] [Agent] The complete system-design decision tree is implemented without
  additional commands, flags, configuration, or fallback behavior.
- [ ] [Agent] All domain, transport, adapter, and orchestration tests pass.
- [ ] [Agent] Formatting and Clippy pass with warnings denied.
- [ ] [User confirmation] Complete `hcd` behavior is approved.

## Out of scope

- Windows and cross-session navigation.
- Configuration, fuzzy matching, history, bookmarks, or extra `cd` forms.
- crates.io, Homebrew, or prebuilt-binary releases.
- Production deployment or interaction with live databases or services.
