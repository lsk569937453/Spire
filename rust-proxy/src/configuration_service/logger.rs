use chrono::DateTime;
use chrono::Local;

use logroller::{LogRollerBuilder, Rotation, RotationAge, TimeZone};
use std::path::Path;
use tracing::level_filters::LevelFilter;
use tracing_appender::non_blocking::NonBlockingBuilder;
use tracing_subscriber::Layer;
use tracing_subscriber::Registry;
use tracing_subscriber::filter::Targets;
use tracing_subscriber::fmt::format::Writer;
use tracing_subscriber::fmt::time::FormatTime;
use tracing_subscriber::prelude::__tracing_subscriber_SubscriberExt;
use tracing_subscriber::reload::Handle;
use tracing_subscriber::{filter, reload};

struct LocalTime;
use tracing_subscriber::util::SubscriberInitExt;

use crate::vojo::app_error::AppError;

impl FormatTime for LocalTime {
    fn format_time(&self, w: &mut Writer<'_>) -> std::fmt::Result {
        let now: DateTime<Local> = Local::now();
        write!(w, "{}", now.format("%Y-%m-%d %H:%M:%S%.3f"))
    }
}
pub fn setup_logger() -> Result<
    (
        Handle<Targets, Registry>,
        tracing_appender::non_blocking::WorkerGuard,
    ),
    AppError,
> {
    let (file_layer, reload_handle, guard) = setup_logger_with_path(Path::new("./logs"))?;

    tracing_subscriber::registry()
        .with(file_layer)
        .with(LevelFilter::TRACE) // Global minimum level
        .try_init()?;
    Ok((reload_handle, guard))
}

pub fn setup_logger_with_path(
    log_directory: &Path,
) -> Result<
    (
        impl Layer<Registry> + 'static,
        Handle<Targets, Registry>,
        tracing_appender::non_blocking::WorkerGuard,
    ),
    AppError,
> {
    let filename = Path::new("spire");
    let rolling_file_builder = LogRollerBuilder::new(log_directory, filename)
        .rotation(Rotation::AgeBased(RotationAge::Daily)) // Rotate daily
        .max_keep_files(7) // Keep a week's worth of logs
        .time_zone(TimeZone::Local) // Use local timezone
        .suffix("log".to_string())
        .build()
        .map_err(|e| AppError(e.to_string()))?;
    let filter = filter::Targets::new()
        .with_targets(vec![
            ("delay_timer", LevelFilter::OFF),
            ("hyper_util", LevelFilter::OFF),
        ])
        .with_default(LevelFilter::INFO);
    let (filter, reload_handle) = reload::Layer::new(filter);
    let noblocking_builder = NonBlockingBuilder::default().lossy(false);
    let (non_blocking, _guard) = noblocking_builder.finish(rolling_file_builder);

    let file_layer = tracing_subscriber::fmt::Layer::new()
        .with_target(true)
        .with_ansi(false)
        .with_line_number(true)
        .with_timer(LocalTime)
        .with_writer(non_blocking)
        .with_filter(filter);
    // let console_layer = tracing_subscriber::fmt::Layer::new()
    //     .with_target(true)
    //     .with_ansi(true)
    //     .with_timer(LocalTime)
    //     .with_writer(std::io::stdout)
    //     .with_filter(tracing_subscriber::filter::LevelFilter::INFO);

    Ok((file_layer, reload_handle, _guard))
}

#[cfg(test)]
mod tests {}
