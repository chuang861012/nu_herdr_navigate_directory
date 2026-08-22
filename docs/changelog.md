# Changelog

Maintain `CHANGELOG.md` according to Keep-a-Changelog guide.

When making a notable user-visible change, update the `## [Unreleased]` section of `CHANGELOG.md`.

Use the appropriate Keep a Changelog category:

- Added
- Changed
- Deprecated
- Removed
- Fixed
- Security

Record notable changes rather than every commit.

Do not add changelog entries for internal-only changes such as refactoring, tests, formatting, comments, routine CI/tooling changes, or dependency updates with no meaningful user-visible or security impact.

Write entries from the user's perspective and describe observable behavior rather than implementation details.

Before adding an entry:

1. Read the existing `CHANGELOG.md`.
2. Check whether the change is notable to users or maintainers.
3. Avoid duplicating an existing entry.
4. Preserve the existing changelog formatting and conventions.

For breaking changes, make the impact explicit.

When preparing a release, move the applicable `[Unreleased]` entries into the supplied release version and date, then leave a fresh `[Unreleased]` section above it. Do not invent versions or release dates.
