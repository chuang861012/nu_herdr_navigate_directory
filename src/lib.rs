//! Nushell plugin that provides the `hcd` command.

mod command;
mod domain;
mod herdr;

pub use command::HerdrCdPlugin;

/// Nushell plugin identity advertised by the `nu_plugin_herdr_cd` binary.
pub const PLUGIN_IDENTITY: &str = "herdr_cd";
