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
    #[clap(
        short,
        long,
        value_parser,
        env = "MONITORD_CONFIG",
        default_value = "/etc/monitord.conf"
    )]
    config: PathBuf,

    /// Adjust the console log-level
    #[arg(long, short, value_enum, ignore_case = true, default_value = "Info")]
    log_level: monitord::logging::LogLevels,
}

fn main() -> anyhow::Result<()> {
    // monitord re-executes itself as a tiny helper inside each machine's PID
    // namespace (see monitord::varlink::machine_connector). Dispatch that
    // before clap and before starting a multi-threaded runtime it never needs.
    let argv: Vec<String> = std::env::args().collect();
    if argv.get(1).map(String::as_str) == Some(monitord::varlink::machine_connector::HELPER_ARG) {
        monitord::varlink::machine_connector::helper_main(&argv[2..]);
    }
    tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()?
        .block_on(async_main())
}

async fn async_main() -> anyhow::Result<()> {
    let args = Cli::parse();
    monitord::logging::setup_logging(args.log_level.into());

    info!("{}", LONG_ABOUT);
    debug!("CLI Args: {:?}", args);
    debug!("Loading {:?} config", args.config.as_os_str());
    let mut config = Ini::new();
    let _config_map = config
        .load(args.config)
        .map_err(|e| anyhow::anyhow!("Config error: {:?}", e))?;

    monitord::stat_collector(config.try_into()?, None, true, None).await?;
    Ok(())
}
