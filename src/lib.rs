//! Nushell plugin that provides the `hnd` command.

mod command;
mod domain;
mod herdr;

pub use command::HerdrNavigateDirectoryPlugin;

/// Nushell plugin identity advertised by the `nu_plugin_herdr_navigate_directory` binary.
pub const PLUGIN_IDENTITY: &str = "herdr_navigate_directory";
