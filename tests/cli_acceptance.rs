mod common;
use common::*;
use std::{
    fs,
    io::{BufRead, BufReader},
    process::{Command, Stdio},
    thread,
    time::{Duration, Instant},
};

fn installed(f: &Fixture) -> std::path::PathBuf {
    let bin = f.temp.path().join("installed tools");
    fs::create_dir(&bin).unwrap();
    let path = bin.join("git-rsrs");
    fs::copy(env!("CARGO_BIN_EXE_git-rsrs"), &path).unwrap();
    path
}
#[test]
fn installed_binary_uses_explicit_repo_and_interoperates_with_python_lock_and_hooks() {
    let f = Fixture::new();
    let binary = installed(&f);
    let cfg = f.temp.path().join("config.toml");
    let validator = f.temp.path().join("validate.py");
    fs::write(&validator,"import pathlib,sys\nsys.exit(3 if pathlib.Path('shared.txt').read_text()=='invalid' else 0)\n").unwrap();
    fs::write(&cfg,format!("version=1\nbranch='main'\nlock_name='docs-maintenance.lock'\nlock_timeout='50ms'\n[[hooks.pre_commit]]\ncommand=['python3',{:?}]\nreject_exit_codes=[3]\n",validator.to_str().unwrap())).unwrap();
    let run = || {
        Command::new(&binary)
            .arg("--repo")
            .arg(&f.a)
            .arg("--config")
            .arg(&cfg)
            .args(["--json", "sync"])
            .output()
            .unwrap()
    };
    let lock = f.a.join(".git/docs-maintenance.lock");
    let mut holder=Command::new("python3").args(["-c","import fcntl,sys; f=open(sys.argv[1],'a+'); fcntl.flock(f,fcntl.LOCK_EX); print('ready',flush=True); sys.stdin.read()"])
        .arg(&lock).stdin(Stdio::piped()).stdout(Stdio::piped()).spawn().unwrap();
    let mut ready = String::new();
    BufReader::new(holder.stdout.take().unwrap())
        .read_line(&mut ready)
        .unwrap();
    let blocked = run();
    holder.kill().unwrap();
    holder.wait().unwrap();
    assert_eq!(blocked.status.code(), Some(5));
    assert_eq!(
        serde_json::from_slice::<serde_json::Value>(&blocked.stdout).unwrap()["outcome"],
        "lock_timeout"
    );
    fs::write(f.a.join("shared.txt"), "invalid").unwrap();
    let output = run();
    assert_eq!(output.status.code(), Some(3));
    assert_eq!(text(&f.origin, &["show", "main:shared.txt"]), "initial");
    fs::write(f.a.join("shared.txt"), "valid").unwrap();
    let output = run();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert_eq!(
        serde_json::from_slice::<serde_json::Value>(&output.stdout).unwrap()["outcome"],
        "synced"
    );
    assert_eq!(text(&f.origin, &["show", "main:shared.txt"]), "valid");
}
#[test]
fn installed_watch_starts_clean_and_shuts_down_without_changes() {
    let f = Fixture::new();
    let binary = installed(&f);
    let cfg = f.temp.path().join("config.toml");
    fs::write(&cfg, "version=1\nbranch='main'").unwrap();
    let head = text(&f.a, &["rev-parse", "HEAD"]);
    let mut child = Command::new(&binary)
        .arg("--repo")
        .arg(&f.a)
        .arg("--config")
        .arg(cfg)
        .args(["--json", "watch"])
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .unwrap();
    let mut line = String::new();
    BufReader::new(child.stdout.take().unwrap())
        .read_line(&mut line)
        .unwrap();
    assert_eq!(
        serde_json::from_str::<serde_json::Value>(&line).unwrap()["outcome"],
        "noop"
    );
    unsafe {
        libc::kill(child.id() as i32, libc::SIGTERM);
    }
    assert!(child.wait().unwrap().success());
    assert_eq!(text(&f.a, &["rev-parse", "HEAD"]), head);
}
#[test]
fn stopping_active_watch_reports_interruption_and_does_not_leave_hooks_running() {
    let f = Fixture::new();
    let binary = installed(&f);
    let cfg = f.temp.path().join("config.toml");
    let hook = f.temp.path().join("blocking.py");
    let ready = f.temp.path().join("ready");
    fs::write(&hook,format!("import os,pathlib,time\npathlib.Path({:?}).write_text(str(os.getpgrp()))\ntime.sleep(20)\n",ready.to_str().unwrap())).unwrap();
    fs::write(&cfg,format!("version=1\nbranch='main'\nquiet_period='50ms'\n[[hooks.pre_commit]]\ncommand=['python3',{:?}]\n",hook.to_str().unwrap())).unwrap();
    fs::write(f.a.join("new"), "pending").unwrap();
    let mut child = Command::new(&binary)
        .arg("--repo")
        .arg(&f.a)
        .arg("--config")
        .arg(cfg)
        .args(["--json", "watch"])
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .unwrap();
    let deadline = Instant::now() + Duration::from_secs(5);
    while !ready.exists() && Instant::now() < deadline {
        thread::sleep(Duration::from_millis(10));
    }
    unsafe {
        libc::kill(child.id() as i32, libc::SIGTERM);
    }
    let deadline = Instant::now() + Duration::from_secs(4);
    let mut finished = false;
    while Instant::now() < deadline {
        if child.try_wait().unwrap().is_some() {
            finished = true;
            break;
        }
        thread::sleep(Duration::from_millis(10));
    }
    if !finished {
        child.kill().unwrap();
    }
    let output = child.wait_with_output().unwrap();
    let group = fs::read_to_string(&ready).unwrap().parse::<i32>().unwrap();
    let group_alive = unsafe { libc::kill(-group, 0) } == 0;
    if group_alive {
        unsafe {
            libc::kill(-group, libc::SIGKILL);
        }
    }
    assert!(finished);
    assert!(!group_alive);
    assert_eq!(output.status.code(), Some(4));
    let report: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(report["outcome"], "manual_intervention");
    assert_eq!(report["committed"], serde_json::Value::Null);
}

#[test]
fn result_hooks_run_at_checkout_root_even_when_repo_argument_is_a_subdirectory() {
    let f = Fixture::new();
    let binary = installed(&f);
    let nested = f.a.join("nested");
    fs::create_dir(&nested).unwrap();
    let hook = f.temp.path().join("result.py");
    let cfg = f.temp.path().join("config.toml");
    fs::write(&hook,format!("import os,pathlib\nassert pathlib.Path.cwd()==pathlib.Path({:?}).resolve()\nassert pathlib.Path(os.environ['RSRS_REPO'])==pathlib.Path.cwd()\n",f.a.to_str().unwrap())).unwrap();
    fs::write(
        &cfg,
        format!(
            "version=1\nbranch='main'\n[[hooks.on_result]]\ncommand=['python3',{:?}]\n",
            hook.to_str().unwrap()
        ),
    )
    .unwrap();
    let output = Command::new(binary)
        .arg("--repo")
        .arg(nested)
        .arg("--config")
        .arg(cfg)
        .args(["--json", "sync"])
        .output()
        .unwrap();
    assert!(output.status.success());
    let report: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(
        report["diagnostics"].as_array().unwrap().len(),
        0,
        "{report}"
    );
}

#[test]
fn stopping_watch_during_lock_wait_reports_manual_interruption() {
    use std::os::fd::AsRawFd;
    let f = Fixture::new();
    let binary = installed(&f);
    let cfg = f.temp.path().join("config.toml");
    fs::write(&cfg, "version=1\nbranch='main'\nquiet_period='50ms'\n").unwrap();
    fs::write(f.a.join("new"), "pending").unwrap();
    let lock = fs::File::create(f.a.join(".git/git-rsrs.lock")).unwrap();
    unsafe {
        libc::flock(lock.as_raw_fd(), libc::LOCK_EX);
    }
    let child = Command::new(binary)
        .arg("--repo")
        .arg(&f.a)
        .arg("--config")
        .arg(cfg)
        .args(["--json", "watch"])
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .unwrap();
    thread::sleep(Duration::from_millis(700));
    unsafe {
        libc::kill(child.id() as i32, libc::SIGTERM);
    }
    let output = child.wait_with_output().unwrap();
    assert_eq!(output.status.code(), Some(4));
    let report: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(report["outcome"], "manual_intervention");
}
