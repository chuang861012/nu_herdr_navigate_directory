# Repository instructions

## Language

- Write all repository content in English, including documentation, source
  code, tests, comments, issue references, and commit messages.

## Design authority

- Treat `docs/system-design.md` as the authoritative product and architecture
  specification.
- Follow the phase order and gates in `docs/phases/README.md` and the applicable
  phase file when planning or implementing work.
- If a phase file conflicts with the system design, the system design wins.
  Correct the phase file before implementing the conflicting work.
- Read the relevant sections of the system design before planning or changing
  behavior.
- Do not silently diverge from an approved design decision. Update the design
  document in the same change when an implementation decision changes the
  specified behavior, compatibility boundary, or security property.
- Keep the initial scope deliberately small. Do not add configuration,
  commands, platforms, transports, or distribution channels that the design
  lists as non-goals without explicit approval.

## Phase responsibility labels

Phase checklist items use these labels:

- `[Agent]`: an agent may implement and verify the item locally.
- `[User action]`: the user must perform the external, account, publishing, or
  local trust-boundary operation unless it is explicitly delegated.
- `[User confirmation]`: an agent may prepare the result and evidence, but the
  user must approve the phase gate before work advances to the next phase.

Do not mark a phase complete while a required user action or confirmation is
unfinished. Later-phase tests may be prepared early, but a later phase must not
be declared complete before its dependency gates pass.

## Implementation

- Keep the decision engine pure and separate from Nushell and Herdr side
  effects.
- Preserve the three intended boundaries: `domain`, `herdr`, and `command`.
- Prefer inferred Rust types where they remain clear, but use explicit domain
  types at protocol and action boundaries.
- Avoid global mutable state, an async runtime, shell command interpolation,
  and unnecessary abstractions.
- Treat paths, Herdr responses, environment context, timeouts, and resource IDs
  according to the fail-closed rules in the system design.
- Use only the Herdr binary injected through `HERDR_BIN_PATH`; never search
  `PATH` for a fallback binary.
- Keep comments synchronized with behavior. Prefer code that makes a comment
  unnecessary.

## Testing and checks

- Add focused tests for behavior changes. Prefer table-driven tests for the
  decision tree.
- Normal automated tests must not require an installed or running Herdr.
- Use fake CLI and Unix socket transports for Herdr integration tests.
- Before handing off Rust changes, run the applicable formatter, Clippy, and
  test commands described in `docs/system-design.md`.
- Do not add repetitive tests that merely restate the same branch without
  increasing confidence.
- Mark phase checklist items complete only after the implementation and the
  stated verification have actually passed.

## Repository hygiene

- Keep local reports, investigation notes, and temporary plans under
  `.agents-local/`; do not commit that directory.
- Keep unrelated user changes separate and do not overwrite them.
- Use Conventional Commits for repository commits.
- Never push from the agent environment.
