//! Manual, filesystem-only Git metadata reader.
//!
//! This module never spawns `git` or any other process. It only reads Git's
//! on-disk metadata that is needed for the lightweight UI and dumb HTTP clone
//! endpoints. Full commit/tree/blob parsing is intentionally not implemented
//! yet because Git object contents are zlib-compressed and pack/delta handling
//! requires a larger native object database implementation.

use std::collections::BTreeMap;
use std::fs;

use crate::repo::Repo;

/// A Git ref resolved from loose refs or `packed-refs`.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct RefInfo {
    /// Ref name such as `refs/heads/main`.
    pub name: String,
    /// Hex object id for the ref target.
    pub oid: String,
}

/// Read the repository description file when present.
pub fn description(repo: &Repo) -> String {
    let path = if repo.is_bare() {
        repo.path().join("description")
    } else {
        repo.git_dir().join("description")
    };
    fs::read_to_string(path)
        .ok()
        .and_then(|s| s.lines().next().map(str::to_string))
        .filter(|s| !s.starts_with("Unnamed repository"))
        .unwrap_or_default()
}

/// Read the raw HEAD file.
pub fn head_bytes(repo: &Repo) -> Option<Vec<u8>> {
    fs::read(repo.git_dir().join("HEAD")).ok()
}

/// Return refs in deterministic name order.
pub fn refs(repo: &Repo) -> Vec<RefInfo> {
    let mut refs = BTreeMap::new();
    read_loose_refs(repo, &mut refs);
    read_packed_refs(repo, &mut refs);
    refs.into_iter()
        .map(|(name, oid)| RefInfo { name, oid })
        .collect()
}

/// Return branch refs only.
pub fn branches(repo: &Repo) -> Vec<RefInfo> {
    refs(repo)
        .into_iter()
        .filter(|r| r.name.starts_with("refs/heads/"))
        .collect()
}

/// Return refs formatted for dumb HTTP `info/refs`.
pub fn info_refs(repo: &Repo) -> Vec<u8> {
    let mut out = Vec::new();
    for r in refs(repo) {
        out.extend_from_slice(r.oid.as_bytes());
        out.push(b'\t');
        out.extend_from_slice(r.name.as_bytes());
        out.push(b'\n');
    }
    out
}

fn read_loose_refs(repo: &Repo, refs: &mut BTreeMap<String, String>) {
    let root = repo.git_dir().join("refs");
    visit_ref_dir(&root, "refs", refs);
}

fn visit_ref_dir(dir: &std::path::Path, prefix: &str, refs: &mut BTreeMap<String, String>) {
    let Ok(entries) = fs::read_dir(dir) else {
        return;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        let Some(name) = entry.file_name().to_str().map(str::to_string) else {
            continue;
        };
        let ref_name = format!("{prefix}/{name}");
        if path.is_dir() {
            visit_ref_dir(&path, &ref_name, refs);
        } else if let Ok(oid) = fs::read_to_string(&path) {
            let oid = oid.trim();
            if is_hex_oid(oid) {
                refs.insert(ref_name, oid.to_string());
            }
        }
    }
}

fn read_packed_refs(repo: &Repo, refs: &mut BTreeMap<String, String>) {
    let Ok(text) = fs::read_to_string(repo.git_dir().join("packed-refs")) else {
        return;
    };
    for line in text.lines() {
        if line.is_empty() || line.starts_with('#') || line.starts_with('^') {
            continue;
        }
        let mut parts = line.split_whitespace();
        let (Some(oid), Some(name)) = (parts.next(), parts.next()) else {
            continue;
        };
        if is_hex_oid(oid) {
            refs.entry(name.to_string())
                .or_insert_with(|| oid.to_string());
        }
    }
}

fn is_hex_oid(value: &str) -> bool {
    matches!(value.len(), 40 | 64) && value.bytes().all(|b| b.is_ascii_hexdigit())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn validates_sha1_and_sha256_lengths() {
        assert!(is_hex_oid("0123456789012345678901234567890123456789"));
        assert!(!is_hex_oid("xyz"));
    }
}
