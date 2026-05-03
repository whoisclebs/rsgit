use rsgit::security::{safe_clone_file_path, safe_repo_name};

#[test]
fn repository_names_are_single_safe_segments() {
    assert!(safe_repo_name("golpher"));
    assert!(!safe_repo_name("../secret"));
    assert!(!safe_repo_name("nested/repo"));
}

#[test]
fn clone_paths_are_limited_to_git_objects() {
    assert!(safe_clone_file_path(
        "objects/ab/01234567890123456789012345678901234567"
    ));
    assert!(safe_clone_file_path("objects/pack/pack-abc123.pack"));
    assert!(!safe_clone_file_path("config"));
}
