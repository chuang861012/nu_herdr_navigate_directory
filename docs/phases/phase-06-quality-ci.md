# Phase 6 — Quality Gates, CI, and Source Distribution

## Objective

Finish repository-wide verification, document supported source installation,
and add non-deploying GitHub Actions for Linux and macOS. This phase prepares
version 0.1.0 for review as source code; it does not publish packages or push
repository state from an agent environment.

## Prerequisites

- [x] [Agent] Phase 5 gate has passed.
- [x] [Agent] Re-read system-design sections 19–21.
- [x] [Agent] Confirm the working tree and branch before changing CI or release
  documentation, and preserve unrelated user changes.

## Work items

### 1. Repository-wide local gate

- [x] [Agent] Run `cargo fmt --check`.
- [x] [Agent] Run
  `cargo clippy --all-targets --all-features -- -D warnings`.
- [x] [Agent] Run the full test suite without an installed or running Herdr.
- [x] [Agent] Build and test with the declared minimum Rust version and latest
  stable Rust where locally practical.
- [x] [Agent] Verify `cargo install --path .` from a clean local target path
  without mutating the user's global Cargo or Nushell configuration.

### 2. GitHub Actions

- [x] [Agent] Add build and test jobs for Linux and macOS.
- [x] [Agent] Run formatting and warning-denied Clippy on Linux.
- [x] [Agent] Verify the declared minimum Rust version and latest stable Rust.
- [x] [Agent] Keep workflow permissions read-only by default and do not add
  deployment, publishing, release-upload, secret, or Windows jobs.
- [x] [Agent] Use dependency caching only when it does not weaken lockfile
  reproducibility.

### 3. Documentation and source-install readiness

- [x] [Agent] Update README prerequisites, build, test, local
  `cargo install --path .`, future `cargo install --git`, and `plugin add`
  instructions to match verified behavior.
- [x] [Agent] Document Linux/macOS support, Herdr 0.8.2 minimum, the exact
  `HERDR_BIN_PATH` requirement, and intentional non-goals.
- [x] [Agent] Keep system design, phase status, comments, and user-facing help
  synchronized with implementation reality.
- [x] [Agent] Confirm license metadata and the tracked MIT license agree.

### 4. Final verification evidence

- [x] [Agent] Record the exact local verification commands and results in the
  handoff or pull-request description, not in a committed local report.
- [x] [Agent] Review the final diff for unrelated changes, generated artifacts,
  secrets, local paths, and accidentally tracked `.agents-local/` content.
- [x] [Agent] Confirm no crates.io, Homebrew, prebuilt-binary, deployment, or
  auto-publish workflow was introduced.

## User actions and confirmation

- [ ] [User action] Push the implementation branch manually when remote CI
  verification is desired; agents never push.
- [ ] [User action] Open or update the pull request and allow the Linux/macOS
  checks to complete.
- [ ] [User confirmation] Review CI results, installation documentation, and
  the final implementation diff.
- [ ] [User confirmation] Approve version 0.1.0 source-distribution readiness;
  publishing to any registry remains a separate future decision.

## Phase gate

- [x] [Agent] The complete local quality suite passes.
- [ ] [User action] Hosted Linux/macOS CI passes when a GitHub pull request is
  part of the chosen delivery workflow.
- [x] [Agent] Source installation and plugin-registration documentation matches
  verified commands.
- [ ] [User confirmation] The final quality and source-readiness review is
  approved.

## Out of scope

- `git push` by an agent.
- crates.io publishing, Homebrew formulae, prebuilt binaries, signing, or
  release automation.
- Windows jobs or support claims.
- Deployment, production environments, live databases, or external services.
