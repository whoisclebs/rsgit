//! Runtime configuration sourced from environment variables.

use std::env;
use std::net::SocketAddr;
use std::path::{Path, PathBuf};
use std::time::Duration;

use crate::error::{Error, Result};

const DEFAULT_ADDR: &str = "127.0.0.1:8080";

/// Immutable runtime configuration for the server.
#[derive(Clone, Debug)]
pub struct Config {
    addr: SocketAddr,
    repo_root: PathBuf,
    public_base: Option<String>,
    git_bin: String,
    git_timeout: Duration,
    max_git_output_bytes: usize,
    max_clone_file_bytes: u64,
}

impl Config {
    /// Build configuration from process environment.
    pub fn from_env() -> Result<Self> {
        let addr = env::var("RSGIT_ADDR").unwrap_or_else(|_| DEFAULT_ADDR.to_string());
        let addr = addr
            .parse::<SocketAddr>()
            .map_err(|err| Error::Config(format!("invalid RSGIT_ADDR: {err}")))?;

        let repo_root = env::var("RSGIT_REPO_ROOT").unwrap_or_else(|_| ".".to_string());
        let repo_root = std::fs::canonicalize(repo_root)?;

        Ok(Self {
            addr,
            repo_root,
            public_base: env::var("RSGIT_PUBLIC_BASE")
                .ok()
                .filter(|v| !v.trim().is_empty()),
            git_bin: env::var("RSGIT_GIT").unwrap_or_else(|_| "git".to_string()),
            git_timeout: Duration::from_secs(5),
            max_git_output_bytes: 2 * 1024 * 1024,
            max_clone_file_bytes: 128 * 1024 * 1024,
        })
    }

    /// Socket address to bind.
    pub fn addr(&self) -> SocketAddr {
        self.addr
    }
    /// Canonical root containing public repositories.
    pub fn repo_root(&self) -> &Path {
        &self.repo_root
    }
    /// Optional public URL base used for clone commands behind reverse proxies.
    pub fn public_base(&self) -> Option<&str> {
        self.public_base.as_deref()
    }
    /// Git executable name or absolute path.
    pub fn git_bin(&self) -> &str {
        &self.git_bin
    }
    /// Maximum runtime for a Git subprocess.
    pub fn git_timeout(&self) -> Duration {
        self.git_timeout
    }
    /// Maximum captured stdout bytes for UI Git commands.
    pub fn max_git_output_bytes(&self) -> usize {
        self.max_git_output_bytes
    }
    /// Maximum object/pack file bytes served for clone endpoints.
    pub fn max_clone_file_bytes(&self) -> u64 {
        self.max_clone_file_bytes
    }
}
