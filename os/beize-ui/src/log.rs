//! An app's log, to the journal as the compositor's: Beize UI and the app
//! report through tracing; each line goes to stderr with its syslog
//! priority in front (`<4>` for a warning), which systemd reads
//! (SyslogLevelPrefix=, on by default), so `journalctl -p warning` sees
//! them.

use tracing::{Event, Level, Subscriber};
use tracing_subscriber::fmt::format::{self, FormatEvent, FormatFields, Writer};
use tracing_subscriber::fmt::{FmtContext, Layer};
use tracing_subscriber::layer::SubscriberExt as _;
use tracing_subscriber::registry::LookupSpan;
use tracing_subscriber::util::SubscriberInitExt as _;
use tracing_subscriber::{Layer as _, filter::LevelFilter};

/// Logs from INFO on; `variable` (e.g. BEIZE_TERMINAL_LOG) set to `debug`
/// or `trace` shows more.
pub fn init(variable: &str) {
    let level = match std::env::var(variable).as_deref() {
        Ok("trace") => LevelFilter::TRACE,
        Ok("debug") => LevelFilter::DEBUG,
        _ => LevelFilter::INFO,
    };
    // The journal stamps time, unit and priority itself.
    let lines = format::Format::default()
        .without_time()
        .with_level(false)
        .with_target(false)
        .with_ansi(false);
    tracing_subscriber::registry()
        .with(
            Layer::default()
                .event_format(Priority(lines))
                .with_writer(std::io::stderr)
                .with_filter(level),
        )
        .init();
}

/// Puts the syslog priority of each event in front of its line.
struct Priority<F>(F);

impl<S, N, F> FormatEvent<S, N> for Priority<F>
where
    S: Subscriber + for<'a> LookupSpan<'a>,
    N: for<'a> FormatFields<'a> + 'static,
    F: FormatEvent<S, N>,
{
    fn format_event(
        &self,
        context: &FmtContext<'_, S, N>,
        mut writer: Writer<'_>,
        event: &Event<'_>,
    ) -> std::fmt::Result {
        let priority = match *event.metadata().level() {
            Level::ERROR => 3,
            Level::WARN => 4,
            Level::INFO => 6,
            _ => 7,
        };
        write!(writer, "<{priority}>")?;
        self.0.format_event(context, writer, event)
    }
}
