# Phase 2 — Domain Model and Path Decisions

## Objective

Implement the canonical path model, typed decision inputs, deterministic
resource selection, idle-evidence classification, and pure navigation decision
engine. No Herdr process, socket, or Nushell environment mutation is allowed in
this phase.

## Prerequisites

- [x] [Agent] Phase 1 gate has passed.
- [x] [Agent] Re-read system-design sections 7 and 9–12.

## Work items

### 1. Canonical path model

- [x] [Agent] Resolve caller-relative paths and support only `~` and `~/...`
      home expansion.
- [x] [Agent] Canonicalize caller cwd and target to physical absolute paths,
      resolving `.`, `..`, and symbolic links.
- [x] [Agent] Reject missing, non-directory, non-enterable, or non-UTF-8
      targets before producing an action.
- [x] [Agent] Implement component-aware equality, strict-descendant checks,
      ancestor checks, and workspace-root depth comparison.
- [x] [Agent] Exclude invalid workspace roots and pane foreground paths from
      matching without weakening target-path validation.

### 2. Typed domain inputs and actions

- [x] [Agent] Define normalized caller, workspace, tab, pane, focus, agent
      status, and shell-process evidence types without importing transport JSON
      types into the domain layer.
- [x] [Agent] Define exactly the approved actions: `NoOp`, `ChangeDirectory`,
      `FocusPane`, `CreateTab`, and `CreateWorkspace`.
- [x] [Agent] Classify agent panes as eligible only for `idle` or `done` and
      classify shell panes as eligible only when foreground evidence proves an
      idle interactive shell.
- [x] [Agent] Treat missing or uncertain evidence as ineligible.

### 3. Deterministic selection and decision tree

- [x] [Agent] Implement same-workspace candidate priority before cwd descent.
- [x] [Agent] Implement focused-tab, focused-pane, and authoritative-list
      ordering without occupant-type weighting.
- [x] [Agent] Select the deepest containing workspace and apply the approved
      equal-depth tie order.
- [x] [Agent] Ignore exact-path panes in non-containing external workspaces.
- [x] [Agent] Treat busy exact-path panes as unavailable and continue to the
      approved directory-change or create action.
- [x] [Agent] Keep the decision function deterministic and free of I/O,
      environment reads, clocks, process execution, and global state.

### 4. Decision-table verification

- [x] [Agent] Add table-driven tests for every branch listed in system-design
      section 19.1.
- [x] [Agent] Add explicit boundary tests for `/`, component prefixes,
      symbolic-link identity, equal workspace roots, and invalid optional paths.
- [x] [Agent] Prove that target-equals-cwd returns `NoOp` without idle evidence.
- [x] [Agent] Prove that `hcd ..` from a nested cwd does not produce
      `ChangeDirectory` when no same-workspace pane matches.

## User actions and confirmation

- [x] [User action] No external operation is required in this phase.
- [x] [User confirmation] Review the decision-table cases and their selected
      actions against the system-design flowchart.

## Phase gate

- [x] [Agent] The domain layer has no Nushell or Herdr transport side effects.
- [x] [Agent] Every decision-tree branch and tie-break rule has focused tests.
- [x] [Agent] Formatting, Clippy, and tests pass.
- [x] [User confirmation] The domain behavior review is approved.

## Out of scope

- Reading `EngineInterface` or changing `$env.PWD`.
- Executing `HERDR_BIN_PATH`.
- Parsing live Herdr responses or connecting to `HERDR_SOCKET_PATH`.
- Retry orchestration, total deadlines, CI, and distribution.
