# Changelog

All notable changes to this project will be documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.1.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [Unreleased]

## [0.2.0] - 2026-08-26

### Added

- No-path and previous-directory navigation: `hnd` now uses the caller's home
  directory, while `hnd -` uses caller `OLDPWD` or falls back to the current
  directory when history is absent.
- Stable `idle_agent_statuses` plugin configuration for choosing which exact
  agent states make a pane reusable. It accepts `idle`, `done`, `blocked`, and
  `working`, defaults to `[idle done]`, and accepts an empty list to disable
  agent-pane reuse without disabling proven-idle shell reuse.
- Experimental, disabled-by-default dynamic completion for `hnd`. Enable it
  with `$env.config.plugins.herdr_navigate_directory.dynamic_completion = true`.
  Inside Herdr it can enrich directory candidates from live workspace and pane
  paths; outside Herdr, on any inspection failure, or when no semantic match
  exists it falls back to native directory completion. Completion never
  changes how `hnd` executes. Nushell 0.115 may cache results, so put the
  opt-in in `config.nu` and start a new session.

### Changed

- **Breaking:** a bare `-` is now the previous-directory sentinel instead of a
  literal relative directory path. Use `hnd ./-` or an absolute path to reach a
  directory named `-`.
- Successful directory changes now write the canonical prior directory to
  `OLDPWD` before updating `PWD`. Herdr focus/create actions and no-ops leave
  both values unchanged.
- **Breaking:** inside Herdr, a present plugin configuration must now be a
  record containing only `dynamic_completion` and `idle_agent_statuses`.
  Non-record values, unknown keys, invalid status lists, and unreadable plugin
  configuration now return `invalid_configuration` before any Herdr action.
- The `hnd` path argument is now a directory rather than a generic filepath,
  so native completion and the public signature match the directory-only
  contract. Accepted path forms are unchanged.
- **Breaking:** renamed the repository to `nu_herdr_navigate_directory`, the
  Cargo package and binary to `nu_plugin_herdr_navigate_directory`, the
  Nushell plugin identity to `herdr_navigate_directory`, and the public
  command from `hcd` to `hnd`. Existing installs must reinstall the new
  binary, then `plugin add` it and `plugin use herdr_navigate_directory`.

### Fixed

- Install docs now register the plugin with the installed binary path. `plugin
  add` searches the current directory and `NU_PLUGIN_DIRS`, not `PATH`.

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

[Unreleased]: https://github.com/chuang861012/nu_herdr_navigate_directory/compare/0.2.0...HEAD
[0.2.0]: https://github.com/chuang861012/nu_herdr_navigate_directory/compare/0.1.1...0.2.0
[0.1.1]: https://github.com/chuang861012/nu_herdr_navigate_directory/compare/0.1.0...0.1.1
[0.1.0]: https://github.com/chuang861012/nu_herdr_navigate_directory/releases/tag/0.1.0
