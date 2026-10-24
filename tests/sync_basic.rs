mod common;
use common::*;
use git_rsrs::{
    outcome::{Outcome, Trigger},
    sync::{sync_once, sync_once_guarded},
};
use std::fs;

#[test]
fn no_changes_is_a_noop_and_local_changes_reach_origin() {
    let f = Fixture::new();
    let c = f.config();
    assert_eq!(sync_once(&c, Trigger::Manual).outcome, Outcome::Noop);
    fs::write(f.a.join("new"), "local\n").unwrap();
    let report = sync_once(&c, Trigger::Manual);
    assert_eq!(report.outcome, Outcome::Synced, "{report:?}");
    assert!(report.committed.is_some());
    assert!(report.pushed);
    assert_eq!(text(&f.origin, &["show", "main:new"]), "local");
    assert_eq!(text(&f.a, &["status", "--porcelain"]), "");
}
#[test]
fn remote_only_changes_arrive_and_diverged_changes_are_rebased() {
    let f = Fixture::new();
    let c = f.config();
    f.commit(&f.b, "remote", "incoming\n");
    git(&f.b, ["push"]);
    let report = sync_once(&c, Trigger::Periodic);
    assert_eq!(report.outcome, Outcome::Synced, "{report:?}");
    assert_eq!(
        fs::read_to_string(f.a.join("remote")).unwrap(),
        "incoming\n"
    );
    f.commit(&f.b, "remote2", "incoming again\n");
    git(&f.b, ["push"]);
    fs::write(f.a.join("local"), "outgoing\n").unwrap();
    let report = sync_once(&c, Trigger::Manual);
    assert_eq!(report.outcome, Outcome::Synced, "{report:?}");
    assert_eq!(text(&f.origin, &["show", "main:local"]), "outgoing");
    assert_eq!(
        text(&f.a, &["rev-parse", "HEAD"]),
        text(&f.origin, &["rev-parse", "main"])
    );
}
#[test]
fn branch_guard_does_not_commit_switch_or_push_a_feature_branch() {
    let f = Fixture::new();
    let c = f.config();
    git(&f.a, ["checkout", "-b", "feature"]);
    fs::write(f.a.join("private"), "keep\n").unwrap();
    let head = text(&f.a, &["rev-parse", "HEAD"]);
    let report = sync_once(&c, Trigger::Manual);
    assert_eq!(report.outcome, Outcome::BranchSkipped);
    assert_eq!(text(&f.a, &["branch", "--show-current"]), "feature");
    assert_eq!(text(&f.a, &["rev-parse", "HEAD"]), head);
}
#[test]
fn json_cli_ignores_foreign_repository_location_environment() {
    let f = Fixture::new();
    let cfg = f.temp.path().join("config.toml");
    fs::write(&cfg, "version=1\nbranch='main'").unwrap();
    fs::write(f.a.join("new"), "right repo").unwrap();
    let original_b = text(&f.b, &["rev-parse", "HEAD"]);
    let output = std::process::Command::new(env!("CARGO_BIN_EXE_git-rsrs"))
        .args(["--json", "--repo"])
        .arg(&f.a)
        .arg("--config")
        .arg(&cfg)
        .arg("sync")
        .env("GIT_DIR", f.b.join(".git"))
        .env("GIT_WORK_TREE", &f.b)
        .env("GIT_INDEX_FILE", f.b.join(".git/index"))
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let report: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(report["outcome"], "synced");
    assert_eq!(text(&f.b, &["rev-parse", "HEAD"]), original_b);
    assert_eq!(text(&f.origin, &["show", "main:new"]), "right repo");
}

#[test]
fn periodic_run_discovering_new_local_edits_defers_instead_of_committing() {
    let f = Fixture::new();
    let c = f.config();
    let head = text(&f.a, &["rev-parse", "HEAD"]);
    fs::write(f.a.join("new"), "just saved").unwrap();
    let commit_local = false;
    let report = sync_once_guarded(&c, Trigger::Periodic, commit_local, || true);
    assert_eq!(report.outcome, Outcome::Deferred);
    assert!(report.committed.is_none());
    assert_eq!(text(&f.a, &["rev-parse", "HEAD"]), head);
}

#[test]
fn periodic_trigger_commits_a_dirty_tree_when_commit_local_is_set() {
    let f = Fixture::new();
    let c = f.config();
    fs::write(f.a.join("new"), "just saved").unwrap();
    let commit_local = true;
    let report = sync_once_guarded(&c, Trigger::Periodic, commit_local, || true);
    assert_eq!(report.outcome, Outcome::Synced, "{report:?}");
    assert!(report.committed.is_some());
    assert_eq!(text(&f.origin, &["show", "main:new"]), "just saved");
}
