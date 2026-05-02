//! Bounded interaction with the `git` executable.

use std::io::Read;
use std::process::{Command, Stdio};
use std::thread;
use std::time::{Duration, Instant};

use crate::config::Config;
use crate::error::{Error, Result};
use crate::repo::Repo;

/// A small adapter that runs Git with a sanitized environment, timeout, and
/// bounded stdout/stderr capture.
#[derive(Clone)]
pub struct Git {
    bin: String,
    timeout: Duration,
    max_output_bytes: usize,
}

impl Git {
    /// Create an adapter from runtime configuration.
    pub fn new(config: &Config) -> Self {
        Self {
            bin: config.git_bin().to_string(),
            timeout: config.git_timeout(),
            max_output_bytes: config.max_git_output_bytes(),
        }
    }

    /// Run a Git command for a repository and return UTF-8-lossy stdout.
    pub fn output(&self, repo: &Repo, args: &[&str]) -> Result<String> {
        let mut cmd = Command::new(&self.bin);
        cmd.env_clear()
            .env("PATH", "/usr/bin:/bin:/usr/local/bin")
            .env("GIT_CONFIG_NOSYSTEM", "1")
            .env("GIT_CONFIG_GLOBAL", "/dev/null")
            .env("GIT_OPTIONAL_LOCKS", "0")
            .env("LC_ALL", "C")
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());
        if repo.is_bare() {
            cmd.arg("--git-dir").arg(repo.path());
        } else {
            cmd.arg("-C").arg(repo.path());
        }
        cmd.args(args);

        let mut child = cmd
            .spawn()
            .map_err(|err| Error::Git(format!("spawn failed: {err}")))?;
        let stdout = child
            .stdout
            .take()
            .ok_or_else(|| Error::Git("missing stdout".into()))?;
        let stderr = child
            .stderr
            .take()
            .ok_or_else(|| Error::Git("missing stderr".into()))?;
        let max_stdout = self.max_output_bytes;
        let out_reader = thread::spawn(move || read_limited(stdout, max_stdout));
        let err_reader = thread::spawn(move || read_limited(stderr, 64 * 1024));

        let start = Instant::now();
        let status = loop {
            if let Some(status) = child.try_wait()? {
                break status;
            }
            if start.elapsed() > self.timeout {
                let _ = child.kill();
                let _ = child.wait();
                return Err(Error::Git("command timed out".into()));
            }
            thread::sleep(Duration::from_millis(10));
        };

        let (stdout, stdout_truncated) = out_reader
            .join()
            .map_err(|_| Error::Thread("git stdout reader panicked".into()))??;
        let (stderr, _) = err_reader
            .join()
            .map_err(|_| Error::Thread("git stderr reader panicked".into()))??;

        if stdout_truncated {
            return Err(Error::Git("output too large".into()));
        }
        if status.success() {
            Ok(String::from_utf8_lossy(&stdout).into_owned())
        } else {
            eprintln!(
                "git command failed for repo {}: {}",
                repo.name(),
                String::from_utf8_lossy(&stderr).trim()
            );
            Err(Error::Git("command failed".into()))
        }
    }
}

fn read_limited<R: Read>(mut reader: R, limit: usize) -> std::io::Result<(Vec<u8>, bool)> {
    let mut out = Vec::new();
    let mut buf = [0_u8; 8192];
    let mut truncated = false;
    loop {
        let n = reader.read(&mut buf)?;
        if n == 0 {
            break;
        }
        let remaining = limit.saturating_sub(out.len());
        if remaining == 0 {
            truncated = true;
            continue;
        }
        let take = n.min(remaining);
        out.extend_from_slice(&buf[..take]);
        if take < n {
            truncated = true;
        }
    }
    Ok((out, truncated))
}
