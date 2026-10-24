mod common;
use common::*;
use git_rsrs::{git::Git, process::ProcessRunner};
use std::{
    ffi::{OsStr, OsString},
    os::unix::ffi::OsStringExt,
    path::{Path, PathBuf},
};

fn git_path(cwd: &Path, name: &str) -> PathBuf {
    let mut raw = git(
        cwd,
        ["rev-parse", "--path-format=absolute", "--git-path", name],
    )
    .stdout;
    assert_eq!(raw.pop(), Some(b'\n'));
    PathBuf::from(OsString::from_vec(raw))
}
/// Opens `checkout` and checks every cached path against Git, one name at a time.
fn assert_paths_match_git(checkout: &Path) -> Git {
    let g = Git::open(checkout, ProcessRunner).unwrap();
    let p = &g.paths;
    // Autostash paths are built by joining onto the resolved parents.
    let merge_autostash = p.rebase_merge.join("autostash");
    let apply_autostash = p.rebase_apply.join("autostash");
    for (name, cached) in [
        (".", &g.git_dir),
        ("index", &p.index),
        ("index.lock", &p.index_lock),
        ("git-rsrs-commit.json", &p.commit_journal),
        ("git-rsrs-integrate.json", &p.integrate_journal),
        ("MERGE_HEAD", &p.merge_head),
        ("rebase-merge", &p.rebase_merge),
        ("rebase-apply", &p.rebase_apply),
        ("CHERRY_PICK_HEAD", &p.cherry_pick_head),
        ("REVERT_HEAD", &p.revert_head),
        ("BISECT_START", &p.bisect_start),
        ("sequencer", &p.sequencer),
        ("rebase-merge/autostash", &merge_autostash),
        ("rebase-apply/autostash", &apply_autostash),
    ] {
        assert_eq!(*cached, git_path(checkout, name), "{name}");
    }
    g
}

#[test]
fn cached_paths_match_git_in_a_normal_clone() {
    let f = Fixture::new();
    let g = assert_paths_match_git(&f.a);
    assert_eq!(g.root, f.a.canonicalize().unwrap());
}

#[test]
fn cached_paths_match_git_in_a_linked_worktree() {
    let f = Fixture::new();
    let linked = f.temp.path().join("linked");
    git(
        &f.a,
        [
            OsStr::new("worktree"),
            "add".as_ref(),
            "-b".as_ref(),
            "feature".as_ref(),
            linked.as_os_str(),
        ],
    );
    let other = assert_paths_match_git(&linked);
    assert_eq!(other.root, linked.canonicalize().unwrap());
    let main = Git::open(&f.a, ProcessRunner).unwrap();
    assert_ne!(main.paths.index, other.paths.index);
}
