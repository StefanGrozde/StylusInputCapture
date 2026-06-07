use std::{env, ffi::OsString, path::PathBuf};

use tablet_stream::Format;

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Source {
    Stdin,
    Tcp(String),
    Pipe(String),
}

#[derive(Clone, Debug, PartialEq)]
pub struct Args {
    pub source: Source,
    pub format: Format,
    pub profile: Option<PathBuf>,
    pub spawn: bool,
}

impl Default for Args {
    fn default() -> Self {
        Self {
            source: Source::Stdin,
            format: Format::Postcard,
            profile: None,
            spawn: false,
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
                    let path = next_value(&mut iter, "--profile")?;
                    parsed.profile = Some(PathBuf::from(path));
                }
                "--spawn" => {
                    parsed.spawn = true;
                }
                flag if flag.starts_with('-') => {
                    return Err(ParseError::UnknownFlag(flag.to_owned()));
                }
                other => {
                    return Err(ParseError::UnexpectedArgument(other.to_owned()));
                }
            }
        }

        Ok(parsed)
    }
}

fn next_value<I>(
    iter: &mut std::iter::Peekable<I>,
    flag: &'static str,
) -> Result<String, ParseError>
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
    if parsed.source != Source::Stdin {
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn defaults_to_stdin_postcard_without_profile_or_spawn() {
        let args = Args::parse_from(std::iter::empty::<&str>()).unwrap();

        assert_eq!(args.source, Source::Stdin);
        assert_eq!(args.format, Format::Postcard);
        assert_eq!(args.profile, None);
        assert!(!args.spawn);
    }

    #[test]
    fn parses_tcp_json_profile_and_spawn() {
        let args = Args::parse_from([
            "--tcp",
            "127.0.0.1:9123",
            "--format",
            "json",
            "--profile",
            "profile.cal.toml",
            "--spawn",
        ])
        .unwrap();

        assert_eq!(args.source, Source::Tcp("127.0.0.1:9123".to_owned()));
        assert_eq!(args.format, Format::Json);
        assert_eq!(args.profile, Some(PathBuf::from("profile.cal.toml")));
        assert!(args.spawn);
    }

    #[test]
    fn parses_pipe_postcard() {
        let args = Args::parse_from(["--pipe", "wacom-capture", "--format", "postcard"]).unwrap();

        assert_eq!(args.source, Source::Pipe("wacom-capture".to_owned()));
        assert_eq!(args.format, Format::Postcard);
    }

    #[test]
    fn rejects_unknown_flag() {
        let error = Args::parse_from(["--unknown"]).unwrap_err();

        assert_eq!(error, ParseError::UnknownFlag("--unknown".to_owned()));
    }

    #[test]
    fn rejects_conflicting_sources() {
        let error = Args::parse_from(["--tcp", "127.0.0.1:9123", "--pipe", "wacom"]).unwrap_err();

        assert_eq!(error, ParseError::ConflictingSources);
    }

    #[test]
    fn rejects_invalid_format() {
        let error = Args::parse_from(["--format", "yaml"]).unwrap_err();

        assert_eq!(error, ParseError::InvalidFormat("yaml".to_owned()));
    }
}
