//! Command-line argument parsing for the MPE HUD.
//!
//! Mirrors `tablet-ui`'s hand-rolled parser (same `--tcp` / `--pipe` /
//! `--format` / `--spawn` conventions) and adds `--profile` (a calibration
//! profile to clean up the raw signal) and `--mapping` (a `*.midimap.toml`).

use std::{env, ffi::OsString, path::PathBuf};

use tablet_consumer::Source;
use tablet_stream::Format;

#[derive(Clone, Debug, PartialEq)]
pub struct Args {
    pub source: Source,
    pub format: Format,
    pub spawn: bool,
    pub profile: Option<PathBuf>,
    pub mapping: Option<PathBuf>,
}

impl Default for Args {
    fn default() -> Self {
        Self {
            source: Source::Stdin,
            format: Format::Postcard,
            spawn: false,
            profile: None,
            mapping: None,
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ParseError {
    MissingValue { flag: &'static str },
    InvalidFormat(String),
    ConflictingSources,
    UnknownFlag(String),
    UnexpectedArgument(String),
    NonUtf8Argument,
}

impl std::fmt::Display for ParseError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::MissingValue { flag } => write!(f, "missing value for {flag}"),
            Self::InvalidFormat(value) => {
                write!(f, "invalid --format '{value}', expected postcard or json")
            }
            Self::ConflictingSources => write!(f, "only one of --tcp or --pipe may be specified"),
            Self::UnknownFlag(flag) => write!(f, "unknown flag {flag}"),
            Self::UnexpectedArgument(arg) => write!(f, "unexpected argument {arg}"),
            Self::NonUtf8Argument => write!(f, "arguments must be valid UTF-8"),
        }
    }
}

impl std::error::Error for ParseError {}

impl Args {
    pub fn parse_env() -> Result<Self, ParseError> {
        Self::parse_from(env::args_os().skip(1))
    }

    pub fn parse_from<I, S>(args: I) -> Result<Self, ParseError>
    where
        I: IntoIterator<Item = S>,
        S: Into<OsString>,
    {
        let mut parsed = Args::default();
        let mut iter = args.into_iter().map(Into::into).peekable();

        while let Some(raw) = iter.next() {
            let arg = raw.into_string().map_err(|_| ParseError::NonUtf8Argument)?;
            match arg.as_str() {
                "--stdin" => set_source(&mut parsed, Source::Stdin)?,
                "--tcp" => {
                    let addr = next_value(&mut iter, "--tcp")?;
                    set_source(&mut parsed, Source::Tcp(addr))?;
                }
                "--pipe" => {
                    let name = next_value(&mut iter, "--pipe")?;
                    set_source(&mut parsed, Source::Pipe(name))?;
                }
                "--format" => {
                    let value = next_value(&mut iter, "--format")?;
                    parsed.format = parse_format(&value)?;
                }
                "--profile" => {
                    parsed.profile = Some(PathBuf::from(next_value(&mut iter, "--profile")?));
                }
                "--mapping" => {
                    parsed.mapping = Some(PathBuf::from(next_value(&mut iter, "--mapping")?));
                }
                "--spawn" => parsed.spawn = true,
                flag if flag.starts_with('-') => {
                    return Err(ParseError::UnknownFlag(flag.to_owned()));
                }
                other => return Err(ParseError::UnexpectedArgument(other.to_owned())),
            }
        }

        Ok(parsed)
    }
}

fn next_value<I>(iter: &mut std::iter::Peekable<I>, flag: &'static str) -> Result<String, ParseError>
where
    I: Iterator<Item = OsString>,
{
    let value = iter.next().ok_or(ParseError::MissingValue { flag })?;
    let value = value
        .into_string()
        .map_err(|_| ParseError::NonUtf8Argument)?;
    if value.starts_with('-') {
        return Err(ParseError::MissingValue { flag });
    }
    Ok(value)
}

fn set_source(parsed: &mut Args, source: Source) -> Result<(), ParseError> {
    // `--stdin` re-selecting stdin is a no-op; any other change away from a
    // previously-set non-stdin source is a conflict.
    if parsed.source != Source::Stdin && source != Source::Stdin && parsed.source != source {
        return Err(ParseError::ConflictingSources);
    }
    parsed.source = source;
    Ok(())
}

fn parse_format(value: &str) -> Result<Format, ParseError> {
    match value {
        "postcard" => Ok(Format::Postcard),
        "json" => Ok(Format::Json),
        _ => Err(ParseError::InvalidFormat(value.to_owned())),
    }
}

pub fn format_label(format: Format) -> &'static str {
    match format {
        Format::Postcard => "postcard",
        Format::Json => "json",
    }
}

pub fn source_label(source: &Source) -> String {
    match source {
        Source::Stdin => "stdin".to_owned(),
        Source::Tcp(addr) => format!("tcp: {addr}"),
        Source::Pipe(name) => format!("pipe: {name}"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn defaults_to_stdin_postcard() {
        let args = Args::parse_from(std::iter::empty::<&str>()).unwrap();
        assert_eq!(args.source, Source::Stdin);
        assert_eq!(args.format, Format::Postcard);
        assert!(!args.spawn);
        assert_eq!(args.profile, None);
        assert_eq!(args.mapping, None);
    }

    #[test]
    fn parses_tcp_mapping_and_spawn() {
        let args = Args::parse_from([
            "--tcp",
            "127.0.0.1:9123",
            "--mapping",
            "lead.midimap.toml",
            "--spawn",
        ])
        .unwrap();
        assert_eq!(args.source, Source::Tcp("127.0.0.1:9123".to_owned()));
        assert_eq!(args.mapping, Some(PathBuf::from("lead.midimap.toml")));
        assert!(args.spawn);
    }

    #[test]
    fn rejects_conflicting_sources() {
        let err = Args::parse_from(["--tcp", "127.0.0.1:9123", "--pipe", "wacom"]).unwrap_err();
        assert_eq!(err, ParseError::ConflictingSources);
    }

    #[test]
    fn rejects_invalid_format() {
        assert_eq!(
            Args::parse_from(["--format", "yaml"]).unwrap_err(),
            ParseError::InvalidFormat("yaml".to_owned())
        );
    }
}
