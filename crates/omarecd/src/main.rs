mod adapters;
mod app;
mod clock;
mod coordinator;
mod output;
mod postprocess;
mod runtime;
mod server;
mod store;

use std::path::PathBuf;

use app::App;
use clap::Parser;
use omarec_core::{AppPaths, Config};
use tracing_subscriber::EnvFilter;

#[derive(Debug, Parser)]
#[command(name = "omarecd", version, about = "omarec per-user recording daemon")]
struct Arguments {
    /// Override the XDG control socket. Intended for tests and development only.
    #[arg(long, env = "OMAREC_SOCKET")]
    socket: Option<PathBuf>,

    /// Override the normal `$XDG_CONFIG_HOME/omarec/config.toml` path.
    #[arg(long, env = "OMAREC_CONFIG")]
    config: Option<PathBuf>,
}

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    tracing_subscriber::fmt()
        .with_env_filter(
            EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new("omarecd=info")),
        )
        .with_target(false)
        .init();

    let arguments = Arguments::parse();
    let mut paths = AppPaths::discover()?;
    if let Some(socket) = arguments.socket {
        paths.control_socket = socket;
    }
    paths.ensure_directories()?;

    let config_path = arguments
        .config
        .unwrap_or_else(|| paths.config_file.clone());
    let config = Config::load(&config_path)?;
    let app = App::new(config, paths.clone());
    app.recover().await?;
    server::run(&paths.control_socket, app).await?;
    Ok(())
}
