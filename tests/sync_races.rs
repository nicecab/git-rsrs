mod common;
use common::*;
use git_rsrs::{
    config::Config,
    outcome::{Outcome, Trigger},
    sync::sync_once,
};
use std::{fs, os::unix::fs::PermissionsExt};
fn race(f: &Fixture, always: bool) -> (Config, std::path::PathBuf) {
    let c = f.config();
    let hooks = f.temp.path().join("hooks");
    fs::create_dir(&hooks).unwrap();
    let count = f.temp.path().join("push-count");
    let hook = hooks.join("pre-push");
    let script = format!(
        "#!/usr/bin/env python3\nimport os,subprocess,pathlib\np=pathlib.Path({count:?}); n=int(p.read_text())+1 if p.exists() else 1; p.write_text(str(n))\nif n==1 or {always}:\n env={{k:v for k,v in os.environ.items() if not k.startswith('GIT_')}}\n def git(*args): subprocess.run(['git','-C',{repo:?},*args],env=env,check=True,stdout=subprocess.DEVNULL,stderr=subprocess.DEVNULL)\n git('fetch','origin','main'); git('reset','--hard','origin/main')\n pathlib.Path({file:?}).write_text(str(n))\n git('add','-A'); git('commit','-m','race'); git('push')\n",
        count = count.to_str().unwrap(),
        always = if always { "True" } else { "False" },
        repo = f.b.to_str().unwrap(),
        file = f.b.join("remote-race").to_str().unwrap()
    );
    fs::write(&hook, script).unwrap();
    fs::set_permissions(hook, fs::Permissions::from_mode(0o755)).unwrap();
    git(
        &f.a,
        [
            std::ffi::OsStr::new("config"),
            "core.hooksPath".as_ref(),
            hooks.as_os_str(),
        ],
    );
    fs::write(f.a.join("local"), "local work\n").unwrap();
    (c, count)
}
#[test]
fn competing_push_reintegrates_and_retries_once() {
    let f = Fixture::new();
    let (c, count) = race(&f, false);
    let report = sync_once(&c, Trigger::Manual);
    assert_eq!(report.outcome, Outcome::Synced, "{report:?}");
    assert_eq!(fs::read_to_string(count).unwrap(), "2");
    assert_eq!(text(&f.origin, &["show", "main:local"]), "local work");
    assert_eq!(text(&f.origin, &["show", "main:remote-race"]), "1");
}
#[test]
fn second_competing_push_is_not_retried_indefinitely() {
    let f = Fixture::new();
    let (c, count) = race(&f, true);
    let report = sync_once(&c, Trigger::Manual);
    assert_eq!(report.outcome, Outcome::ManualIntervention, "{report:?}");
    assert_eq!(fs::read_to_string(count).unwrap(), "2");
    assert!(!report.pushed);
    assert_eq!(text(&f.a, &["show", "HEAD:local"]), "local work");
}

#[test]
fn server_hook_rejection_is_not_retried_as_a_push_race() {
    let f = Fixture::new();
    let c = f.config();
    let hooks = f.temp.path().join("hooks");
    fs::create_dir(&hooks).unwrap();
    let count = f.temp.path().join("push-count");
    let hook = hooks.join("pre-push");
    fs::write(&hook,format!("#!/usr/bin/env python3\nimport pathlib\np=pathlib.Path({:?}); p.write_text(str(int(p.read_text())+1 if p.exists() else 1))\n",count.to_str().unwrap())).unwrap();
    fs::set_permissions(hook, fs::Permissions::from_mode(0o755)).unwrap();
    git(
        &f.a,
        [
            std::ffi::OsStr::new("config"),
            "core.hooksPath".as_ref(),
            hooks.as_os_str(),
        ],
    );
    let reject = f.origin.join("hooks/pre-receive");
    fs::write(
        &reject,
        "#!/bin/sh\necho 'server policy denied push' >&2\nexit 1\n",
    )
    .unwrap();
    fs::set_permissions(reject, fs::Permissions::from_mode(0o755)).unwrap();
    fs::write(f.a.join("new"), "local work").unwrap();
    let report = sync_once(&c, Trigger::Manual);
    assert_eq!(report.outcome, Outcome::Failed, "{report:?}");
    assert_eq!(fs::read_to_string(count).unwrap(), "1");
    assert!(report.committed.is_some());
}

#[test]
fn review_branch_switch_during_fetch_cannot_publish_feature_work_to_main() {
    use std::os::unix::fs::PermissionsExt;
    let f = Fixture::new();
    let barrier = common::Barrier {
        ready: f.temp.path().join("ready"),
        release: f.temp.path().join("release"),
    };
    let upload = f.temp.path().join("upload.py");
    fs::write(&upload,format!("#!/usr/bin/env python3\nimport os,sys,time,pathlib\npathlib.Path({:?}).touch()\ndeadline=time.monotonic()+60\nwhile not pathlib.Path({:?}).exists():\n if time.monotonic()>deadline: raise SystemExit(1)\n time.sleep(.01)\nos.execvp('git-upload-pack',['git-upload-pack',*sys.argv[1:]])\n",barrier.ready.to_str().unwrap(),barrier.release.to_str().unwrap())).unwrap();
    fs::set_permissions(&upload, fs::Permissions::from_mode(0o755)).unwrap();
    git(
        &f.a,
        [
            std::ffi::OsStr::new("config"),
            "remote.origin.uploadpack".as_ref(),
            upload.as_os_str(),
        ],
    );
    let original = text(&f.origin, &["rev-parse", "main"]);
    let child = spawn_sync(&f, "version=1\nbranch='main'");
    let ready = barrier.wait();
    git(&f.a, ["checkout", "-b", "feature"]);
    f.commit(&f.a, "feature-only", "not for main");
    let feature = text(&f.a, &["rev-parse", "HEAD"]);
    barrier.release();
    let report = finish_sync(child);
    assert!(ready);
    assert_eq!(report.outcome, Outcome::ManualIntervention, "{report:?}");
    assert!(!report.pushed);
    assert_eq!(text(&f.origin, &["rev-parse", "main"]), original);
    assert_eq!(text(&f.a, &["rev-parse", "HEAD"]), feature);
    assert_eq!(text(&f.a, &["branch", "--show-current"]), "feature");
}
