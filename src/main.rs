use std::path::PathBuf;

use clap::Parser;
use configparser::ini::Ini;
use tracing::debug;
use tracing::info;

const LONG_ABOUT: &str = "monitord: Know how happy your systemd is! 😊";

/// Clap CLI Args struct with metadata in help output
#[derive(Debug, Parser)]
#[clap(author, version, about, long_about = LONG_ABOUT)]
struct Cli {
    /// Location of your monitord config
    #[clap(short, long, value_parser, default_value = "/etc/monitord.conf")]
    config: PathBuf,

    /// Adjust the console log-level
    #[arg(long, short, value_enum, ignore_case = true, default_value = "Info")]
    log_level: monitord::logging::LogLevels,
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let args = Cli::parse();
    monitord::logging::setup_logging(args.log_level.into());

    info!("{}", LONG_ABOUT);
    debug!("CLI Args: {:?}", args);
    debug!("Loading {:?} config", args.config.as_os_str());
    let mut config = Ini::new();
    let _config_map = config
        .load(args.config)
        .map_err(|e| anyhow::anyhow!("Config error: {:?}", e))?;

    monitord::stat_collector(config.into(), true).await
}
