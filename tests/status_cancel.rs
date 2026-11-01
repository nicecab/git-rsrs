// Own test binary: CANCELLED is process-global and must not leak into other tests.
mod common;
use common::*;
use git_rsrs::{config::Config, outcome::Trigger, process::CANCELLED, sync::sync_once};
use std::{fs, sync::atomic::Ordering};

#[test]
fn attempt_cut_short_by_shutdown_is_not_recorded() {
    let f = Fixture::new();
    let status = f.temp.path().join("status.json");
    let mut config = Config::parse(&format!(
        "version=1\nbranch='main'\nstatus_file={:?}\n",
        status.to_str().unwrap()
    ))
    .unwrap();
    config.repo = f.a.canonicalize().unwrap();
    sync_once(&config, Trigger::Manual);
    let before = fs::read_to_string(&status).unwrap();
    CANCELLED.store(true, Ordering::Relaxed);
    sync_once(&config, Trigger::Manual);
    CANCELLED.store(false, Ordering::Relaxed);
    assert_eq!(fs::read_to_string(&status).unwrap(), before);
}
