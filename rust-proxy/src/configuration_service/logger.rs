use chrono::DateTime;
use chrono::Local;

use std::path::Path;
use tracing::level_filters::LevelFilter;
use tracing_appender::rolling;
use tracing_appender::rolling::RollingFileAppender;
use tracing_subscriber::filter::Targets;
use tracing_subscriber::fmt::format::Writer;
use tracing_subscriber::fmt::time::FormatTime;
use tracing_subscriber::prelude::__tracing_subscriber_SubscriberExt;
use tracing_subscriber::reload::Handle;
use tracing_subscriber::Layer;
use tracing_subscriber::Registry;
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
pub fn setup_logger() -> Result<Handle<Targets, Registry>, AppError> {
    let (file_layer, reload_handle) = setup_logger_with_path(Path::new("./logs"))?;

    tracing_subscriber::registry()
        .with(file_layer)
        .with(LevelFilter::TRACE) // Global minimum level
        .try_init()?;
    Ok(reload_handle)
}

pub fn setup_logger_with_path(
    log_directory: &Path,
) -> Result<(impl Layer<Registry> + 'static, Handle<Targets, Registry>), AppError> {
    let rolling_file_builder = RollingFileAppender::builder()
        .rotation(rolling::Rotation::DAILY)
        .filename_prefix("spire")
        .filename_suffix("log")
        .max_log_files(10)
        .build(log_directory)?;
    let filter = filter::Targets::new()
        .with_targets(vec![
            ("delay_timer", LevelFilter::OFF),
            ("hyper_util", LevelFilter::OFF),
        ])
        .with_default(LevelFilter::INFO);
    let (filter, reload_handle) = reload::Layer::new(filter);

    let file_layer = tracing_subscriber::fmt::Layer::new()
        .with_target(true)
        .with_ansi(false)
        .with_line_number(true)
        .with_timer(LocalTime)
        .with_writer(rolling_file_builder)
        .with_filter(filter);
    // let console_layer = tracing_subscriber::fmt::Layer::new()
    //     .with_target(true)
    //     .with_ansi(true)
    //     .with_timer(LocalTime)
    //     .with_writer(std::io::stdout)
    //     .with_filter(tracing_subscriber::filter::LevelFilter::INFO);

    Ok((file_layer, reload_handle))
}
#[cfg(all(debug_assertions, not(tarpaulin)))]
pub fn setup_logger_for_test() -> Result<Handle<Targets, Registry>, AppError> {
    let rolling_file_builder = RollingFileAppender::builder()
        .rotation(rolling::Rotation::MINUTELY)
        .filename_prefix("spire")
        .filename_suffix("log")
        .max_log_files(10)
        .build("./logs")?;
    let filter = filter::Targets::new()
        .with_targets(vec![
            ("delay_timer", LevelFilter::OFF),
            ("hyper_util", LevelFilter::OFF),
        ])
        .with_default(LevelFilter::INFO);
    let (filter, reload_handle) = reload::Layer::new(filter);

    let file_layer = tracing_subscriber::fmt::Layer::new()
        .with_target(true)
        .with_ansi(false)
        .with_line_number(true)
        .with_timer(LocalTime)
        .with_writer(rolling_file_builder)
        .with_filter(filter);
    let console_layer = tracing_subscriber::fmt::Layer::new()
        .with_target(true)
        .with_ansi(true)
        .with_timer(LocalTime)
        .with_writer(std::io::stdout)
        .with_filter(tracing_subscriber::filter::LevelFilter::DEBUG);
    let _ = tracing_subscriber::registry()
        .with(file_layer)
        .with(console_layer)
        .with(tracing_subscriber::filter::LevelFilter::TRACE)
        .try_init();
    Ok(reload_handle)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::{fs, thread, time::Duration};
    use tempfile::tempdir;
    use tracing::Subscriber;
    use tracing::{debug, error, event, info, trace, warn, Level};
    fn build_test_subscriber(
        log_directory: &Path,
    ) -> Result<(impl Subscriber + Send + Sync, Handle<Targets, Registry>), AppError> {
        let (file_layer, reload_handle) = setup_logger_with_path(log_directory)?;
        let subscriber = tracing_subscriber::registry()
            .with(file_layer)
            .with(LevelFilter::TRACE);
        Ok((subscriber, reload_handle))
    }

    fn read_log_file(log_dir: &Path) -> Result<String, AppError> {
        thread::sleep(Duration::from_millis(200));

        for entry in fs::read_dir(log_dir)? {
            let entry = entry?;
            let path = entry.path();
            if path.is_file()
                && path.to_string_lossy().contains("spire")
                && path.extension().is_some_and(|ext| ext == "log")
            {
                return Ok(fs::read_to_string(path)?);
            }
        }
        Err("No log file found")?
    }

    #[test]
    fn test_logger_setup_and_default_filtering() {
        let temp_log_dir = tempdir().expect("Failed to create temp dir");
        let (subscriber, _reload_handle) =
            build_test_subscriber(temp_log_dir.path()).expect("Logger setup failed");
        tracing::subscriber::with_default(subscriber, || {
            event!(target: "my_app", Level::TRACE, "This is a TRACE message.");
            debug!(target: "my_app", "This is a DEBUG message.");
            info!(target: "my_app", "This is an INFO message.");
            warn!(target: "my_app", "This is a WARN message.");
            error!(target: "my_app", "This is an ERROR message.");

            info!(target: "delay_timer", "This delay_timer INFO should be OFF.");
            info!(target: "hyper_util", "This hyper_util INFO should be OFF.");
        });

        let log_content = read_log_file(temp_log_dir.path()).expect("Could not read log file");

        assert!(log_content.contains("This is an INFO message."));
        assert!(log_content.contains("This is a WARN message."));
        assert!(log_content.contains("This is an ERROR message."));
        assert!(log_content.contains("my_app"));

        assert!(!log_content.contains("This is a TRACE message."));
        assert!(!log_content.contains("This is a DEBUG message."));

        assert!(!log_content.contains("This delay_timer INFO should be OFF."));
        assert!(!log_content.contains("This hyper_util INFO should be OFF."));

        temp_log_dir.close().expect("Failed to close temp_dir");
    }

    #[test]
    fn test_logger_filter_reloading() {
        let temp_log_dir = tempdir().expect("Failed to create temp dir for reloading test");
        println!("temp_log_dir: {:?}", temp_log_dir.path());
        let (subscriber, reload_handle) = build_test_subscriber(temp_log_dir.path())
            .expect("Logger setup failed for reloading test");

        tracing::subscriber::with_default(subscriber, || {
            info!(target: "reload_test", "Initial INFO message.");
            debug!(target: "reload_test", "Initial DEBUG message (should not appear).");
            let new_filter_targets = filter::Targets::new()
                .with_target("delay_timer", LevelFilter::INFO) // Change one specific target
                .with_default(LevelFilter::DEBUG); // Change default

            reload_handle
                .reload(new_filter_targets)
                .expect("Failed to reload filter");
            info!(target: "reload_test", "Post-reload INFO message.");
            debug!(target: "reload_test", "Post-reload DEBUG message (should appear now).");
            trace!(target: "reload_test", "Post-reload TRACE message (should not appear).");
            info!(target: "delay_timer", "Post-reload delay_timer INFO (should appear now).");
        });

        let log_content_after_reload =
            read_log_file(temp_log_dir.path()).expect("Could not read log file before reload");

        assert!(log_content_after_reload.contains("Initial INFO message."));
        assert!(!log_content_after_reload.contains("Initial DEBUG message"));

        assert!(log_content_after_reload.contains("Post-reload INFO message."));
        assert!(log_content_after_reload.contains("Post-reload DEBUG message (should appear now)."));
        assert!(
            !log_content_after_reload.contains("Post-reload TRACE message (should not appear).")
        );
        assert!(
            log_content_after_reload.contains("Post-reload delay_timer INFO (should appear now).")
        );

        temp_log_dir
            .close()
            .expect("Failed to close temp_dir for reloading test");
    }
}
