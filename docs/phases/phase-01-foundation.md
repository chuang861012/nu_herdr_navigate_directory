# Phase 1 — Project and Plugin Foundation

## Objective

Create a reproducible, compilable Rust application that registers the planned
Nushell plugin and `hcd` signature without implementing navigation behavior.
This phase establishes only the package, process entry point, module boundaries,
and baseline developer checks.

## Prerequisites

- [ ] [Agent] Confirm the repository contains no implementation files that
  conflict with the planned scaffold.
- [ ] [Agent] Re-read system-design sections 4–6, 13, and 17.
- [ ] [Agent] Determine the current stable Nushell plugin SDK and Rust stable
  release, then record exact dependency versions in project manifests rather
  than duplicating patch versions in design documents.

## Work items

### 1. Cargo baseline

- [ ] [Agent] Create one root Cargo application package named
  `nu_plugin_herdr_cd`; do not create a workspace or additional crates.
- [ ] [Agent] Use Rust edition 2024, declare an explicit `rust-version`, and
  track `Cargo.lock`.
- [ ] [Agent] Add `nu-plugin` and `nu-protocol` at the same minor version and
  only the dependencies required by the approved design.
- [ ] [Agent] Set package metadata and the MIT license consistently.

### 2. Plugin entry point and command surface

- [ ] [Agent] Serve the plugin with `nu_plugin::serve_plugin` and
  `MsgPackSerializer`.
- [ ] [Agent] Register the plugin identity `herdr_cd` and exactly one public
  command, `hcd`.
- [ ] [Agent] Declare one required `path: filepath` positional argument, no
  flags, no pipeline input, and `nothing` output.
- [ ] [Agent] Return an explicit not-yet-implemented error from the command
  body until later phases replace it; do not add partial navigation behavior.

### 3. Source boundaries and tooling

- [ ] [Agent] Establish narrow `domain`, `herdr`, and `command` module
  boundaries without speculative submodules or framework abstractions.
- [ ] [Agent] Add the minimal internal error-kind skeleton needed by the
  command boundary.
- [ ] [Agent] Add repository-local Rust formatting and lint expectations only
  where configuration differs from tool defaults.
- [ ] [Agent] Document build, test, and local plugin-registration commands in
  the README without claiming behavior that is not implemented.

### 4. Baseline verification

- [ ] [Agent] `cargo fmt --check` passes.
- [ ] [Agent] `cargo clippy --all-targets --all-features -- -D warnings`
  passes.
- [ ] [Agent] `cargo test` passes.
- [ ] [Agent] The compiled binary exposes the expected Nushell plugin metadata
  and command signature in a test that does not modify the user's plugin
  registry.

## User actions and confirmation

- [ ] [User action] No external system or global Nushell registration is
  required in this phase.
- [ ] [User confirmation] Review the direct dependencies, command signature,
  and three module boundaries before implementation advances.

## Phase gate

- [ ] [Agent] The package builds reproducibly from the tracked lockfile.
- [ ] [Agent] The plugin serves through MsgPack and advertises only `hcd`.
- [ ] [Agent] All baseline checks pass with no navigation behavior implemented.
- [ ] [User confirmation] The foundation review is approved.

## Out of scope

- Path canonicalization and containment.
- Herdr environment or process inspection.
- Socket communication, focus, tab creation, or workspace creation.
- Complete `hcd` behavior, CI, publishing, and global plugin installation.
