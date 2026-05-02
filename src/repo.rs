//! Repository discovery and safe path containment.

use std::fs;
use std::path::{Path, PathBuf};

use crate::config::Config;
use crate::security::safe_repo_name;

/// A repository visible to rsgit.
#[derive(Clone, Debug)]
pub struct Repo {
    name: String,
    path: PathBuf,
    bare: bool,
}

impl Repo {
    /// Repository display/URL name.
    pub fn name(&self) -> &str {
        &self.name
    }
    /// Canonical repository working-tree or bare Git dir path.
    pub fn path(&self) -> &Path {
        &self.path
    }
    /// True when this repository is bare.
    pub fn is_bare(&self) -> bool {
        self.bare
    }
    /// Canonical Git directory path.
    pub fn git_dir(&self) -> PathBuf {
        if self.bare {
            self.path.clone()
        } else {
            self.path.join(".git")
        }
    }
}

/// List repositories one level below the configured root.
pub fn list(config: &Config) -> Vec<Repo> {
    let mut repos = Vec::new();
    let Ok(entries) = fs::read_dir(config.repo_root()) else {
        return repos;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        let Ok(meta) = fs::symlink_metadata(&path) else {
            continue;
        };
        if meta.file_type().is_symlink() || !meta.is_dir() {
            continue;
        }
        let Some(name) = entry.file_name().to_str().map(str::to_string) else {
            continue;
        };
        if name.starts_with('.') {
            continue;
        }
        if let Some(repo) = from_path(config, name, path) {
            repos.push(repo);
        }
    }
    repos
}

/// Find a repository by safe URL name.
pub fn find(config: &Config, name: &str) -> Option<Repo> {
    if !safe_repo_name(name) {
        return None;
    }
    from_path(config, name.to_string(), config.repo_root().join(name))
}

fn from_path(config: &Config, name: String, path: PathBuf) -> Option<Repo> {
    let meta = fs::symlink_metadata(&path).ok()?;
    if meta.file_type().is_symlink() || !meta.is_dir() {
        return None;
    }
    let path = fs::canonicalize(path).ok()?;
    if !path.starts_with(config.repo_root()) {
        return None;
    }
    let normal = path.join(".git");
    if normal.is_dir()
        && fs::symlink_metadata(&normal)
            .map(|m| !m.file_type().is_symlink())
            .unwrap_or(false)
    {
        return Some(Repo {
            name,
            path,
            bare: false,
        });
    }
    if path.join("HEAD").is_file() && path.join("objects").is_dir() {
        return Some(Repo {
            name,
            path,
            bare: true,
        });
    }
    None
}
