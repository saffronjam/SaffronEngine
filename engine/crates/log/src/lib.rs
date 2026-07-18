//! The engine's logging install and line format.
//!
//! Every process renders the same compact line —
//! `HH:MM:SS.mmm  LEVEL  subsystem  [span fields] message` — built on `tracing`. The
//! subsystem column is derived from the event's target (the emitting crate, `saffron_`
//! stripped), span fields carry per-event context (e.g. a script's `entity`), and the
//! level is colored only on a real terminal so piped/captured output stays plain ASCII.
//!
//! [`init_logging`] installs the global subscriber once; it is the single seam where a
//! future file or editor-channel sink is added as one more layer.
//!
//! This crate has no Saffron dependencies so both the engine host and the out-of-workspace
//! editor bridge can share one formatter without pulling the engine into the editor build.

#![deny(unsafe_code)]

use std::fmt;
use std::io::IsTerminal;
use std::sync::Once;

use nu_ansi_term::Color;
use time::format_description::FormatItem;
use time::macros::format_description;
use time::{OffsetDateTime, UtcOffset};
use tracing::field::{Field, Visit};
use tracing::{Event, Level, Subscriber};
use tracing_subscriber::fmt::format::Writer;
use tracing_subscriber::fmt::{FmtContext, FormatEvent, FormatFields, FormattedFields};
use tracing_subscriber::prelude::*;
use tracing_subscriber::registry::LookupSpan;
use tracing_subscriber::{EnvFilter, registry, reload};

/// The wall-clock portion of a line: `12:30:01.234`.
const TIMESTAMP: &[FormatItem<'static>] =
    format_description!("[hour]:[minute]:[second].[subsecond digits:3]");

/// Column width the subsystem tag is padded to, so messages line up.
const SUBSYSTEM_WIDTH: usize = 10;

static INIT: Once = Once::new();

/// The default `EnvFilter` directive when `RUST_LOG` is unset: our crates at `debug`, but the
/// chatty third-party HTTP/TLS stack (pulled in by the editor's connector `reqwest`) pinned to
/// `warn`, and `winit` to `info` (its debug stream narrates internal event queuing), so they
/// don't drown the engine's own lines.
const DEFAULT_FILTER: &str =
    "debug,hyper=warn,hyper_util=warn,reqwest=warn,rustls=warn,h2=warn,tower=warn,winit=info";

/// Installs the global `tracing` subscriber: an `EnvFilter` (honoring `RUST_LOG`, default
/// [`DEFAULT_FILTER`]) feeding the [`CompactFormatter`] to stdout.
///
/// Idempotent and panic-free — a second call (a re-entered test, a second binary path)
/// is a no-op rather than the panic a bare `init` would raise.
pub fn init_logging() {
    INIT.call_once(|| {
        let fmt_layer = tracing_subscriber::fmt::layer()
            .with_writer(std::io::stdout)
            .event_format(CompactFormatter::new());

        let filter =
            EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new(DEFAULT_FILTER));
        let (filter, handle) = reload::Layer::new(filter);
        let _ = FILTER_HANDLE.set(handle);

        // `try_init` (not `init`) so a subscriber already set elsewhere can't panic us.
        let _ = registry().with(filter).with(fmt_layer).try_init();
    });
}

/// The live filter handle, so a target can be silenced after `init_logging`.
static FILTER_HANDLE: std::sync::OnceLock<reload::Handle<EnvFilter, registry::Registry>> =
    std::sync::OnceLock::new();

/// Silence a log target for the remainder of the process. For a third-party dependency whose
/// global hooks keep emitting after its lifecycle in this process has ended (e.g. a windowing
/// library's handlers observing stray platform events after its event loop finished) — the
/// target's lines carry no signal past that boundary.
pub fn silence_target(target: &str) {
    let Some(handle) = FILTER_HANDLE.get() else {
        return;
    };
    let Ok(directive) = format!("{target}=off").parse() else {
        return;
    };
    let _ = handle.modify(|filter| {
        *filter = std::mem::take(filter).add_directive(directive);
    });
}

/// Maps an event target to its subsystem tag: strip the `saffron_` crate prefix and keep
/// the first `::` segment. `saffron_rendering::renderer` → `rendering`; an explicit target
/// like `vulkan` or `viewport` is kept verbatim; an empty target → `engine`.
#[must_use]
pub fn subsystem_of(target: &str) -> &str {
    if target.is_empty() {
        return "engine";
    }
    let first = target.split("::").next().unwrap_or(target);
    first.strip_prefix("saffron_").unwrap_or(first)
}

/// The compact one-line event format shared by every Saffron process.
struct CompactFormatter {
    /// Captured once at init (looking it up per-event is unsound once threads spawn).
    offset: UtcOffset,
    /// Whether stdout is a terminal; gates ANSI coloring so captured logs stay plain.
    ansi: bool,
}

impl CompactFormatter {
    fn new() -> Self {
        Self {
            offset: UtcOffset::current_local_offset().unwrap_or(UtcOffset::UTC),
            ansi: std::io::stdout().is_terminal(),
        }
    }
}

impl<S, N> FormatEvent<S, N> for CompactFormatter
where
    S: Subscriber + for<'a> LookupSpan<'a>,
    N: for<'a> FormatFields<'a> + 'static,
{
    fn format_event(
        &self,
        ctx: &FmtContext<'_, S, N>,
        mut writer: Writer<'_>,
        event: &Event<'_>,
    ) -> fmt::Result {
        let meta = event.metadata();

        let now = OffsetDateTime::now_utc().to_offset(self.offset);
        let ts = now.format(&TIMESTAMP).unwrap_or_default();
        write!(writer, "{ts}  ")?;

        let level = *meta.level();
        let label = level_label(level);
        if self.ansi {
            write!(writer, "{}  ", level_color(level).paint(label))?;
        } else {
            write!(writer, "{label}  ")?;
        }

        let subsystem = subsystem_of(meta.target());
        write!(writer, "{subsystem:<SUBSYSTEM_WIDTH$} ")?;

        // Span context (outermost → innermost), e.g. `[entity=42]` from a script span.
        if let Some(scope) = ctx.event_scope() {
            for span in scope.from_root() {
                let ext = span.extensions();
                if let Some(fields) = ext.get::<FormattedFields<N>>() {
                    if !fields.fields.is_empty() {
                        write!(writer, "[{}] ", fields.fields)?;
                    }
                }
            }
        }

        let mut message = String::new();
        event.record(&mut MessageVisitor(&mut message));
        write!(writer, "{message}")?;

        writeln!(writer)
    }
}

/// Captures only the `message` field's rendered text (no surrounding quotes).
struct MessageVisitor<'a>(&'a mut String);

impl Visit for MessageVisitor<'_> {
    fn record_str(&mut self, field: &Field, value: &str) {
        if field.name() == "message" {
            self.0.push_str(value);
        }
    }

    fn record_debug(&mut self, field: &Field, value: &dyn fmt::Debug) {
        if field.name() == "message" {
            use std::fmt::Write;
            let _ = write!(self.0, "{value:?}");
        }
    }
}

/// Fixed-width (5 char) level label.
fn level_label(level: Level) -> &'static str {
    match level {
        Level::ERROR => "ERROR",
        Level::WARN => "WARN ",
        Level::INFO => "INFO ",
        Level::DEBUG => "DEBUG",
        Level::TRACE => "TRACE",
    }
}

/// The terminal color a level is painted in.
fn level_color(level: Level) -> Color {
    match level {
        Level::ERROR => Color::Red,
        Level::WARN => Color::Yellow,
        Level::INFO => Color::Green,
        Level::DEBUG => Color::Blue,
        Level::TRACE => Color::Purple,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn subsystem_strips_crate_prefix() {
        assert_eq!(subsystem_of("saffron_core"), "core");
        assert_eq!(subsystem_of("saffron_core::uuid"), "core");
        assert_eq!(
            subsystem_of("saffron_rendering::renderer::pass"),
            "rendering"
        );
    }

    #[test]
    fn subsystem_keeps_explicit_target() {
        // An explicit, non-`saffron_` target (the Vulkan messenger, the editor bridge)
        // is kept as-is rather than collapsed.
        assert_eq!(subsystem_of("vulkan"), "vulkan");
        assert_eq!(subsystem_of("viewport"), "viewport");
    }

    #[test]
    fn subsystem_falls_back_on_empty() {
        assert_eq!(subsystem_of(""), "engine");
    }

    #[test]
    fn level_labels_are_fixed_width() {
        for level in [
            Level::ERROR,
            Level::WARN,
            Level::INFO,
            Level::DEBUG,
            Level::TRACE,
        ] {
            assert_eq!(level_label(level).len(), 5);
        }
    }
}
