//! The shared command table: the single ordered source the runtime dispatch and the codegen
//! emitters both read.
//!
//! [`COMMANDS`] holds the typed commands in frozen wire order (the committed
//! `schemas/control/command-manifest.generated.json` order, `ping` first, `quit` last), so the
//! emitters reproduce the committed artifacts byte for byte. The untyped reflective builtin `help`
//! is not a row; it is the manifest's single top-level skip.
//!
//! [`COMMAND_FIXTURES`] and [`COMMAND_SKIPS`] are e2e wire-contract metadata for the manifest
//! emitter. Every command carries exactly one of the two, which a test enforces.

mod dto_names;
mod metadata;
mod table;
#[cfg(test)]
mod tests;

pub use dto_names::DTO_TYPE_NAMES;
pub use metadata::{COMMAND_FIXTURES, COMMAND_SKIPS, fixture_for, skip_for};
pub use table::COMMANDS;

/// One row of the command table: a wire command name, its one-line `help` summary, and the type
/// *names* of its params/result DTOs. The emitters resolve the names to
/// `#/components/schemas/<name>` `$ref`s; the runtime knows the concrete `P`/`R` at its
/// `register::<P, R>` call sites.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct CommandSpec {
    /// The wire `cmd` string (kebab-case; dotted for the `profiler.*` group).
    pub name: &'static str,
    /// The one-line summary `help` reports and the OpenRPC `method.summary` carries.
    pub summary: &'static str,
    /// The params DTO type name, resolved to a schema `$ref` by the emitter.
    pub params: &'static str,
    /// The result DTO type name, resolved to a schema `$ref` by the emitter.
    pub result: &'static str,
}

impl CommandSpec {
    /// One table row.
    #[must_use]
    pub const fn new(
        name: &'static str,
        summary: &'static str,
        params: &'static str,
        result: &'static str,
    ) -> Self {
        Self {
            name,
            summary,
            params,
            result,
        }
    }
}

/// The untyped reflective builtin, recorded as the manifest's single top-level skip.
pub const HELP_COMMAND: &str = "help";

/// The reason recorded for the `help` skip in the manifest's top-level `skips` list.
pub const HELP_SKIP_REASON: &str = "reflective registry";
