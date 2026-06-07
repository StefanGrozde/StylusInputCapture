//! Optional convenience: launch `tablet-cli` as a child process and read its
//! stdout as a stream source (SPEC_CUI.md §5.2, TC3.5).
//!
//! This module is **process orchestration only** — it never touches capture
//! code. The child is started with `--transport stdout --format postcard` (the
//! UI always treats a spawned producer as postcard, regardless of `--format`,
//! per the task brief) plus any passthrough capture flags the caller supplies,
//! and its stdout pipe is handed to the existing reader path exactly like the
//! `Stdin` source.

use std::{
    env,
    ffi::OsString,
    io,
    path::PathBuf,
    process::{Child, ChildStdout, Command, Stdio},
};

/// Optional passthrough capture flags forwarded to the spawned `tablet-cli`
/// (SPEC_1.md capture knobs the user may want to tune without leaving the UI).
/// All fields are optional; an all-`None` value still produces a working
/// invocation (`--transport stdout --format postcard` only).
#[derive(Clone, Debug, Default, PartialEq)]
pub struct SpawnOptions {
    pub queue_size: Option<u32>,
    pub requested_rate_hz: Option<u32>,
    pub flip_y: Option<bool>,
}

/// A spawned `tablet-cli` child plus its stdout pipe, ready to be consumed as
/// a `Read` source.
pub struct SpawnedProducer {
    child: Child,
    stdout: Option<ChildStdout>,
}

impl SpawnedProducer {
    /// Take ownership of the child's stdout pipe. Returns `None` if it was
    /// already taken (or somehow missing) — callers should take it exactly
    /// once, immediately after spawning.
    pub fn take_stdout(&mut self) -> Option<ChildStdout> {
        self.stdout.take()
    }

    /// Terminate the child and reap it so no orphan process remains
    /// (TC3.5 acceptance: "a spawned child process is cleaned up on UI
    /// exit"). Best-effort: a process that already exited is not an error.
    pub fn kill_and_wait(&mut self) {
        if let Err(error) = self.child.kill() {
            if error.kind() != io::ErrorKind::InvalidInput {
                eprintln!("failed to kill spawned tablet-cli: {error}");
            }
        }
        if let Err(error) = self.child.wait() {
            eprintln!("failed to wait for spawned tablet-cli: {error}");
        }
    }
}

impl Drop for SpawnedProducer {
    fn drop(&mut self) {
        // Defensive: ensure the child is reaped even if `kill_and_wait` was
        // never called explicitly (e.g. an early error path).
        let _ = self.child.try_wait();
    }
}

/// Spawn `tablet-cli` configured to stream postcard over stdout, returning the
/// child handle (with its stdout pipe available via [`SpawnedProducer::take_stdout`]).
///
/// Resolution order for the binary (TC3.5 step 2):
/// 1. `<dir of current_exe>/tablet-cli[.exe]` — the sibling-binary convention
///    used by Cargo workspaces (both binaries land in the same `target/<profile>`).
/// 2. Fall back to bare `"tablet-cli"`, resolved via `PATH` by the OS loader.
pub fn spawn_producer(options: &SpawnOptions) -> io::Result<SpawnedProducer> {
    let binary = resolve_tablet_cli_path();

    let mut command = Command::new(&binary);
    command
        .args(["--transport", "stdout", "--format", "postcard"])
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::inherit());

    append_passthrough_args(&mut command, options);

    let mut child = command.spawn().map_err(|error| {
        io::Error::new(
            error.kind(),
            format!("failed to spawn '{}': {error}", binary.display()),
        )
    })?;

    let stdout = child.stdout.take();

    Ok(SpawnedProducer { child, stdout })
}

/// Append the passthrough capture flags (SPEC_1.md capture knobs) that were
/// requested. Kept as a free function so the argument-building logic is
/// testable without spawning a process.
fn append_passthrough_args(command: &mut Command, options: &SpawnOptions) {
    for arg in passthrough_args(options) {
        command.arg(arg);
    }
}

/// Pure helper: build the passthrough argument vector for the given options.
/// Factored out so it can be unit-tested without touching `std::process`.
fn passthrough_args(options: &SpawnOptions) -> Vec<OsString> {
    let mut args = Vec::new();

    if let Some(queue_size) = options.queue_size {
        args.push(OsString::from("--queue-size"));
        args.push(OsString::from(queue_size.to_string()));
    }
    if let Some(rate) = options.requested_rate_hz {
        args.push(OsString::from("--requested-rate-hz"));
        args.push(OsString::from(rate.to_string()));
    }
    if let Some(flip_y) = options.flip_y {
        args.push(OsString::from("--flip-y"));
        args.push(OsString::from(if flip_y { "true" } else { "false" }));
    }

    args
}

/// Resolve the `tablet-cli` binary path: prefer the sibling of the running
/// executable (same directory as `tablet-ui[.exe]`, the layout `cargo build`
/// produces for a workspace), falling back to a bare name resolved via `PATH`.
fn resolve_tablet_cli_path() -> PathBuf {
    let exe_name = if cfg!(windows) {
        "tablet-cli.exe"
    } else {
        "tablet-cli"
    };

    if let Ok(current_exe) = env::current_exe() {
        if let Some(dir) = current_exe.parent() {
            let candidate = dir.join(exe_name);
            if candidate.is_file() {
                return candidate;
            }
        }
    }

    PathBuf::from(if cfg!(windows) {
        "tablet-cli.exe"
    } else {
        "tablet-cli"
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn passthrough_args_empty_when_no_options_set() {
        let options = SpawnOptions::default();
        assert!(passthrough_args(&options).is_empty());
    }

    #[test]
    fn passthrough_args_forward_queue_size_and_rate_and_flip_y() {
        let options = SpawnOptions {
            queue_size: Some(1024),
            requested_rate_hz: Some(200),
            flip_y: Some(true),
        };

        let args: Vec<String> = passthrough_args(&options)
            .into_iter()
            .map(|arg| arg.into_string().unwrap())
            .collect();

        assert_eq!(
            args,
            vec![
                "--queue-size".to_owned(),
                "1024".to_owned(),
                "--requested-rate-hz".to_owned(),
                "200".to_owned(),
                "--flip-y".to_owned(),
                "true".to_owned(),
            ]
        );
    }

    #[test]
    fn passthrough_args_render_flip_y_false() {
        let options = SpawnOptions {
            flip_y: Some(false),
            ..SpawnOptions::default()
        };

        let args: Vec<String> = passthrough_args(&options)
            .into_iter()
            .map(|arg| arg.into_string().unwrap())
            .collect();

        assert_eq!(args, vec!["--flip-y".to_owned(), "false".to_owned()]);
    }

    #[test]
    fn resolves_to_a_non_empty_path() {
        // We can't assert the exact path (depends on the test binary's
        // location and whether a sibling tablet-cli exists), but resolution
        // must always produce *some* candidate — never panic, never empty.
        let resolved = resolve_tablet_cli_path();
        assert!(!resolved.as_os_str().is_empty());
    }
}
