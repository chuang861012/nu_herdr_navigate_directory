# nu_herdr_cd

`nu_herdr_cd` is a Rust plugin for Nushell. It provides `hcd`, a
directory-navigation command that cooperates with Herdr workspaces, tabs, and
panes.

```nu
hcd <path: filepath> -> nothing
```

Outside Herdr, `hcd` updates the caller's `$env.PWD` to the canonical target.
Inside Herdr, it reuses an idle pane at the exact target when possible, changes
the current pane's directory only for downward navigation, and otherwise creates
or focuses a tab or workspace. Successful calls are silent and return
`nothing`.

See [the system design](docs/system-design.md) for architecture, constraints,
and verification.

## Prerequisites

- Linux or macOS
- Rust 1.95.0 or later to build from source
- Nushell 0.115
- Herdr 0.8.2 or later inside a Herdr session

Inside Herdr, `hcd` uses only the caller-injected `HERDR_BIN_PATH`. It never
searches `PATH`. Incomplete Herdr context is an error, not a fallback to
ordinary directory change.

## How `hcd` decides

> [!WARNING]
> `hcd` is opinionated. This version does not provide behavior customization.
> Customization may be added in a future version.

The target must exist, be an enterable directory, and resolve to a canonical
UTF-8 path. `~` and a leading `~/` are expanded. Relative paths are resolved
against the caller's cwd.

### Outside Herdr

If `HERDR_ENV` is absent, `hcd` sets the caller's `$env.PWD` to the canonical
target. It does not change the plugin process's working directory.

Any other `HERDR_ENV` value is an error.

### Inside Herdr

If `HERDR_ENV` is exactly `1`, `hcd` inspects the live session and chooses one
action:

```mermaid
flowchart TD
    A[hcd path] --> B[Canonicalize cwd and target]
    B --> C{Inside Herdr?}
    C -->|No| D[Set $env.PWD to the target]
    C -->|Yes| H{Target equals cwd?}
    H -->|Yes| N[Do nothing]
    H -->|No| I{Idle pane at the exact path<br/>in the current workspace?}
    I -->|Yes| J[Focus that pane]
    I -->|No| K{Target is inside the current directory?}
    K -->|Yes| D
    K -->|No| L{A workspace root contains the target?}
    L -->|No| M[Create and focus a workspace at the target]
    L -->|Yes| O{Idle pane at the exact path<br/>in that workspace?}
    O -->|Yes| J
    O -->|No| P[Create and focus a tab at the target]
```

In order:

1. If the target is already the caller's directory, do nothing.
2. If the current workspace already has an idle pane at the exact target,
   focus that pane. The calling pane's directory does not change.
3. If the target is a subdirectory of the current directory, change directory
   in the current pane.
4. Otherwise, use the nearest Herdr workspace whose root contains the target:
   - focus an idle pane already at the exact target, or
   - create and focus a new tab at the target.
5. If no workspace contains the target, create and focus a new workspace
   there.

Going to a parent or sibling never changes the current pane's directory. Busy
panes at the target are skipped; they do not block a directory change or the
creation of a new tab or workspace.

Created tabs and workspaces are focused. The calling pane stays where it is
unless the action is a downward directory change.

### Idle panes

A pane is reused only when its foreground directory is the exact target and it
is idle:

- a shell pane is idle only when process info proves the interactive shell
  itself is in the foreground;
- an agent pane is idle only when its status is `idle` or `done`.

Incomplete process information, `working`, `blocked`, and `unknown` agent
states are never treated as idle.

When several idle panes match, `hcd` prefers the caller's tab, then the
workspace's focused tab, then snapshot list order.

### Examples

| Current directory | Target       | Action                                                              |
| ----------------- | ------------ | ------------------------------------------------------------------- |
| `/repo`           | `/repo/src`  | Change directory, unless an idle pane already sits at `/repo/src`   |
| `/repo/src`       | `/repo`      | Create or focus a tab at `/repo`; never `cd ..` in the current pane |
| `/repo/src`       | `/repo/docs` | Focus an idle pane at `/repo/docs`, or create a tab                 |
| `/repo/src`       | `/other`     | Create and focus a workspace at `/other`                            |

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
remains in the registry. After registration, `hcd <path>` follows the decision
tree above.

## Non-goals

The initial version does not provide Windows support, configuration, extra `cd`
forms, directory creation, crates.io or Homebrew packages, or prebuilt
binaries. See the system design for the complete list.

## License

MIT
