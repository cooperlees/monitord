use std::path::PathBuf;

use anyhow::Result;
use clap::Parser;
use clap_verbosity_flag::InfoLevel;
use configparser::ini::Ini;
use log::debug;
use log::info;

const LONG_ABOUT: &str = "monitord: Know how happy your systemd is! 😊";

/// Clap CLI Args struct with metadata in help output
#[derive(Debug, Parser)]
#[clap(author, version, about, long_about = LONG_ABOUT)]
struct Cli {
    /// Location of your monitord config
    #[clap(short, long, value_parser, default_value = "/etc/monitord.conf")]
    config: PathBuf,
    #[clap(flatten)]
    verbose: clap_verbosity_flag::Verbosity<InfoLevel>,
}

fn main() -> Result<(), String> {
    let args = Cli::parse();
    env_logger::Builder::new()
        .filter_level(args.verbose.log_level_filter())
        .init();

    info!("{}", LONG_ABOUT);
    debug!("CLI Args: {:?}", args);
    debug!("Loading {:?} config", args.config.as_os_str());
    let mut config = Ini::new();
    let _config_map = config.load(args.config)?;

    monitord::stat_collector(config)
}
