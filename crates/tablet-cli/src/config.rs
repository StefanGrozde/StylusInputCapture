use std::{
    fmt, fs,
    net::SocketAddr,
    path::{Path, PathBuf},
    str::FromStr,
};

use serde::{Deserialize, Serialize};
use tablet_stream::Format;
use thiserror::Error;

use crate::cli::Args;

const DEFAULT_CONFIG_PATH: &str = "wacom-capture.toml";

const DEFAULT_FIELDS: &[&str] = &[
    "x", "y", "pressure", "tilt", "rotation", "tangent", "buttons",
];

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Config {
    pub capture: CaptureConfig,
    pub output: OutputConfig,
    pub telemetry: TelemetryConfig,
}

impl Default for Config {
    fn default() -> Self {
        Self {
            capture: CaptureConfig::default(),
            output: OutputConfig::default(),
            telemetry: TelemetryConfig::default(),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct CaptureConfig {
    pub requested_rate_hz: u32,
    pub queue_size: u32,
    pub flip_y: bool,
    pub fields: Vec<String>,
}

impl Default for CaptureConfig {
    fn default() -> Self {
        Self {
            requested_rate_hz: 200,
            queue_size: 1024,
            flip_y: true,
            fields: DEFAULT_FIELDS
                .iter()
                .map(|field| (*field).to_string())
                .collect(),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct OutputConfig {
    pub transport: Transport,
    pub format: OutputFormat,
    pub tcp_addr: String,
    pub pipe_name: String,
}

impl Default for OutputConfig {
    fn default() -> Self {
        Self {
            transport: Transport::Stdout,
            format: OutputFormat::Postcard,
            tcp_addr: "127.0.0.1:9123".to_string(),
            pipe_name: "wacom-capture".to_string(),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct TelemetryConfig {
    pub metrics_interval_ms: u64,
    pub log_level: LogLevel,
}

impl Default for TelemetryConfig {
    fn default() -> Self {
        Self {
            metrics_interval_ms: 1000,
            log_level: LogLevel::Info,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Transport {
    Stdout,
    Tcp,
    Pipe,
}

impl Transport {
    fn as_str(self) -> &'static str {
        match self {
            Self::Stdout => "stdout",
            Self::Tcp => "tcp",
            Self::Pipe => "pipe",
        }
    }
}

impl fmt::Display for Transport {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

impl FromStr for Transport {
    type Err = ConfigError;

    fn from_str(value: &str) -> Result<Self, Self::Err> {
        match value {
            "stdout" => Ok(Self::Stdout),
            "tcp" => Ok(Self::Tcp),
            "pipe" => Ok(Self::Pipe),
            other => Err(ConfigError::InvalidTransport {
                value: other.to_string(),
            }),
        }
    }
}

impl Serialize for Transport {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: serde::Serializer,
    {
        serializer.serialize_str(self.as_str())
    }
}

impl<'de> Deserialize<'de> for Transport {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        let value = String::deserialize(deserializer)?;
        value.parse().map_err(serde::de::Error::custom)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum OutputFormat {
    Postcard,
    Json,
}

impl OutputFormat {
    fn as_str(self) -> &'static str {
        match self {
            Self::Postcard => "postcard",
            Self::Json => "json",
        }
    }
}

impl From<OutputFormat> for Format {
    fn from(value: OutputFormat) -> Self {
        match value {
            OutputFormat::Postcard => Format::Postcard,
            OutputFormat::Json => Format::Json,
        }
    }
}

impl fmt::Display for OutputFormat {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

impl FromStr for OutputFormat {
    type Err = ConfigError;

    fn from_str(value: &str) -> Result<Self, Self::Err> {
        match value {
            "postcard" => Ok(Self::Postcard),
            "json" => Ok(Self::Json),
            other => Err(ConfigError::InvalidFormat {
                value: other.to_string(),
            }),
        }
    }
}

impl Serialize for OutputFormat {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: serde::Serializer,
    {
        serializer.serialize_str(self.as_str())
    }
}

impl<'de> Deserialize<'de> for OutputFormat {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        let value = String::deserialize(deserializer)?;
        value.parse().map_err(serde::de::Error::custom)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LogLevel {
    Trace,
    Debug,
    Info,
    Warn,
    Error,
}

impl LogLevel {
    fn as_str(self) -> &'static str {
        match self {
            Self::Trace => "trace",
            Self::Debug => "debug",
            Self::Info => "info",
            Self::Warn => "warn",
            Self::Error => "error",
        }
    }
}

impl fmt::Display for LogLevel {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

impl FromStr for LogLevel {
    type Err = ConfigError;

    fn from_str(value: &str) -> Result<Self, Self::Err> {
        match value {
            "trace" => Ok(Self::Trace),
            "debug" => Ok(Self::Debug),
            "info" => Ok(Self::Info),
            "warn" => Ok(Self::Warn),
            "error" => Ok(Self::Error),
            other => Err(ConfigError::InvalidLogLevel {
                value: other.to_string(),
            }),
        }
    }
}

impl Serialize for LogLevel {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: serde::Serializer,
    {
        serializer.serialize_str(self.as_str())
    }
}

impl<'de> Deserialize<'de> for LogLevel {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        let value = String::deserialize(deserializer)?;
        value.parse().map_err(serde::de::Error::custom)
    }
}

#[derive(Debug, Error)]
pub enum ConfigError {
    #[error("failed to read config file '{path}': {source}")]
    ReadConfig {
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },
    #[error("failed to parse TOML config file '{path}': {source}")]
    ParseConfig {
        path: PathBuf,
        #[source]
        source: toml::de::Error,
    },
    #[error("invalid output.transport '{value}'; expected one of: stdout, tcp, pipe")]
    InvalidTransport { value: String },
    #[error("invalid output.format '{value}'; expected one of: postcard, json")]
    InvalidFormat { value: String },
    #[error(
        "invalid telemetry.log_level '{value}'; expected one of: trace, debug, info, warn, error"
    )]
    InvalidLogLevel { value: String },
    #[error("capture.queue_size must be greater than 0")]
    InvalidQueueSize,
    #[error("capture.requested_rate_hz must be greater than 0")]
    InvalidRequestedRate,
    #[error("telemetry.metrics_interval_ms must be greater than 0")]
    InvalidMetricsInterval,
    #[error(
        "output.tcp_addr '{value}' is not a valid socket address; use a value like 127.0.0.1:9123"
    )]
    InvalidTcpAddr { value: String },
    #[error("output.pipe_name must not be empty")]
    EmptyPipeName,
    #[error("capture.fields must contain at least one field")]
    EmptyFields,
}

pub fn load_config(args: &Args) -> Result<Config, ConfigError> {
    let mut config = load_config_file(args.config.as_deref())?.unwrap_or_default();
    apply_cli_overrides(&mut config, args)?;
    config.validate()?;
    Ok(config)
}

fn load_config_file(path: Option<&Path>) -> Result<Option<Config>, ConfigError> {
    let Some(path) = path.or_else(|| {
        let default = Path::new(DEFAULT_CONFIG_PATH);
        default.exists().then_some(default)
    }) else {
        return Ok(None);
    };

    let text = fs::read_to_string(path).map_err(|source| ConfigError::ReadConfig {
        path: path.to_path_buf(),
        source,
    })?;
    let config = toml::from_str(&text).map_err(|source| ConfigError::ParseConfig {
        path: path.to_path_buf(),
        source,
    })?;
    Ok(Some(config))
}

pub fn apply_cli_overrides(config: &mut Config, args: &Args) -> Result<(), ConfigError> {
    if let Some(transport) = &args.transport {
        config.output.transport = transport.parse()?;
    }
    if let Some(format) = &args.format {
        config.output.format = format.parse()?;
    }
    if let Some(tcp_addr) = &args.tcp_addr {
        config.output.tcp_addr.clone_from(tcp_addr);
    }
    if let Some(pipe_name) = &args.pipe_name {
        config.output.pipe_name.clone_from(pipe_name);
    }
    if let Some(queue_size) = args.queue_size {
        config.capture.queue_size = queue_size;
    }
    if let Some(requested_rate_hz) = args.requested_rate_hz {
        config.capture.requested_rate_hz = requested_rate_hz;
    }
    if let Some(flip_y) = args.flip_y {
        config.capture.flip_y = flip_y;
    }
    if let Some(log_level) = &args.log_level {
        config.telemetry.log_level = log_level.parse()?;
    }
    Ok(())
}

impl Config {
    pub fn validate(&self) -> Result<(), ConfigError> {
        if self.capture.queue_size == 0 {
            return Err(ConfigError::InvalidQueueSize);
        }
        if self.capture.requested_rate_hz == 0 {
            return Err(ConfigError::InvalidRequestedRate);
        }
        if self.capture.fields.is_empty() {
            return Err(ConfigError::EmptyFields);
        }
        if self.telemetry.metrics_interval_ms == 0 {
            return Err(ConfigError::InvalidMetricsInterval);
        }
        if self.output.pipe_name.trim().is_empty() {
            return Err(ConfigError::EmptyPipeName);
        }
        self.output
            .tcp_addr
            .parse::<SocketAddr>()
            .map_err(|_| ConfigError::InvalidTcpAddr {
                value: self.output.tcp_addr.clone(),
            })?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn args() -> Args {
        Args {
            transport: None,
            format: None,
            tcp_addr: None,
            pipe_name: None,
            config: None,
            queue_size: None,
            requested_rate_hz: None,
            flip_y: None,
            log_level: None,
        }
    }

    #[test]
    fn defaults_match_spec_section_9() {
        let config = Config::default();
        assert_eq!(config.capture.requested_rate_hz, 200);
        assert_eq!(config.capture.queue_size, 1024);
        assert!(config.capture.flip_y);
        assert_eq!(config.capture.fields, DEFAULT_FIELDS);
        assert_eq!(config.output.transport, Transport::Stdout);
        assert_eq!(config.output.format, OutputFormat::Postcard);
        assert_eq!(config.output.tcp_addr, "127.0.0.1:9123");
        assert_eq!(config.output.pipe_name, "wacom-capture");
        assert_eq!(config.telemetry.metrics_interval_ms, 1000);
        assert_eq!(config.telemetry.log_level, LogLevel::Info);
    }

    #[test]
    fn toml_loads_and_cli_overrides_win() {
        let toml = r#"
            [capture]
            requested_rate_hz = 133
            queue_size = 64
            flip_y = false

            [output]
            transport = "stdout"
            format = "postcard"
            tcp_addr = "127.0.0.1:9000"
            pipe_name = "from-file"

            [telemetry]
            log_level = "debug"
        "#;
        let mut config: Config = toml::from_str(toml).expect("valid config");
        let mut args = args();
        args.transport = Some("tcp".to_string());
        args.format = Some("json".to_string());
        args.queue_size = Some(2048);
        args.requested_rate_hz = Some(250);
        args.flip_y = Some(true);
        args.log_level = Some("warn".to_string());

        apply_cli_overrides(&mut config, &args).expect("CLI overrides apply");
        config.validate().expect("merged config is valid");

        assert_eq!(config.output.transport, Transport::Tcp);
        assert_eq!(config.output.format, OutputFormat::Json);
        assert_eq!(config.capture.queue_size, 2048);
        assert_eq!(config.capture.requested_rate_hz, 250);
        assert!(config.capture.flip_y);
        assert_eq!(config.telemetry.log_level, LogLevel::Warn);
        assert_eq!(config.output.tcp_addr, "127.0.0.1:9000");
        assert_eq!(config.output.pipe_name, "from-file");
    }

    #[test]
    fn invalid_transport_format_and_log_level_are_rejected() {
        assert!(matches!(
            "udp".parse::<Transport>(),
            Err(ConfigError::InvalidTransport { .. })
        ));
        assert!(matches!(
            "xml".parse::<OutputFormat>(),
            Err(ConfigError::InvalidFormat { .. })
        ));
        assert!(matches!(
            "verbose".parse::<LogLevel>(),
            Err(ConfigError::InvalidLogLevel { .. })
        ));
    }

    #[test]
    fn output_format_converts_to_stream_format() {
        assert_eq!(Format::from(OutputFormat::Postcard), Format::Postcard);
        assert_eq!(Format::from(OutputFormat::Json), Format::Json);
    }
}
