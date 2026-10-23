use git_rsrs::config::Config;
use std::{process::Command, time::Duration};

#[test]
fn cli_help_is_available_without_a_repository_or_config() {
    let output = Command::new(env!("CARGO_BIN_EXE_git-rsrs"))
        .arg("--help")
        .output()
        .unwrap();
    assert!(output.status.success());
    assert!(String::from_utf8_lossy(&output.stdout).contains("Usage:"));
}

#[test]
fn validates_configuration_and_uses_documented_defaults() {
    let config = Config::parse("version = 1\nbranch = 'main'\n").unwrap();
    assert_eq!(config.quiet_period, Duration::from_secs(30));
    assert_eq!(config.interval, Duration::from_secs(300));
    assert_eq!(config.git_timeout, Duration::from_secs(120));
    assert_eq!(config.lock_timeout, Duration::from_secs(30));
    assert_eq!(config.lock_name, "git-rsrs.lock");
    assert_eq!(config.remote, "origin");
    for invalid in [
        "version = 2\nbranch = 'main'",
        "version = 1",
        "version = 1\nbranch = 'main'\nunknown = true",
        "version = 1\nbranch = ''",
        "version = 1\nbranch = 'main'\nquiet_period = '0s'",
        "version = 1\nbranch = 'main'\ninterval = '-1s'",
        "version = 1\nbranch = 'main'\nlock_name = '../index'",
        "version = 1\nbranch = 'main'\nlock_name = 'config'",
        "version = 1\nbranch = 'main'\nlock_name = 'index.lock'",
        "version = 1\nbranch = 'main'\n[[hooks.pre_commit]]\ncommand = []",
        "version = 1\nbranch = 'main'\n[[hooks.pre_commit]]\ncommand = ['x']\nreject_exit_codes = [0]",
        "version = 1\nbranch = 'main'\n[[hooks.pre_commit]]\ncommand = ['x']\nreject_exit_codes = [3,3]",
    ] {
        assert!(Config::parse(invalid).is_err(), "accepted {invalid}");
    }
    let config = Config::parse("version = 1\nbranch = 'main'\n[[hooks.pre_commit]]\ncommand = ['docsctl', 'check', '--changed']\nreject_exit_codes = [3]").unwrap();
    assert_eq!(config.hooks.pre_commit[0].timeout, Duration::from_secs(30));
}
