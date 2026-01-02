use std::io::stderr;
use std::io::IsTerminal;

use clap::ValueEnum;
use tracing_glog::Glog;
use tracing_glog::GlogFields;
use tracing_subscriber::filter::LevelFilter;
use tracing_subscriber::fmt;
use tracing_subscriber::prelude::*;
use tracing_subscriber::Registry;

// This enum can be used to add `log-level` option to CLI binaries.
#[derive(ValueEnum, Clone, Debug, Copy)]
pub enum LogLevels {
    Error,
    Warn,
    Info,
    Debug,
    Trace,
}

impl From<LogLevels> for LevelFilter {
    fn from(public_level: LogLevels) -> Self {
        match public_level {
            LogLevels::Error => LevelFilter::ERROR,
            LogLevels::Warn => LevelFilter::WARN,
            LogLevels::Info => LevelFilter::INFO,
            LogLevels::Debug => LevelFilter::DEBUG,
            LogLevels::Trace => LevelFilter::TRACE,
        }
    }
}

/// Setup logging with tracing in Glog format for CLI
/// If `enable_tokio_console` is true and the `tokio-console` feature is enabled,
/// also spawns the tokio-console server on port 6669 for async runtime debugging.
///
/// **Important**: To use tokio-console, you must build with:
/// ```sh
/// RUSTFLAGS="--cfg tokio_unstable" cargo build --features tokio-console
/// ```
pub fn setup_logging(log_filter_level: LevelFilter, enable_tokio_console: bool) {
    #[cfg(feature = "tokio-console")]
    {
        if enable_tokio_console {
            console_subscriber::init();
            tracing::info!(
                "tokio-console enabled, connect with: tokio-console http://127.0.0.1:6669"
            );
            return;
        }
    }

    #[cfg(not(feature = "tokio-console"))]
    {
        if enable_tokio_console {
            eprintln!(
                "Warning: --enable-tokio-console was specified but the 'tokio-console' feature is not enabled. \
                 Rebuild with `cargo build --features tokio-console` to enable."
            );
        }
    }

    // Suppress unused variable warning when tokio-console feature is enabled
    #[cfg(feature = "tokio-console")]
    let _ = enable_tokio_console;

    let fmt = fmt::Layer::default()
        .with_writer(std::io::stderr)
        .with_ansi(stderr().is_terminal())
        .event_format(Glog::default().with_timer(tracing_glog::LocalTime::default()))
        .fmt_fields(GlogFields::default())
        .with_filter(log_filter_level);

    let subscriber = Registry::default().with(fmt);
    tracing::subscriber::set_global_default(subscriber)
        .expect("Unable to set global tracing subscriber");
}
