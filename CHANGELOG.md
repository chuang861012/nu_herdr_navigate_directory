# Changelog

All notable changes to this project will be documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.1.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [Unreleased]

## [0.1.1] - 2026-08-23

### Security

- Authenticate the connected Herdr Unix-socket peer as the current user before
  sending a pane-focus request. A replaced socket path whose peer is not the
  current user is now a transport error.

## [0.1.0] - 2026-08-22

### Added

- `hcd <path>`, a Herdr-aware directory navigation command for Nushell on Linux
  and macOS. Outside Herdr it sets the caller's `$env.PWD` to the canonical
  target. Inside Herdr it uses only the injected `HERDR_BIN_PATH`, reuses an
  idle pane already at the target, changes directory only when navigating into
  a subdirectory, or creates and focuses a tab or workspace. Success is silent
  and returns nothing. Requires Nushell 0.115 and, inside a Herdr session,
  Herdr 0.8.2 or later.

[Unreleased]: https://github.com/chuang861012/nu_herdr_cd/compare/0.1.1...HEAD
[0.1.1]: https://github.com/chuang861012/nu_herdr_cd/compare/0.1.0...0.1.1
[0.1.0]: https://github.com/chuang861012/nu_herdr_cd/releases/tag/0.1.0
