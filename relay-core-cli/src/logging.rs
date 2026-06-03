//! CLI log layout: RFC3339 timestamps, `relay_core_`-stripped targets, optional ANSI colors.

use nu_ansi_term::{Color, Style};
use std::borrow::Cow;
use std::fs::File;
use tracing::Level;
use tracing::Subscriber;
use tracing_appender::non_blocking::WorkerGuard;
use tracing_subscriber::EnvFilter;
use tracing_subscriber::fmt::format::{FormatEvent, FormatFields, Writer};
use tracing_subscriber::layer::SubscriberExt;
use tracing_subscriber::registry::LookupSpan;
use tracing_subscriber::util::SubscriberInitExt;

struct RelayEventFormat;

impl<S, N> FormatEvent<S, N> for RelayEventFormat
where
    S: Subscriber + for<'a> LookupSpan<'a>,
    N: for<'a> FormatFields<'a> + 'static,
{
    fn format_event(
        &self,
        ctx: &tracing_subscriber::fmt::FmtContext<'_, S, N>,
        mut writer: Writer<'_>,
        event: &tracing::Event<'_>,
    ) -> std::fmt::Result {
        let meta = event.metadata();
        write_log_prefix(&mut writer, meta.level(), meta.target())?;
        ctx.field_format().format_fields(writer.by_ref(), event)?;
        writeln!(writer)
    }
}

fn write_log_prefix(writer: &mut Writer<'_>, level: &Level, target: &str) -> std::fmt::Result {
    let ts = log_timestamp();
    let target = short_target_cow(target);

    if writer.has_ansi_escapes() {
        write!(
            writer,
            "{} {} {} ",
            Style::new().dimmed().paint(&ts),
            paint_level(*level),
            Color::Cyan.dimmed().paint(target.as_ref()),
        )
    } else {
        write!(writer, "{ts} {} {} ", level.as_str(), target.as_ref(),)
    }
}

/// Level colors approximate tracing-subscriber's default `FmtLevel`.
fn paint_level(level: Level) -> nu_ansi_term::AnsiString<'static> {
    let label = level.as_str();
    match level {
        Level::ERROR => Color::Red.bold().paint(label),
        Level::WARN => Color::Yellow.paint(label),
        Level::INFO => Color::Green.paint(label),
        Level::DEBUG => Color::Blue.paint(label),
        Level::TRACE => Color::Purple.paint(label),
    }
}

/// UTC, RFC3339, microsecond precision (`2026-06-03T18:27:08.442784Z`).
fn log_timestamp() -> String {
    chrono::Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Micros, true)
}

fn short_target_cow(target: &str) -> Cow<'_, str> {
    Cow::Borrowed(target.strip_prefix("relay_core_").unwrap_or(target))
}

fn build_filter() -> EnvFilter {
    EnvFilter::from_default_env().add_directive(tracing::Level::INFO.into())
}

/// Log to stdout (normal CLI mode).
pub fn init_stdout() {
    tracing_subscriber::registry()
        .with(build_filter())
        .with(
            tracing_subscriber::fmt::layer()
                .event_format(RelayEventFormat)
                .with_ansi(true)
                .with_writer(std::io::stdout),
        )
        .init();
}

/// Log to a file on a background thread (TUI mode — keeps the terminal clean).
///
/// Returns a guard that must be held for the process lifetime so buffered logs flush.
pub fn init_file(file: File) -> WorkerGuard {
    let (writer, guard) = tracing_appender::non_blocking(file);
    tracing_subscriber::registry()
        .with(build_filter())
        .with(
            tracing_subscriber::fmt::layer()
                .event_format(RelayEventFormat)
                .with_ansi(false)
                .with_writer(writer),
        )
        .init();
    guard
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn short_target_cow_strips_crate_prefix() {
        assert_eq!(
            short_target_cow("relay_core_lib::proxy::tunnel").as_ref(),
            "lib::proxy::tunnel"
        );
        assert_eq!(
            short_target_cow("relay_core_lifecycle").as_ref(),
            "lifecycle"
        );
    }

    #[test]
    fn short_target_cow_keeps_remainder_of_path() {
        let long = "relay_core_lib::proxy::something::deep";
        assert_eq!(
            short_target_cow(long).as_ref(),
            "lib::proxy::something::deep"
        );
    }

    #[test]
    fn log_timestamp_is_rfc3339_micros_utc() {
        let ts = log_timestamp();
        assert!(ts.ends_with('Z'));
        assert!(ts.contains('T'));
        assert_eq!(ts.split('.').nth(1).map(|f| f.len()), Some(7)); // ".442784Z"
    }

    #[test]
    fn paint_level_emits_expected_ansi_codes() {
        let err = paint_level(Level::ERROR).to_string();
        assert!(
            err.contains("\x1b[31m") || err.contains("\x1b[1;31m"),
            "ERROR should be red: {err:?}"
        );
        let info = paint_level(Level::INFO).to_string();
        assert!(info.contains("\x1b[32m"), "INFO should be green: {info:?}");
        let warn = paint_level(Level::WARN).to_string();
        assert!(warn.contains("\x1b[33m"), "WARN should be yellow: {warn:?}");
        let debug = paint_level(Level::DEBUG).to_string();
        assert!(
            debug.contains("\x1b[34m"),
            "DEBUG should be blue: {debug:?}"
        );
        let trace = paint_level(Level::TRACE).to_string();
        assert!(
            trace.contains("\x1b[35m"),
            "TRACE should be purple: {trace:?}"
        );
    }
}
