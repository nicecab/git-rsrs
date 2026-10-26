use git_rsrs::process::{CommandSpec, ProcessError, ProcessRunner};
use std::{
    fs,
    time::{Duration, Instant},
};

fn run(code: &str, timeout: Duration) -> Result<git_rsrs::process::CommandOutput, ProcessError> {
    let cwd = std::env::current_dir().unwrap();
    ProcessRunner.run(&CommandSpec::new(["python3", "-c", code], cwd, timeout))
}

#[test]
fn literal_arguments_and_closed_stdin() {
    let temp = tempfile::tempdir().unwrap();
    let mut spec = CommandSpec::new(
        [
            "python3",
            "-c",
            "import sys; print(repr(sys.argv[1])); print(len(sys.stdin.read()))",
            "a b;$(false)",
        ],
        temp.path(),
        Duration::from_secs(3),
    );
    spec.capture_limit = 100;
    let output = ProcessRunner.run(&spec).unwrap();
    assert_eq!(output.code, Some(0));
    assert_eq!(output.stdout, b"'a b;$(false)'\n0\n");
}

#[test]
fn drains_both_pipes_and_caps_capture() {
    let temp = tempfile::tempdir().unwrap();
    let mut spec = CommandSpec::new(
        [
            "python3",
            "-c",
            "import os; [(os.write(1,b'a'*8192),os.write(2,b'b'*8192)) for _ in range(64)]",
        ],
        temp.path(),
        Duration::from_secs(5),
    );
    spec.capture_limit = 1000;
    let output = ProcessRunner.run(&spec).unwrap();
    assert_eq!(output.code, Some(0));
    assert_eq!(output.stdout.len(), 1000);
    assert_eq!(output.stderr.len(), 1000);
    assert!(output.truncated);
}

#[test]
fn inherited_pipe_cannot_wait_forever_after_leader_exits() {
    let start = Instant::now();
    let result = run(
        "import os,time; p=os.fork(); time.sleep(20) if p==0 else None",
        Duration::from_millis(100),
    );
    assert!(matches!(result, Err(ProcessError::Timeout(_))));
    assert!(start.elapsed() < Duration::from_secs(4));
}

#[test]
fn timeout_kills_descendants_even_if_the_leader_dies_on_term() {
    let temp = tempfile::tempdir().unwrap();
    let marker = temp.path().join("escaped");
    let code = format!(
        "import os,signal,time; p=os.fork();\nif p==0:\n signal.signal(signal.SIGTERM,signal.SIG_IGN); time.sleep(2); open({:?},'w').write('bad')\nelse: time.sleep(20)",
        marker.to_str().unwrap()
    );
    assert!(matches!(
        run(&code, Duration::from_millis(150)),
        Err(ProcessError::Timeout(_))
    ));
    std::thread::sleep(Duration::from_secs(1));
    assert!(!marker.exists());
}

#[test]
fn preserves_signals_and_spawn_errors() {
    let output = run(
        "import os,signal; os.kill(os.getpid(),signal.SIGTERM)",
        Duration::from_secs(3),
    )
    .unwrap();
    assert_eq!(output.signal, Some(15));
    let temp = tempfile::tempdir().unwrap();
    let result = ProcessRunner.run(&CommandSpec::new(
        ["/no/such/program"],
        temp.path(),
        Duration::from_secs(1),
    ));
    assert!(matches!(result, Err(ProcessError::Io(_))));
    assert_eq!(fs::read_dir(temp.path()).unwrap().count(), 0);
}

#[test]
#[ignore = "timing benchmark; run with --ignored --nocapture"]
fn bench_short_git_commands() {
    let cwd = std::env::current_dir().unwrap();
    let spec = CommandSpec::new(["git", "rev-parse", "HEAD"], cwd, Duration::from_secs(5));
    let start = Instant::now();
    for _ in 0..200 {
        assert_eq!(ProcessRunner.run(&spec).unwrap().code, Some(0));
    }
    eprintln!("200 x git rev-parse HEAD: {:?}", start.elapsed());
}
