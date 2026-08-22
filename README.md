# nu_herdr_cd

`nu_herdr_cd` is a Rust plugin for Nushell. It provides `hcd`, a
directory-navigation command that cooperates with Herdr workspaces, tabs, and
panes.

Outside Herdr, `hcd` updates the caller's `$env.PWD` to the canonical target.
Inside Herdr, it reuses an idle pane at the exact target when possible, changes
the current pane's directory only for downward navigation, and otherwise creates
or focuses a tab or workspace. Successful calls are silent and return
`nothing`.

See [the system design](docs/system-design.md) for the agreed behavior,
architecture, constraints, and verification strategy.

See [the implementation phases](docs/phases/README.md) for the staged delivery
order, prerequisites, work items, and acceptance gates.

## Status

| Item | Value |
| --- | --- |
| Package | `nu_plugin_herdr_cd` |
| Binary | `nu_plugin_herdr_cd` |
| Plugin identity | `herdr_cd` |
| Public command | `hcd <path: filepath> -> nothing` |
| Crate version | 0.1.0 |
| Nushell plugin SDK | 0.115 |
| Minimum Rust | 1.95.0 |
| Minimum Herdr | 0.8.2 |
| Platforms | Linux and macOS |
| License | MIT |

## Prerequisites

- Linux or macOS. Other platforms return `unsupported_platform` before any
  external action.
- Rust 1.95.0 or later to build from source. Exact crate versions are recorded
  in `Cargo.toml` and `Cargo.lock`.
- Nushell 0.115 to register and run the plugin.
- Herdr 0.8.2 or later when `hcd` runs inside a Herdr session. The plugin
  validates the live snapshot version and protocol; it does not cache that
  result across calls.

Inside Herdr, the plugin uses only the caller-injected `HERDR_BIN_PATH`. That
path is canonicalized and must resolve to an absolute, regular, executable
file. The plugin never searches `PATH` for a `herdr` binary. Missing,
malformed, or incomplete Herdr context is an error and never falls back to an
ordinary directory change.

## Development

```text
cargo fmt --check
cargo clippy --all-targets --all-features -- -D warnings
cargo test
cargo build --release
```

Automated tests do not require an installed Herdr, a running Herdr session, or
changes to the local Nushell plugin registry. They use fake CLI and Unix socket
transports.

GitHub Actions build and test on Linux and macOS for Rust 1.95.0 and latest
stable, using `--locked`. They also run formatting and warning-denied Clippy
on Linux. The workflow has read-only default permissions and does not publish,
deploy, or upload releases.

## Source installation

The initial supported installation paths are source-based. Publishing to
crates.io, packaging for Homebrew, and producing prebuilt binaries are
deferred.

From a local checkout:

```text
cargo install --path .
```

From the Git repository:

```text
cargo install --git https://github.com/chuang861012/nu_herdr_cd nu_plugin_herdr_cd
```

Both commands install `nu_plugin_herdr_cd` into Cargo's binary directory,
normally `~/.cargo/bin`.

## Plugin registration

Registration writes to the current Nushell plugin registry. After a source
install, with the binary on `PATH`:

```nu
plugin add nu_plugin_herdr_cd
plugin use herdr_cd
```

During development, register the release binary from the checkout instead:

```nu
plugin add target/release/nu_plugin_herdr_cd
plugin use herdr_cd
```

`plugin add` is not required again after a Nushell restart if the plugin
remains in the registry. After registration, `hcd <path>` navigates as specified
in the system design.

## Non-goals

The initial version does not provide Windows support, configuration, extra `cd`
forms, directory creation, crates.io or Homebrew packages, or prebuilt
binaries. See the system design for the complete list.

## License

MIT
