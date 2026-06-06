mod cli;
mod config;
mod metrics;
mod runtime;

use clap::Parser;
use tracing_subscriber::EnvFilter;

use crate::{
    cli::Args,
    config::{load_config, Config},
};

fn main() {
    let args = Args::parse();

    match load_config(&args) {
        Ok(config) => {
            init_tracing(&config);
            if let Err(error) = runtime::run(config) {
                eprintln!("runtime error: {error}");
                std::process::exit(1);
            }
        }
        Err(error) => {
            eprintln!("configuration error: {error}");
            std::process::exit(2);
        }
    }
}

fn init_tracing(config: &Config) {
    let filter = EnvFilter::try_new(config.telemetry.log_level.to_string())
        .unwrap_or_else(|_| EnvFilter::new("info"));

    tracing_subscriber::fmt()
        .with_env_filter(filter)
        .with_writer(std::io::stderr)
        .init();
}
