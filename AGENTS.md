# Repository instructions

## Language

- Write all repository content in English, including documentation, source
  code, tests, comments, issue references, and commit messages.

## Design authority

- Treat `docs/system-design.md` as the authoritative product and architecture
  specification.
- Treat `docs/archived/` as a historical record only. It is read-only; do not
  edit files there, including the completed 0.1.0 implementation phases.
- Read the relevant sections of the system design before planning or changing
  behavior.
- Do not silently diverge from an approved design decision. Update the design
  document in the same change when an implementation decision changes the
  specified behavior, compatibility boundary, or security property.

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

## Repository hygiene

- Keep local reports, investigation notes, and temporary plans under
  `.agents-local/`; do not commit that directory.
- Keep unrelated user changes separate and do not overwrite them.
- Use Conventional Commits for repository commits.
- Never push from the agent environment.

## Commit & Pull Request Guidelines

- Before implementation, run `git branch --show-current`. If it reports `main`, create or switch to a focused feature branch before changing code, tests, configuration, or migrations. Never commit implementation work directly to `main`.
- History uses Conventional Commit subjects such as `docs: add system design`. Keep commits focused with an imperative `<type>: <summary>`. PRs should describe scope, list verification commands, and include screenshots for UI changes. Do not edit `docs/archived/` or other archived pre-release progress files.
