use chrono::DateTime;
use chrono::Local;

use tracing::level_filters::LevelFilter;
use tracing::Level;
use tracing::Metadata;
use tracing_appender::rolling;
use tracing_subscriber::filter::FilterFn;
use tracing_subscriber::fmt::format::Writer;
use tracing_subscriber::fmt::time::FormatTime;
use tracing_subscriber::prelude::__tracing_subscriber_SubscriberExt;
use tracing_subscriber::reload::Handle;
use tracing_subscriber::Layer;
use tracing_subscriber::Registry;
use tracing_subscriber::{filter, prelude::*, reload};

struct LocalTime;
use tracing_subscriber::util::SubscriberInitExt;

impl FormatTime for LocalTime {
    fn format_time(&self, w: &mut Writer<'_>) -> std::fmt::Result {
        // Get the current time in local timezone
        let now: DateTime<Local> = Local::now();
        write!(w, "{}", now.format("%Y-%m-%d %H:%M:%S%.3f"))
    }
}

pub fn setup_logger(
) -> Result<Handle<FilterFn<impl Fn(&Metadata<'_>) -> bool>, Registry>, anyhow::Error> {
    let app_file = rolling::daily("./logs", "spire.log");
    let filter = filter::filter_fn(|metadata| {
        if metadata.target().starts_with("delay_timer::entity") {
            return false;
        }

        if metadata.level() > &Level::INFO {
            return false;
        }
        true
    });
    let (filter, reload_handle) = reload::Layer::new(filter);

    let file_layer = tracing_subscriber::fmt::Layer::new()
        .with_target(true)
        .with_ansi(false)
        .with_line_number(true)
        .with_timer(LocalTime)
        .with_writer(app_file)
        .with_filter(filter);
    // let console_layer = tracing_subscriber::fmt::Layer::new()
    //     .with_target(true)
    //     .with_ansi(true)
    //     .with_timer(LocalTime)
    //     .with_writer(std::io::stdout)
    //     .with_filter(tracing_subscriber::filter::LevelFilter::INFO);
    let _ = tracing_subscriber::registry()
        .with(file_layer)
        // .with(console_layer)
        .with(tracing_subscriber::filter::LevelFilter::TRACE)
        .try_init();
    Ok(reload_handle)
}
