//! Launch IDF scripts explicitly through Python on both Windows and Unix.
use std::{
    env,
    ffi::OsString,
    path::{Path, PathBuf},
    process::Command,
};

const SETUP_HELP: &str = "Activate the ESP-IDF terminal (export.ps1 on Windows or source export.sh on Linux), then retry. See docs/DEVELOPMENT.md.";

pub struct Invocation {
    program: OsString,
    script: Option<PathBuf>,
}

impl Invocation {
    pub fn resolve(idf: Option<&str>, python: Option<&str>) -> Result<Self, String> {
        Self::with_environment(
            idf,
            python,
            env::var_os("IDF_PATH"),
            env::var_os("IDF_PYTHON_ENV_PATH"),
            env::var_os("PATH"),
        )
    }

    fn with_environment(
        idf: Option<&str>,
        python: Option<&str>,
        idf_root: Option<OsString>,
        python_env: Option<OsString>,
        search_path: Option<OsString>,
    ) -> Result<Self, String> {
        let script =
            match idf {
                Some(path) => PathBuf::from(path),
                None => match idf_root {
                    Some(root) => PathBuf::from(root).join("tools/idf.py"),
                    None => find_script(Path::new("idf.py"), search_path.as_deref()).ok_or_else(
                        || format!("Cannot locate idf.py; IDF_PATH is not set. {SETUP_HELP}"),
                    )?,
                },
            };
        if !script
            .extension()
            .is_some_and(|ext| ext.eq_ignore_ascii_case("py"))
        {
            if python.is_some() {
                return Err("--python requires --idf to name a Python script".into());
            }
            // Preserve support for an explicit executable IDF wrapper.
            return Ok(Self {
                program: script.into_os_string(),
                script: None,
            });
        }
        let script = find_script(&script, search_path.as_deref()).ok_or_else(|| {
            format!(
                "IDF script does not exist: {}. {SETUP_HELP}",
                script.display()
            )
        })?;
        let program = match (python, python_env) {
            (Some(program), _) => OsString::from(program),
            (None, Some(root)) => {
                let executable = PathBuf::from(root).join(if cfg!(windows) {
                    "Scripts/python.exe"
                } else {
                    "bin/python"
                });
                if !executable.is_file() {
                    return Err(format!(
                        "IDF_PYTHON_ENV_PATH has no interpreter at {}. {SETUP_HELP}",
                        executable.display()
                    ));
                }
                executable.into_os_string()
            }
            (None, None) => OsString::from(if cfg!(windows) { "python" } else { "python3" }),
        };
        Ok(Self {
            program,
            script: Some(script),
        })
    }

    pub fn command(&self) -> Command {
        let mut command = Command::new(&self.program);
        if let Some(script) = &self.script {
            command.arg(script);
        }
        command
    }
}

fn find_script(script: &Path, search_path: Option<&std::ffi::OsStr>) -> Option<PathBuf> {
    if script.is_file() {
        return Some(script.to_path_buf());
    }
    if script.components().count() != 1 {
        return None;
    }
    env::split_paths(search_path?)
        .map(|dir| dir.join(script))
        .find(|path| path.is_file())
}

fn output(label: &str, command: &mut Command) -> Result<String, String> {
    let result = command
        .output()
        .map_err(|error| format!("{label}: cannot start {command:?}: {error}"))?;
    if !result.status.success() {
        return Err(format!(
            "{label}: {command:?} exited with {}\n{}{}",
            result.status,
            String::from_utf8_lossy(&result.stdout),
            String::from_utf8_lossy(&result.stderr)
        ));
    }
    let text = String::from_utf8_lossy(&result.stdout).trim().to_owned();
    println!("[OK] {label}: {text}");
    Ok(text)
}

pub fn check(idf: Option<&str>, python: Option<&str>) -> Result<(), String> {
    let mut errors = Vec::new();
    let idf_result = Invocation::resolve(idf, python).and_then(|invocation| {
        if invocation.script.is_none() {
            return Err("firmware-check requires the actual idf.py script; an executable wrapper may report its own version without loading the SDK. firmware-build still accepts wrappers.".into());
        }
        output("ESP-IDF", invocation.command().arg("--version"))
    });
    if let Err(error) = idf_result {
        errors.push(error);
    }
    for tool in ["cmake", "ninja"] {
        if let Err(error) = output(tool, Command::new(tool).arg("--version")) {
            errors.push(error);
        }
    }
    if let Err(error) = output(
        "Rust esp",
        Command::new("rustup").args(["run", "esp", "rustc", "--version"]),
    ) {
        errors.push(error);
    } else {
        match output(
            "Rust esp sysroot",
            Command::new("rustup").args(["run", "esp", "rustc", "--print", "sysroot"]),
        ) {
            Ok(sysroot)
                if Path::new(&sysroot)
                    .join("lib/rustlib/src/rust/library/core/Cargo.toml")
                    .is_file() =>
            {
                println!("[OK] Rust source for -Z build-std=core")
            }
            Ok(_) => errors.push(
                "Rust esp is missing rust-src; reinstall it with espup (see the development guide)"
                    .into(),
            ),
            Err(error) => errors.push(error),
        }
    }
    if errors.is_empty() {
        println!("Firmware tools are available. This checks tools only; run firmware-build to verify a board profile.");
        Ok(())
    } else {
        Err(format!(
            "Firmware environment is incomplete:\n- {}\n{SETUP_HELP}",
            errors.join("\n- ")
        ))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    #[test]
    fn idf_and_python_paths_with_spaces_remain_single_arguments() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().join("SDK with spaces");
        fs::create_dir_all(root.join("tools")).unwrap();
        let script = root.join("tools/idf.py");
        fs::write(&script, "").unwrap();
        let invocation = Invocation::with_environment(
            None,
            Some("Python with spaces/python"),
            Some(root.into_os_string()),
            None,
            None,
        )
        .unwrap();
        let mut command = invocation.command();
        command.arg("-C").arg("project with spaces");
        assert_eq!(command.get_program(), "Python with spaces/python");
        assert_eq!(
            command.get_args().collect::<Vec<_>>(),
            vec![
                script.as_os_str(),
                std::ffi::OsStr::new("-C"),
                std::ffi::OsStr::new("project with spaces")
            ]
        );
    }

    #[test]
    fn explicit_script_overrides_idf_path_and_python_overrides_broken_venv() {
        let dir = tempfile::tempdir().unwrap();
        let script = dir.path().join("idf.py");
        fs::write(&script, "").unwrap();
        let invocation = Invocation::with_environment(
            Some(script.to_str().unwrap()),
            Some("chosen-python"),
            Some("missing-idf".into()),
            Some("missing-venv".into()),
            None,
        )
        .unwrap();
        assert_eq!(invocation.command().get_program(), "chosen-python");
        assert_eq!(
            invocation.command().get_args().next(),
            Some(script.as_os_str())
        );
    }

    #[test]
    fn missing_environment_and_stale_paths_fail_without_fallback() {
        assert!(Invocation::with_environment(None, None, None, None, None).is_err());
        assert!(
            Invocation::with_environment(Some("missing/idf.py"), None, None, None, None).is_err()
        );
        let dir = tempfile::tempdir().unwrap();
        let script = dir.path().join("idf.py");
        fs::write(&script, "").unwrap();
        assert!(Invocation::with_environment(
            Some(script.to_str().unwrap()),
            None,
            None,
            Some(dir.path().as_os_str().to_owned()),
            None
        )
        .is_err());
    }

    #[test]
    fn searches_path_and_uses_the_idf_virtual_environment() {
        let dir = tempfile::tempdir().unwrap();
        let script = dir.path().join("idf.py");
        fs::write(&script, "").unwrap();
        let interpreter = dir.path().join(if cfg!(windows) {
            "Scripts/python.exe"
        } else {
            "bin/python"
        });
        fs::create_dir_all(interpreter.parent().unwrap()).unwrap();
        fs::write(&interpreter, "").unwrap();
        let invocation = Invocation::with_environment(
            None,
            None,
            None,
            Some(dir.path().as_os_str().to_owned()),
            Some(env::join_paths([dir.path()]).unwrap()),
        )
        .unwrap();
        assert_eq!(invocation.command().get_program(), interpreter.as_os_str());
        assert_eq!(
            invocation.command().get_args().next(),
            Some(script.as_os_str())
        );
    }

    #[test]
    fn executable_wrapper_is_preserved_but_cannot_ignore_python_override() {
        let invocation =
            Invocation::with_environment(Some("custom-idf.exe"), None, None, None, None).unwrap();
        assert_eq!(invocation.command().get_program(), "custom-idf.exe");
        assert_eq!(invocation.command().get_args().count(), 0);
        assert!(Invocation::with_environment(
            Some("custom-idf.exe"),
            Some("python"),
            None,
            None,
            None
        )
        .is_err());
    }
}
