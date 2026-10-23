#![allow(dead_code)]
use git_rsrs::process::{CommandOutput, CommandSpec, ProcessRunner};
use std::{
    ffi::OsStr,
    fs,
    path::{Path, PathBuf},
    time::Duration,
};

pub fn git<I, S>(cwd: &Path, args: I) -> CommandOutput
where
    I: IntoIterator<Item = S>,
    S: AsRef<OsStr>,
{
    let mut argv = vec!["git".into()];
    argv.extend(args.into_iter().map(|s| s.as_ref().to_os_string()));
    let mut spec = CommandSpec::new(argv, cwd, Duration::from_secs(10));
    spec.env.insert("GIT_CONFIG_NOSYSTEM".into(), "1".into());
    spec.env
        .insert("GIT_CONFIG_GLOBAL".into(), "/dev/null".into());
    let output = ProcessRunner.run(&spec).unwrap();
    assert_eq!(
        output.code,
        Some(0),
        "git failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    output
}
pub fn text(cwd: &Path, args: &[&str]) -> String {
    String::from_utf8(git(cwd, args).stdout)
        .unwrap()
        .trim_end_matches('\n')
        .into()
}
pub fn identity(cwd: &Path) {
    git(cwd, ["config", "user.name", "Test User"]);
    git(cwd, ["config", "user.email", "test@example.invalid"]);
    git(cwd, ["config", "commit.gpgsign", "false"]);
    git(cwd, ["config", "core.hooksPath", "/dev/null"]);
}
pub struct Fixture {
    pub temp: tempfile::TempDir,
    pub origin: PathBuf,
    pub a: PathBuf,
    pub b: PathBuf,
}
impl Fixture {
    pub fn new() -> Self {
        let temp = tempfile::tempdir().unwrap();
        let origin = temp.path().join("origin.git");
        let a = temp.path().join("repo a");
        let b = temp.path().join("repo b");
        git(
            temp.path(),
            [OsStr::new("init"), OsStr::new("--bare"), origin.as_os_str()],
        );
        git(
            temp.path(),
            [OsStr::new("clone"), origin.as_os_str(), a.as_os_str()],
        );
        identity(&a);
        git(&a, ["checkout", "-b", "main"]);
        fs::write(a.join("shared.txt"), "initial\n").unwrap();
        git(&a, ["add", "-A"]);
        git(&a, ["commit", "-m", "initial"]);
        git(&a, ["push", "-u", "origin", "main"]);
        git(&origin, ["symbolic-ref", "HEAD", "refs/heads/main"]);
        git(
            temp.path(),
            [OsStr::new("clone"), origin.as_os_str(), b.as_os_str()],
        );
        identity(&b);
        Self { temp, origin, a, b }
    }
    pub fn commit(&self, repo: &Path, name: &str, content: &str) {
        fs::write(repo.join(name), content).unwrap();
        git(repo, ["add", "-A"]);
        git(repo, ["commit", "-m", "test change"]);
    }
}
