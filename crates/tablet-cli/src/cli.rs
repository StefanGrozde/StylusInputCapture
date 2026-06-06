use std::path::PathBuf;

use clap::Parser;

#[derive(Debug, Parser)]
#[command(
    author,
    version,
    about = "Capture Wacom tablet input and stream pen samples."
)]
pub struct Args {
    #[arg(long)]
    pub transport: Option<String>,

    #[arg(long)]
    pub format: Option<String>,

    #[arg(long)]
    pub tcp_addr: Option<String>,

    #[arg(long)]
    pub pipe_name: Option<String>,

    #[arg(long)]
    pub config: Option<PathBuf>,

    #[arg(long)]
    pub queue_size: Option<u32>,

    #[arg(long)]
    pub requested_rate_hz: Option<u32>,

    #[arg(
        long,
        num_args = 0..=1,
        default_missing_value = "true",
        value_parser = clap::builder::BoolishValueParser::new()
    )]
    pub flip_y: Option<bool>,

    #[arg(long)]
    pub log_level: Option<String>,
}
