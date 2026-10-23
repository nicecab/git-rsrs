mod common;
use common::*;
use git_rsrs::{git::Git, process::ProcessRunner};
use std::{
    ffi::OsString,
    fs,
    os::unix::{
        ffi::OsStringExt,
        fs::{PermissionsExt, symlink},
    },
};

#[test]
fn status_preserves_paths_modes_deletions_and_tracked_ignored_files() {
    let f = Fixture::new();
    f.commit(&f.a, ".gitignore", "*.ignored\n");
    fs::write(f.a.join("tracked.ignored"), "old").unwrap();
    git(&f.a, ["add", "-f", "tracked.ignored"]);
    git(&f.a, ["commit", "-m", "tracked ignored"]);
    fs::write(f.a.join("tracked.ignored"), "new").unwrap();
    fs::write(f.a.join("hidden.ignored"), "skip").unwrap();
    fs::write(f.a.join("space\nand name"), "new").unwrap();
    let raw = OsString::from("ordinary");
    fs::write(f.a.join(&raw), "bytes").unwrap();
    fs::remove_file(f.a.join("shared.txt")).unwrap();
    symlink("missing", f.a.join("dangling")).unwrap();
    fs::set_permissions(f.a.join(".gitignore"), fs::Permissions::from_mode(0o755)).unwrap();
    let g = Git::open(&f.a, ProcessRunner).unwrap();
    let entries = g.status().unwrap();
    assert!(
        entries
            .iter()
            .any(|e| e.path == std::path::Path::new("tracked.ignored"))
    );
    assert!(
        !entries
            .iter()
            .any(|e| e.path == std::path::Path::new("hidden.ignored"))
    );
    for path in [
        OsString::from("space\nand name"),
        raw,
        "dangling".into(),
        ".gitignore".into(),
    ] {
        assert!(entries.iter().any(|e| e.path.as_os_str() == path));
    }
    assert!(
        entries
            .iter()
            .any(|e| e.path == std::path::Path::new("shared.txt") && e.code == *b" D")
    );
}

#[test]
fn rename_records_are_not_misparsed_as_separate_changes() {
    let f = Fixture::new();
    git(&f.a, ["mv", "shared.txt", "new name"]);
    let entries = Git::open(&f.a, ProcessRunner).unwrap().status().unwrap();
    assert_eq!(entries.len(), 1);
    assert_eq!(entries[0].path.to_str(), Some("new name"));
    assert_eq!(
        entries[0].original_path.as_deref(),
        Some(std::path::Path::new("shared.txt"))
    );
}

#[test]
fn status_parser_retains_non_utf8_bytes() {
    let entries = git_rsrs::git::status::parse(b"?? non-utf8-\xff\0").unwrap();
    assert_eq!(
        entries[0].path.as_os_str(),
        OsString::from_vec(b"non-utf8-\xff".to_vec())
    );
}

#[cfg(target_os = "linux")]
#[test]
fn linux_non_utf8_file_is_visible_to_git() {
    let f = Fixture::new();
    let raw = OsString::from_vec(b"non-utf8-\xff".to_vec());
    fs::write(f.a.join(&raw), "bytes").unwrap();
    let entries = Git::open(&f.a, ProcessRunner).unwrap().status().unwrap();
    assert!(entries.iter().any(|e| e.path.as_os_str() == raw));
}
