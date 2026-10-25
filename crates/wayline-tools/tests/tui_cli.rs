use std::process::Command;

#[test]
fn preview_renders_without_a_terminal_or_ansi_control_sequences() {
    let output = Command::new(env!("CARGO_BIN_EXE_wayline-tools"))
        .args(["tui", "--preview"])
        .output()
        .unwrap();
    assert!(output.status.success(), "{:?}", output);
    assert!(!output.stdout.contains(&0x1b));
    assert!(String::from_utf8(output.stdout).unwrap().lines().count() >= 38);
}

#[test]
fn noninteractive_launch_and_unknown_options_fail_without_changing_terminal_state() {
    for args in [
        vec!["tui"],
        vec!["tui", "--invalid"],
        vec!["tui", "--preview", "--invalid"],
        vec!["tui", "--lang"],
        vec!["tui", "--lang", "fr-FR", "--preview"],
        vec!["tui", "--lang", "en-US", "--lang", "zh-CN", "--preview"],
        vec!["tui", "--lang", "en-US", "--preview", "--help"],
    ] {
        let output = Command::new(env!("CARGO_BIN_EXE_wayline-tools"))
            .args(args)
            .output()
            .unwrap();
        assert!(!output.status.success());
        assert!(!output.stderr.is_empty());
        assert!(!output.stdout.contains(&0x1b));
        assert!(!output.stderr.contains(&0x1b));
    }
}

#[test]
fn startup_language_applies_to_preview_and_help_in_either_argument_order() {
    for mode in ["--preview", "--help"] {
        for (language, marker, other) in
            [("en-US", "switches", "切换"), ("zh-CN", "切换", "switches")]
        {
            for args in [
                vec!["tui", mode, "--lang", language],
                vec!["tui", "--lang", language, mode],
            ] {
                let output = Command::new(env!("CARGO_BIN_EXE_wayline-tools"))
                    .args(args)
                    .output()
                    .unwrap();
                assert!(output.status.success(), "{output:?}");
                let screen = String::from_utf8(output.stdout).unwrap();
                // Preview's short footer uses a label rather than the help's verb.
                let marker = if mode == "--preview" && language == "en-US" {
                    "Language"
                } else {
                    marker
                };
                assert!(screen.contains(marker), "{screen}");
                assert!(!screen.contains(other), "{screen}");
                assert!(screen.contains("L "));
                assert!(!screen.contains("F2"));
            }
        }
    }
}
