//! These commands stop at parsing/help; they never start a service or touch a device.
use std::process::{Command, Stdio};

fn invoke(args: &[&str]) -> std::process::Output {
    Command::new(env!("CARGO_BIN_EXE_wayline-cli"))
        .args(args)
        .stdin(Stdio::null())
        .output()
        .unwrap()
}

#[test]
fn version_has_no_diagnostic_or_terminal_event() {
    let output = invoke(&["--version"]);
    assert!(output.status.success());
    assert_eq!(
        String::from_utf8(output.stdout).unwrap().trim(),
        concat!("wayline-cli ", env!("CARGO_PKG_VERSION"))
    );
    assert!(output.stderr.is_empty());
}

#[test]
fn unattended_failures_have_one_json_terminal_event_and_exit_two() {
    for args in [
        vec!["receive", "--json"],
        vec!["send", "--json", "test.txt"],
        vec!["discover", "--json", "--iface-index", "1"],
        vec![
            "firmware",
            "flash",
            "--json",
            "--port",
            "COM4",
            "--board",
            "cardputer-adv",
            "--reinstall",
        ],
    ] {
        let output = invoke(&args);
        assert_eq!(output.status.code(), Some(2));
        let stdout = String::from_utf8(output.stdout).unwrap();
        let events: Vec<serde_json::Value> = stdout
            .lines()
            .map(|line| serde_json::from_str(line).unwrap())
            .collect();
        assert_eq!(events.len(), 1);
        assert_eq!(events[0]["schemaVersion"], 1);
        assert_eq!(events[0]["event"], "result");
        assert_eq!(events[0]["data"]["exitCode"], 2);
        assert_eq!(events[0]["data"]["ok"], false);
        assert!(output.stderr.is_empty());
    }
}

#[test]
fn json_help_is_structured_and_has_a_success_terminal_event() {
    let output = invoke(&["--json", "--help"]);
    assert!(output.status.success());
    let events: Vec<serde_json::Value> = String::from_utf8(output.stdout)
        .unwrap()
        .lines()
        .map(|line| serde_json::from_str(line).unwrap())
        .collect();
    assert_eq!(events[0]["event"], "help");
    assert_eq!(events.last().unwrap()["data"]["exitCode"], 0);
    assert!(output.stderr.is_empty());
}

#[test]
fn radio_policy_failures_report_the_full_command_name() {
    let output = invoke(&["radio", "set", "--band", "5ghz", "--channel", "6", "--json"]);
    assert_eq!(output.status.code(), Some(2));
    let events: Vec<serde_json::Value> = String::from_utf8(output.stdout)
        .unwrap()
        .lines()
        .map(|line| serde_json::from_str(line).unwrap())
        .collect();
    assert_eq!(events[0]["event"], "result");
    assert_eq!(events[0]["data"]["command"], "radio set");
    assert_eq!(events[0]["data"]["exitCode"], 2);
}

#[test]
fn firmware_info_reports_the_full_command_name() {
    let output = invoke(&["firmware", "info", "--board", "esp32c5-uart0", "--json"]);
    let stdout = String::from_utf8(output.stdout).unwrap();
    let terminal = stdout
        .lines()
        .map(|line| serde_json::from_str::<serde_json::Value>(line).unwrap())
        .next_back()
        .unwrap();
    assert_eq!(terminal["event"], "result");
    assert_eq!(terminal["data"]["command"], "firmware info");
}
