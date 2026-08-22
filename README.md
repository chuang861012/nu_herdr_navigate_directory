# nu_herdr_cd

`nu_herdr_cd` is a Rust plugin for Nushell. It will provide `hcd`, a
directory-navigation command that cooperates with Herdr workspaces, tabs, and
panes.

The current tree is a compilable plugin with the complete `hcd` command:
canonical path handling, a pure decision engine, Herdr inspection, exact-pane
focus, focused create, bounded recomputation, and structured errors. Automated
tests use fake CLI and socket transports.

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
| Platforms | Linux and macOS |

## Development

The plugin SDK requires Rust 1.95 or later. Exact crate versions are recorded
in `Cargo.toml` and `Cargo.lock`.

```text
cargo fmt --check
cargo clippy --all-targets --all-features -- -D warnings
cargo test
cargo build --release
```

Automated tests do not require an installed Herdr, a running Herdr session, or
changes to the local Nushell plugin registry.

## Local plugin registration

Registration is optional during development and is not part of the automated
test suite. It writes to the current Nushell plugin registry.

The plugin binary must be used with Nushell 0.115. After building:

```nu
plugin add target/release/nu_plugin_herdr_cd
plugin use herdr_cd
```

`plugin add` is not required again after a Nushell restart if the plugin
remains in the registry. After registration, `hcd <path>` navigates as specified
in the system design. Inspection, focus, create, retry, and error behavior are
covered by fake-CLI and fake-socket tests.

## License

MIT
