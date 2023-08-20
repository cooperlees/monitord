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

pub fn setup_logging(log_filter_level: LevelFilter) {
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
