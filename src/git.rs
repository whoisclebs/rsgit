//! Small native Git reader inspired by gitoxide's object database layering.
//!
//! This is intentionally not a general Git implementation. It implements only
//! the pieces rsgit uses: refs, loose objects, pack/idx lookup, OFS/REF deltas,
//! commit parsing, tree traversal, and blob reads. It never spawns `git`.

use std::collections::{BTreeMap, HashSet};
use std::fs;
use std::io::Read;
use std::path::Path;

use flate2::read::ZlibDecoder;

use crate::repo::Repo;

/// A Git object id as lowercase hex.
type Oid = String;

/// A Git ref resolved from loose refs or `packed-refs`.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct RefInfo {
    /// Ref name such as `refs/heads/main`.
    pub name: String,
    /// Hex object id for the ref target.
    pub oid: String,
}

/// Commit information rendered by the UI.
#[derive(Clone, Debug)]
pub struct CommitSummary {
    /// Full object id.
    pub id: String,
    /// Short object id.
    pub short_id: String,
    /// Commit subject line.
    pub subject: String,
    /// Author display name.
    pub author: String,
    /// Commit timestamp as seconds since Unix epoch.
    pub time: i64,
    /// Parent commit ids.
    pub parents: Vec<String>,
    tree: String,
}

/// Tree entry information rendered by the UI.
#[derive(Clone, Debug)]
pub struct TreeEntry {
    /// Git mode string.
    pub mode: String,
    /// Entry kind (`tree`, `blob`, `commit`).
    pub kind: String,
    /// Object id.
    pub oid: String,
    /// File name.
    pub name: String,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum Kind {
    Commit,
    Tree,
    Blob,
    Tag,
}

#[derive(Clone, Debug)]
struct Object {
    kind: Kind,
    data: Vec<u8>,
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

/// Return recent commits by walking the first-parent chain from HEAD.
pub fn recent_commits(repo: &Repo, limit: usize) -> Vec<CommitSummary> {
    let Some(mut oid) = resolve_rev(repo, "HEAD") else {
        return Vec::new();
    };
    let mut out = Vec::new();
    let mut seen = HashSet::new();
    for _ in 0..limit {
        if !seen.insert(oid.clone()) {
            break;
        }
        let Some(commit) = commit(repo, &oid) else {
            break;
        };
        oid = match commit.parents.first() {
            Some(parent) => parent.clone(),
            None => {
                out.push(commit);
                break;
            }
        };
        out.push(commit);
    }
    out
}

/// Return entries for `path` at `rev`.
pub fn tree_entries(repo: &Repo, rev: &str, path: &str) -> Vec<TreeEntry> {
    let Some(commit) = commit(repo, rev) else {
        return Vec::new();
    };
    let Some(tree_oid) = lookup_tree_path(repo, &commit.tree, path) else {
        return Vec::new();
    };
    read_tree(repo, &tree_oid).unwrap_or_default()
}

/// Return blob bytes for `path` at `rev`.
pub fn blob(repo: &Repo, rev: &str, path: &str, max_bytes: usize) -> Option<Vec<u8>> {
    let commit = commit(repo, rev)?;
    let entry = lookup_entry(repo, &commit.tree, path)?;
    if entry.kind != "blob" || entry.oid.len() != 40 {
        return None;
    }
    let object = read_object(repo, &entry.oid).ok()?;
    if object.kind != Kind::Blob || object.data.len() > max_bytes {
        return None;
    }
    Some(object.data)
}

/// Return a decoded commit by revision.
pub fn commit(repo: &Repo, rev: &str) -> Option<CommitSummary> {
    let oid = resolve_rev(repo, rev)?;
    let object = read_object(repo, &oid).ok()?;
    (object.kind == Kind::Commit)
        .then(|| parse_commit(oid, &object.data))
        .flatten()
}

/// Search recent commits by subject/message.
pub fn search_commits(repo: &Repo, query: &str, limit: usize) -> Vec<CommitSummary> {
    let q = query.to_ascii_lowercase();
    recent_commits(repo, limit * 4)
        .into_iter()
        .filter(|c| c.subject.to_ascii_lowercase().contains(&q))
        .take(limit)
        .collect()
}

fn resolve_rev(repo: &Repo, rev: &str) -> Option<Oid> {
    if is_hex_oid(rev) {
        return Some(rev.to_ascii_lowercase());
    }
    if rev == "HEAD" {
        let head = String::from_utf8(head_bytes(repo)?).ok()?;
        if let Some(sym) = head.strip_prefix("ref: ") {
            return resolve_ref(repo, sym.trim());
        }
        let oid = head.trim();
        return is_hex_oid(oid).then(|| oid.to_string());
    }
    resolve_ref(repo, rev).or_else(|| resolve_ref(repo, &format!("refs/heads/{rev}")))
}

fn resolve_ref(repo: &Repo, name: &str) -> Option<Oid> {
    refs(repo)
        .into_iter()
        .find(|r| r.name == name)
        .map(|r| r.oid)
}

fn read_object(repo: &Repo, oid: &str) -> Result<Object, String> {
    read_loose_object(repo, oid).or_else(|_| read_packed_object(repo, oid))
}

fn read_loose_object(repo: &Repo, oid: &str) -> Result<Object, String> {
    if oid.len() != 40 {
        return Err("bad oid".into());
    }
    let path = repo
        .git_dir()
        .join("objects")
        .join(&oid[0..2])
        .join(&oid[2..]);
    let compressed = fs::read(path).map_err(|e| e.to_string())?;
    let data = inflate_all(&compressed)?;
    parse_object_payload(&data)
}

fn parse_object_payload(data: &[u8]) -> Result<Object, String> {
    let nul = data
        .iter()
        .position(|b| *b == 0)
        .ok_or("missing object header")?;
    let header = std::str::from_utf8(&data[..nul]).map_err(|e| e.to_string())?;
    let mut parts = header.split(' ');
    let kind = match parts.next().ok_or("missing kind")? {
        "commit" => Kind::Commit,
        "tree" => Kind::Tree,
        "blob" => Kind::Blob,
        "tag" => Kind::Tag,
        other => return Err(format!("unsupported object kind {other}")),
    };
    Ok(Object {
        kind,
        data: data[nul + 1..].to_vec(),
    })
}

fn read_packed_object(repo: &Repo, oid: &str) -> Result<Object, String> {
    let pack_dir = repo.git_dir().join("objects").join("pack");
    let entries = fs::read_dir(&pack_dir).map_err(|e| e.to_string())?;
    for entry in entries.flatten() {
        let path = entry.path();
        if path.extension().and_then(|s| s.to_str()) != Some("idx") {
            continue;
        }
        if let Ok(offset) = idx_lookup(&path, oid) {
            let pack = path.with_extension("pack");
            return read_pack_at(repo, &pack, offset);
        }
    }
    Err("object not found in packs".into())
}

fn idx_lookup(path: &Path, oid: &str) -> Result<u64, String> {
    let data = fs::read(path).map_err(|e| e.to_string())?;
    if data.len() < 8 || &data[0..4] != b"\xfftOc" {
        return Err("unsupported idx".into());
    }
    let version = be_u32(&data, 4)?;
    if version != 2 {
        return Err("unsupported idx version".into());
    }
    let fanout = 8;
    let count = be_u32(&data, fanout + 255 * 4)? as usize;
    let oid_bytes = hex_to_20(oid)?;
    let first = oid_bytes[0] as usize;
    let start = if first == 0 {
        0
    } else {
        be_u32(&data, fanout + (first - 1) * 4)? as usize
    };
    let end = be_u32(&data, fanout + first * 4)? as usize;
    let names = fanout + 256 * 4;
    for i in start..end {
        let pos = names + i * 20;
        if data.get(pos..pos + 20) == Some(&oid_bytes[..]) {
            let offsets = names + count * 20 + count * 4;
            let raw = be_u32(&data, offsets + i * 4)?;
            if raw & 0x8000_0000 == 0 {
                return Ok(raw as u64);
            }
            let large_index = (raw & 0x7fff_ffff) as usize;
            let large_offsets = offsets + count * 4;
            return be_u64(&data, large_offsets + large_index * 8);
        }
    }
    Err("oid not in idx".into())
}

fn read_pack_at(repo: &Repo, pack: &Path, offset: u64) -> Result<Object, String> {
    let data = fs::read(pack).map_err(|e| e.to_string())?;
    if data.len() < 12 || &data[0..4] != b"PACK" {
        return Err("bad pack".into());
    }
    read_pack_object(repo, &data, offset as usize, 0)
}

fn read_pack_object(
    repo: &Repo,
    data: &[u8],
    offset: usize,
    depth: usize,
) -> Result<Object, String> {
    if depth > 32 {
        return Err("delta depth exceeded".into());
    }
    let (typ, _size, mut pos) = parse_pack_header(data, offset)?;
    match typ {
        1..=4 => {
            let inflated = inflate_from(&data[pos..])?;
            let kind = match typ {
                1 => Kind::Commit,
                2 => Kind::Tree,
                3 => Kind::Blob,
                4 => Kind::Tag,
                _ => unreachable!(),
            };
            Ok(Object {
                kind,
                data: inflated,
            })
        }
        6 => {
            let (base_offset, next) = parse_ofs_delta(data, offset, pos)?;
            pos = next;
            let base = read_pack_object(repo, data, base_offset, depth + 1)?;
            let delta = inflate_from(&data[pos..])?;
            Ok(Object {
                kind: base.kind,
                data: apply_delta(&base.data, &delta)?,
            })
        }
        7 => {
            let base_oid = bytes_to_hex(data.get(pos..pos + 20).ok_or("bad ref delta")?);
            pos += 20;
            let base = read_object(repo, &base_oid)?;
            let delta = inflate_from(&data[pos..])?;
            Ok(Object {
                kind: base.kind,
                data: apply_delta(&base.data, &delta)?,
            })
        }
        _ => Err("unsupported pack object type".into()),
    }
}

fn parse_pack_header(data: &[u8], offset: usize) -> Result<(u8, usize, usize), String> {
    let mut pos = offset;
    let c = *data.get(pos).ok_or("short pack header")?;
    pos += 1;
    let typ = (c >> 4) & 0x07;
    let mut size = (c & 0x0f) as usize;
    let mut shift = 4;
    let mut byte = c;
    while byte & 0x80 != 0 {
        byte = *data.get(pos).ok_or("short pack size")?;
        pos += 1;
        size |= ((byte & 0x7f) as usize) << shift;
        shift += 7;
    }
    Ok((typ, size, pos))
}

fn parse_ofs_delta(
    data: &[u8],
    object_offset: usize,
    mut pos: usize,
) -> Result<(usize, usize), String> {
    let mut c = *data.get(pos).ok_or("short ofs delta")? as usize;
    pos += 1;
    let mut ofs = c & 0x7f;
    while c & 0x80 != 0 {
        c = *data.get(pos).ok_or("short ofs delta")? as usize;
        pos += 1;
        ofs = ((ofs + 1) << 7) | (c & 0x7f);
    }
    object_offset
        .checked_sub(ofs)
        .map(|base| (base, pos))
        .ok_or_else(|| "bad ofs delta".into())
}

fn apply_delta(base: &[u8], delta: &[u8]) -> Result<Vec<u8>, String> {
    let mut pos = 0;
    let _source_size = read_varint(delta, &mut pos)?;
    let target_size = read_varint(delta, &mut pos)?;
    let mut out = Vec::with_capacity(target_size);
    while pos < delta.len() {
        let op = delta[pos];
        pos += 1;
        if op & 0x80 != 0 {
            let mut cp_off = 0usize;
            let mut cp_size = 0usize;
            if op & 0x01 != 0 {
                cp_off |= read_byte(delta, &mut pos)?;
            }
            if op & 0x02 != 0 {
                cp_off |= read_byte(delta, &mut pos)? << 8;
            }
            if op & 0x04 != 0 {
                cp_off |= read_byte(delta, &mut pos)? << 16;
            }
            if op & 0x08 != 0 {
                cp_off |= read_byte(delta, &mut pos)? << 24;
            }
            if op & 0x10 != 0 {
                cp_size |= read_byte(delta, &mut pos)?;
            }
            if op & 0x20 != 0 {
                cp_size |= read_byte(delta, &mut pos)? << 8;
            }
            if op & 0x40 != 0 {
                cp_size |= read_byte(delta, &mut pos)? << 16;
            }
            if cp_size == 0 {
                cp_size = 0x10000;
            }
            out.extend_from_slice(
                base.get(cp_off..cp_off + cp_size)
                    .ok_or("delta copy out of range")?,
            );
        } else if op != 0 {
            let len = op as usize;
            out.extend_from_slice(
                delta
                    .get(pos..pos + len)
                    .ok_or("delta insert out of range")?,
            );
            pos += len;
        } else {
            return Err("invalid delta opcode".into());
        }
    }
    if out.len() != target_size {
        return Err("delta size mismatch".into());
    }
    Ok(out)
}

fn read_tree(repo: &Repo, oid: &str) -> Result<Vec<TreeEntry>, String> {
    let object = read_object(repo, oid)?;
    if object.kind != Kind::Tree {
        return Err("not a tree".into());
    }
    parse_tree(&object.data)
}

fn parse_tree(data: &[u8]) -> Result<Vec<TreeEntry>, String> {
    let mut out = Vec::new();
    let mut pos = 0;
    while pos < data.len() {
        let mode_end = find_byte(data, pos, b' ').ok_or("bad tree mode")?;
        let mode = std::str::from_utf8(&data[pos..mode_end])
            .map_err(|e| e.to_string())?
            .to_string();
        pos = mode_end + 1;
        let name_end = find_byte(data, pos, 0).ok_or("bad tree name")?;
        let name = String::from_utf8_lossy(&data[pos..name_end]).into_owned();
        pos = name_end + 1;
        let oid = bytes_to_hex(data.get(pos..pos + 20).ok_or("bad tree oid")?);
        pos += 20;
        let kind = if mode == "40000" || mode == "040000" {
            "tree"
        } else if mode == "160000" {
            "commit"
        } else {
            "blob"
        };
        out.push(TreeEntry {
            mode,
            kind: kind.to_string(),
            oid,
            name,
        });
    }
    Ok(out)
}

fn lookup_tree_path(repo: &Repo, root: &str, path: &str) -> Option<Oid> {
    if path.is_empty() {
        return Some(root.to_string());
    }
    let entry = lookup_entry(repo, root, path)?;
    (entry.kind == "tree").then_some(entry.oid)
}

fn lookup_entry(repo: &Repo, root: &str, path: &str) -> Option<TreeEntry> {
    let mut current = root.to_string();
    let mut parts = path.split('/').peekable();
    while let Some(part) = parts.next() {
        let entries = read_tree(repo, &current).ok()?;
        let entry = entries.into_iter().find(|e| e.name == part)?;
        if parts.peek().is_none() {
            return Some(entry);
        }
        if entry.kind != "tree" {
            return None;
        }
        current = entry.oid;
    }
    None
}

fn parse_commit(oid: Oid, data: &[u8]) -> Option<CommitSummary> {
    let text = String::from_utf8_lossy(data);
    let (headers, message) = text.split_once("\n\n").unwrap_or((&text, ""));
    let mut tree = String::new();
    let mut parents = Vec::new();
    let mut author = String::new();
    let mut time = 0;
    for line in headers.lines() {
        if let Some(v) = line.strip_prefix("tree ") {
            tree = v.to_string();
        }
        if let Some(v) = line.strip_prefix("parent ") {
            parents.push(v.to_string());
        }
        if let Some(v) = line.strip_prefix("author ") {
            let (name, ts) = parse_signature(v);
            author = name;
            time = ts;
        }
    }
    let subject = message.lines().next().unwrap_or_default().to_string();
    let short_id = oid.get(..8).unwrap_or(&oid).to_string();
    Some(CommitSummary {
        id: oid,
        short_id,
        subject,
        author,
        time,
        parents,
        tree,
    })
}

fn parse_signature(value: &str) -> (String, i64) {
    let name = value.split('<').next().unwrap_or(value).trim().to_string();
    let ts = value
        .rsplit('>')
        .next()
        .unwrap_or("")
        .split_whitespace()
        .next()
        .and_then(|s| s.parse().ok())
        .unwrap_or_default();
    (name, ts)
}

fn inflate_all(data: &[u8]) -> Result<Vec<u8>, String> {
    inflate_from(data)
}

fn inflate_from(data: &[u8]) -> Result<Vec<u8>, String> {
    let mut decoder = ZlibDecoder::new(data);
    let mut out = Vec::new();
    decoder.read_to_end(&mut out).map_err(|e| e.to_string())?;
    Ok(out)
}

fn read_varint(data: &[u8], pos: &mut usize) -> Result<usize, String> {
    let mut out = 0usize;
    let mut shift = 0;
    loop {
        let b = read_byte(data, pos)?;
        out |= (b & 0x7f) << shift;
        if b & 0x80 == 0 {
            return Ok(out);
        }
        shift += 7;
    }
}

fn read_byte(data: &[u8], pos: &mut usize) -> Result<usize, String> {
    let b = *data.get(*pos).ok_or("short read")? as usize;
    *pos += 1;
    Ok(b)
}

fn be_u32(data: &[u8], pos: usize) -> Result<u32, String> {
    let b = data.get(pos..pos + 4).ok_or("short u32")?;
    Ok(u32::from_be_bytes([b[0], b[1], b[2], b[3]]))
}

fn be_u64(data: &[u8], pos: usize) -> Result<u64, String> {
    let b = data.get(pos..pos + 8).ok_or("short u64")?;
    Ok(u64::from_be_bytes([
        b[0], b[1], b[2], b[3], b[4], b[5], b[6], b[7],
    ]))
}

fn find_byte(data: &[u8], start: usize, needle: u8) -> Option<usize> {
    data.get(start..)?
        .iter()
        .position(|b| *b == needle)
        .map(|idx| start + idx)
}

fn hex_to_20(oid: &str) -> Result<[u8; 20], String> {
    if oid.len() != 40 {
        return Err("bad oid length".into());
    }
    let mut out = [0u8; 20];
    for i in 0..20 {
        out[i] = u8::from_str_radix(&oid[i * 2..i * 2 + 2], 16).map_err(|e| e.to_string())?;
    }
    Ok(out)
}

fn bytes_to_hex(bytes: &[u8]) -> String {
    let mut out = String::with_capacity(bytes.len() * 2);
    for b in bytes {
        out.push_str(&format!("{b:02x}"));
    }
    out
}

fn read_loose_refs(repo: &Repo, refs: &mut BTreeMap<String, String>) {
    let root = repo.git_dir().join("refs");
    visit_ref_dir(&root, "refs", refs);
}

fn visit_ref_dir(dir: &Path, prefix: &str, refs: &mut BTreeMap<String, String>) {
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
    value.len() == 40 && value.bytes().all(|b| b.is_ascii_hexdigit())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn validates_sha1_lengths() {
        assert!(is_hex_oid("0123456789012345678901234567890123456789"));
        assert!(!is_hex_oid("xyz"));
    }
}
