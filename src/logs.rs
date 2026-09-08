use std::{io, os::unix::process::CommandExt, path::PathBuf, process::Command};

use clap::Args;

use crate::platform::service::Service;

#[derive(Clone, Debug, Args)]
pub struct LogArgs {
    /// Continues streaming new output; Ctrl-C stops the reader, not the daemon.
    #[arg(short, long)]
    follow: bool,

    /// Lines to read from each stream before following (all, or a nonnegative integer).
    #[arg(short = 'n', long, default_value = "all", value_parser = parse_tail)]
    tail: String,
}

fn parse_tail(value: &str) -> Result<String, String> {
    if value == "all" {
        return Ok("+1".into());
    }
    value
        .parse::<u64>()
        .map(|count| count.to_string())
        .map_err(|_| "expected 'all' or a nonnegative integer".into())
}

pub fn run(service: &Service, args: &LogArgs) -> io::Result<()> {
    let mut paths = service.log_paths()?;
    if paths.is_empty() {
        return Err(io::Error::new(
            io::ErrorKind::NotFound,
            "the installed launch agent does not capture stdout or stderr",
        ));
    }
    if !args.follow {
        paths = existing_log_paths(paths)?;
    }
    if paths.is_empty() {
        return Err(io::Error::new(
            io::ErrorKind::NotFound,
            "no captured service logs found; foreground launches only log to their terminal",
        ));
    }
    // exec preserves tail's exit status and signal handling without an orphaned follower.
    Err(tail_command(&paths, args).exec())
}

fn existing_log_paths(paths: Vec<PathBuf>) -> io::Result<Vec<PathBuf>> {
    let mut existing = Vec::new();
    for path in paths {
        match path.metadata() {
            Ok(metadata) if metadata.is_file() => existing.push(path),
            Ok(_) => {
                return Err(io::Error::new(
                    io::ErrorKind::InvalidInput,
                    format!("not a log file: {}", path.display()),
                ));
            }
            Err(error) if error.kind() == io::ErrorKind::NotFound => {}
            Err(error) => {
                return Err(io::Error::new(
                    error.kind(),
                    format!("{}: {error}", path.display()),
                ));
            }
        }
    }
    Ok(existing)
}

fn tail_command(paths: &[PathBuf], args: &LogArgs) -> Command {
    let mut command = Command::new("/usr/bin/tail");
    command.args(["-q", "-n", &args.tail]);
    if args.follow {
        // macOS tail -F also reopens a replaced file after log rotation.
        command.arg("-F");
    }
    command.arg("--");
    command.args(paths);
    command
}

#[cfg(test)]
mod tests {
    use super::*;
    use clap::Parser;
    use std::fs;

    #[test]
    fn cli_accepts_defaults_alias_follow_and_tail() {
        let cli = crate::Spool::try_parse_from(["spool", "log"]).unwrap();
        let Some(crate::SubCmd::Log(args)) = cli.subcmd else {
            panic!("expected log command");
        };
        assert!(!args.follow);
        assert_eq!(args.tail, "+1");
        let cli = crate::Spool::try_parse_from(["spool", "logs", "-f", "--tail", "0"]).unwrap();
        let Some(crate::SubCmd::Log(args)) = cli.subcmd else {
            panic!("expected log command");
        };
        assert!(args.follow);
        assert_eq!(args.tail, "0");
        assert!(crate::Spool::try_parse_from(["spool", "log", "--tail", "oops"]).is_err());
        assert!(crate::Spool::try_parse_from(["spool", "log", "--tail=-1"]).is_err());
    }

    #[test]
    fn snapshot_reads_all_or_last_lines_without_headers() {
        let path = std::env::temp_dir().join(format!("spool-log-{}", uuid::Uuid::new_v4()));
        fs::write(&path, b"first\nsecond\nthird\n").unwrap();
        let _cleanup = scopeguard::guard(path.clone(), |path| {
            let _ = fs::remove_file(path);
        });
        for (tail, expected) in [
            ("all", "first\nsecond\nthird\n"),
            ("1", "third\n"),
            ("0", ""),
        ] {
            let output = tail_command(
                std::slice::from_ref(&path),
                &LogArgs {
                    follow: false,
                    tail: parse_tail(tail).unwrap(),
                },
            )
            .output()
            .unwrap();
            assert!(output.status.success());
            assert_eq!(output.stdout, expected.as_bytes());
            assert!(output.stderr.is_empty());
        }
    }

    #[test]
    fn snapshot_skips_absent_streams_but_rejects_directories() {
        let dir = std::env::temp_dir().join(format!("spool-log-paths-{}", uuid::Uuid::new_v4()));
        fs::create_dir(&dir).unwrap();
        let _cleanup = scopeguard::guard(dir.clone(), |dir| {
            let _ = fs::remove_dir_all(dir);
        });
        let missing = dir.join("missing");
        assert!(
            existing_log_paths(vec![missing.clone()])
                .unwrap()
                .is_empty()
        );
        let path = dir.join("stderr.log");
        fs::write(&path, b"error\n").unwrap();
        assert_eq!(
            existing_log_paths(vec![missing, path.clone()]).unwrap(),
            vec![path]
        );
        assert_eq!(
            existing_log_paths(vec![dir]).unwrap_err().kind(),
            io::ErrorKind::InvalidInput
        );
    }

    #[test]
    fn follow_reads_appends_and_reopens_rotated_file() {
        use std::io::{BufRead, BufReader, Write};
        use std::process::Stdio;
        use std::sync::mpsc;
        use std::time::Duration;

        let dir = std::env::temp_dir().join(format!("spool-follow-{}", uuid::Uuid::new_v4()));
        fs::create_dir(&dir).unwrap();
        let _cleanup = scopeguard::guard(dir.clone(), |dir| {
            let _ = fs::remove_dir_all(dir);
        });
        let path = dir.join("current.log");
        fs::write(&path, b"ready\n").unwrap();
        let child = tail_command(
            std::slice::from_ref(&path),
            &LogArgs {
                follow: true,
                tail: "+1".into(),
            },
        )
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .unwrap();
        let mut child = scopeguard::guard(child, |mut child| {
            let _ = child.kill();
            let _ = child.wait();
        });
        let stdout = child.stdout.take().unwrap();
        let (sender, receiver) = mpsc::channel();
        let reader = std::thread::spawn(move || {
            for line in BufReader::new(stdout).lines() {
                if sender.send(line.unwrap()).is_err() {
                    break;
                }
            }
        });
        let next = || receiver.recv_timeout(Duration::from_secs(10)).unwrap();
        assert_eq!(next(), "ready");
        let mut file = fs::OpenOptions::new().append(true).open(&path).unwrap();
        file.write_all(b"appended\n").unwrap();
        assert_eq!(next(), "appended");
        fs::rename(&path, dir.join("previous.log")).unwrap();
        fs::write(&path, b"rotated\n").unwrap();
        assert_eq!(next(), "rotated");
        drop(child);
        reader.join().unwrap();
    }
}
