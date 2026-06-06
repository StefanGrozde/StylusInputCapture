use std::{
    env,
    io::{self, BufReader, Read},
    net::TcpStream,
    process,
};

use tablet_core::{DeviceCapabilities, PenSample};
use tablet_stream::{Format, FrameReader, Metrics, StreamError, StreamMessage};

#[derive(Debug)]
struct Args {
    tcp_addr: Option<String>,
    format: Format,
}

fn main() {
    let args = match Args::parse(env::args().skip(1)) {
        Ok(args) => args,
        Err(ArgError::Help) => {
            print_help();
            return;
        }
        Err(ArgError::Message(message)) => {
            eprintln!("{message}");
            eprintln!("run with --help for usage");
            process::exit(2);
        }
    };

    let result = if let Some(addr) = args.tcp_addr {
        TcpStream::connect(&addr)
            .map_err(|error| format!("failed to connect to {addr}: {error}"))
            .and_then(|stream| consume(stream, args.format).map_err(|error| error.to_string()))
    } else {
        let stdin = io::stdin();
        consume(stdin.lock(), args.format).map_err(|error| error.to_string())
    };

    if let Err(error) = result {
        eprintln!("{error}");
        process::exit(1);
    }
}

impl Args {
    fn parse<I>(mut args: I) -> Result<Self, ArgError>
    where
        I: Iterator<Item = String>,
    {
        let mut parsed = Self {
            tcp_addr: None,
            format: Format::Postcard,
        };

        while let Some(arg) = args.next() {
            match arg.as_str() {
                "-h" | "--help" => return Err(ArgError::Help),
                "--tcp" => {
                    let addr = args.next().ok_or_else(|| {
                        ArgError::Message(
                            "--tcp requires an address, e.g. --tcp 127.0.0.1:9123".to_string(),
                        )
                    })?;
                    parsed.tcp_addr = Some(addr);
                }
                "--format" => {
                    let format = args.next().ok_or_else(|| {
                        ArgError::Message("--format requires postcard or json".to_string())
                    })?;
                    parsed.format = parse_format(&format)?;
                }
                other => {
                    return Err(ArgError::Message(format!("unknown argument: {other}")));
                }
            }
        }

        Ok(parsed)
    }
}

#[derive(Debug)]
enum ArgError {
    Help,
    Message(String),
}

fn parse_format(value: &str) -> Result<Format, ArgError> {
    match value {
        "postcard" => Ok(Format::Postcard),
        "json" => Ok(Format::Json),
        other => Err(ArgError::Message(format!(
            "unsupported format {other:?}; expected postcard or json"
        ))),
    }
}

fn consume<R: Read>(reader: R, format: Format) -> Result<(), StreamError> {
    let mut reader = match format {
        Format::Postcard => FrameReader::new_from_header(reader)?,
        Format::Json => FrameReader::new_json(BufReader::new(reader)),
    };
    let mut printed_sample_header = false;

    loop {
        let message = match reader.read_message() {
            Ok(message) => message,
            Err(StreamError::TruncatedFrame) => return Ok(()),
            Err(error) => return Err(error),
        };

        match message {
            StreamMessage::Capabilities(caps) => print_capabilities(&caps),
            StreamMessage::Sample(sample) => {
                if !printed_sample_header {
                    println!("sample,t_capture_ns,x_raw,y_raw,pressure_raw,serial");
                    printed_sample_header = true;
                }
                print_sample(&sample);
            }
            StreamMessage::Proximity {
                in_range,
                tool_serial,
            } => {
                println!("proximity,in_range={in_range},tool_serial={tool_serial}");
            }
            StreamMessage::Metrics(metrics) => print_metrics(&metrics),
            StreamMessage::Heartbeat => println!("heartbeat"),
        }
    }
}

fn print_help() {
    println!(
        "\
tablet-stream consumer

Usage:
  cargo run -p tablet-stream --example consumer -- [--tcp <addr>] [--format <postcard|json>]

Options:
  --tcp <addr>              Connect to a TCP stream instead of reading stdin
  --format <postcard|json>  Decode framed postcard binary or JSONL (default: postcard)
  -h, --help                Show this help text

Examples:
  cargo run -p tablet-cli -- --transport stdout | cargo run -p tablet-stream --example consumer
  cargo run -p tablet-stream --example consumer -- --tcp 127.0.0.1:9123 --format json"
    );
}

fn print_capabilities(caps: &DeviceCapabilities) {
    println!("capabilities");
    println!("  device_name={}", caps.device_name);
    println!("  driver_version={}", caps.driver_version);
    println!(
        "  x={}..{} supported={}",
        caps.x.min, caps.x.max, caps.x.supported
    );
    println!(
        "  y={}..{} supported={}",
        caps.y.min, caps.y.max, caps.y.supported
    );
    println!(
        "  pressure={}..{} supported={}",
        caps.pressure.min, caps.pressure.max, caps.pressure.supported
    );
    println!("  max_packet_rate_hz={}", caps.max_packet_rate_hz);
    println!("  queue_size={}", caps.queue_size);
}

fn print_sample(sample: &PenSample) {
    println!(
        "sample,{},{},{},{},{}",
        sample.t_capture_ns, sample.x_raw, sample.y_raw, sample.pressure_raw, sample.serial
    );
}

fn print_metrics(metrics: &Metrics) {
    println!(
        "metrics,packets_per_sec={:.2},dropped={},queue_depth={},actual_rate_hz={},requested_rate_hz={},connected_clients={}",
        metrics.packets_per_sec,
        metrics.dropped,
        metrics.queue_depth,
        metrics.actual_rate_hz,
        metrics.requested_rate_hz,
        metrics.connected_clients
    );
}
