//! Validation helpers that keep user input out of dangerous states.

/// Returns true when the string contains control characters.
pub fn has_control_chars(input: &str) -> bool {
    input.chars().any(char::is_control)
}

/// Validate repository names as a single safe URL path segment.
pub fn safe_repo_name(name: &str) -> bool {
    !name.is_empty()
        && !name.contains('/')
        && !name.contains('\\')
        && name != "."
        && name != ".."
        && !name.contains("..")
        && !has_control_chars(name)
}

/// Validate a path inside a Git tree, not the host filesystem.
pub fn safe_git_path(path: &str) -> bool {
    if path.is_empty() {
        return true;
    }
    if path.starts_with('/')
        || path.contains('\\')
        || path.contains('\0')
        || path.contains(':')
        || has_control_chars(path)
    {
        return false;
    }
    path.split('/').all(|part| !matches!(part, "" | "." | ".."))
}

/// Validate a Git revision accepted by the UI.
pub fn safe_git_rev(rev: &str) -> bool {
    if rev.is_empty()
        || rev.len() > 128
        || rev.starts_with('-')
        || rev.contains(':')
        || rev.contains('\\')
        || rev.contains(' ')
        || rev.contains('\0')
        || rev.contains("..")
        || has_control_chars(rev)
    {
        return false;
    }
    rev == "HEAD"
        || rev
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'/' | b'.' | b'_' | b'-'))
}

/// Validate file paths used by dumb HTTP clone endpoints.
pub fn safe_clone_file_path(path: &str) -> bool {
    if has_control_chars(path) || path.contains('\\') || path.contains('\0') || path.contains(':') {
        return false;
    }
    let parts: Vec<&str> = path.split('/').collect();
    if parts.iter().any(|part| matches!(*part, "" | "." | "..")) {
        return false;
    }
    match parts.as_slice() {
        ["objects", a, b]
            if a.len() == 2
                && b.len() == 38
                && a.bytes().all(|c| c.is_ascii_hexdigit())
                && b.bytes().all(|c| c.is_ascii_hexdigit()) =>
        {
            true
        }
        ["objects", "pack", file]
            if file.starts_with("pack-")
                && (file.ends_with(".pack") || file.ends_with(".idx"))
                && file
                    .bytes()
                    .all(|c| c.is_ascii_alphanumeric() || matches!(c, b'-' | b'.')) =>
        {
            true
        }
        _ => false,
    }
}

/// Validate HTTP Host values used to render local clone commands.
pub fn safe_host(host: &str) -> bool {
    !host.is_empty()
        && host.len() <= 255
        && !has_control_chars(host)
        && host
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'.' | b'-' | b':' | b'[' | b']'))
}

/// Validate a configured public HTTP(S) base URL.
pub fn safe_http_clone_url(url: &str) -> bool {
    !url.is_empty()
        && url.len() <= 512
        && !has_control_chars(url)
        && !url.contains(' ')
        && (url.starts_with("https://") || url.starts_with("http://"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rejects_option_like_revisions() {
        assert!(!safe_git_rev("--help"));
        assert!(!safe_git_rev("-r"));
        assert!(safe_git_rev("HEAD"));
        assert!(safe_git_rev("main"));
    }

    #[test]
    fn rejects_parent_paths() {
        assert!(!safe_git_path("../secret"));
        assert!(!safe_git_path("a/../secret"));
        assert!(safe_git_path("src/main.rs"));
    }

    #[test]
    fn allows_only_git_object_clone_paths() {
        assert!(safe_clone_file_path(
            "objects/ab/01234567890123456789012345678901234567"
        ));
        assert!(safe_clone_file_path("objects/pack/pack-abc123.pack"));
        assert!(!safe_clone_file_path("config"));
        assert!(!safe_clone_file_path("objects/../../config"));
    }
}
