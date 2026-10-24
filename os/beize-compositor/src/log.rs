//! Logging to the journal: Smithay and the compositor report through
//! tracing; each line goes to stderr with its syslog priority in front
//! (`<4>` for a warning), which systemd reads (SyslogLevelPrefix=, on by
//! default), so `journalctl -p warning` sees the compositor's warnings.
//! BEIZE_COMPOSITOR_LOG=debug or trace shows Smithay's steps too.

use tracing::{Event, Level, Subscriber};
use tracing_subscriber::Layer as _;
use tracing_subscriber::filter::DynFilterFn;
use tracing_subscriber::fmt::format::{self, FormatEvent, FormatFields, Writer};
use tracing_subscriber::fmt::{FmtContext, Layer};
use tracing_subscriber::layer::SubscriberExt as _;
use tracing_subscriber::registry::LookupSpan;
use tracing_subscriber::util::SubscriberInitExt as _;

pub fn init() {
    let level = match std::env::var("BEIZE_COMPOSITOR_LOG").as_deref() {
        Ok("trace") => Level::TRACE,
        Ok("debug") => Level::DEBUG,
        _ => Level::INFO,
    };
    // Smithay passes the Vulkan loader's messages on at their own severity;
    // its chatter at INFO only shows when asked for debug.
    let filter = DynFilterFn::new(move |meta, context| {
        if *meta.level() > level {
            return false;
        }
        level >= Level::DEBUG
            || *meta.level() < Level::INFO
            || context
                .lookup_current()
                .is_none_or(|span| span.name() != "backend_vulkan_debug")
    });
    // The journal stamps time, unit and priority itself.
    let lines = format()
        .without_time()
        .with_level(false)
        .with_target(false)
        .with_ansi(false);
    tracing_subscriber::registry()
        .with(
            Layer::default()
                .event_format(Priority(lines))
                .with_writer(std::io::stderr)
                .with_filter(filter),
        )
        .init();
}

fn format() -> format::Format {
    format::Format::default()
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
        // From info on only the message: inside one of Smithay's spans the
        // same line would else begin with its name (`drm_atomic: pacing:
        // …` at a page flip, `pacing: …` at idle), and the journal is read
        // by its lines. Debug and trace keep the spans, to follow a step.
        if priority <= 6 {
            context.format_fields(writer.by_ref(), event)?;
            return writeln!(writer);
        }
        self.0.format_event(context, writer, event)
    }
}
