mod artifacts;
mod firmware_package;
mod idf;
mod packaging;
mod tool_package;
mod tui;
use std::{
    collections::HashSet,
    env, fs,
    path::{Path, PathBuf},
    process::Command,
};

fn usage() -> ! {
    eprintln!(
        "usage:\n  wayline-tools tui [--lang zh-CN|en-US] [--preview | --help]\n  wayline-tools host-build [--release] [--test]\n  wayline-tools desktop-build [--release]\n  wayline-tools bridge-build [--release] [--test]\n  wayline-tools firmware-check [--idf PATH] [--python PATH]\n  wayline-tools firmware-build --board BOARD [--output DIR] [--idf PATH] [--python PATH]\n  wayline-tools firmware-build --target TARGET --output DIR [--idf PATH] [--python PATH] [--firmware-dir DIR] [--sdkconfig-defaults VALUE] [--no-component-manager]\n  wayline-tools firmware-package --board BOARD --output DIR [--build-dir DIR]\n  wayline-tools tool-package --output DIR [--platform windows-x64|linux-x64] [--archive FILE] [--cache-dir DIR]\n  wayline-tools package-build --host-dir DIR --board BOARD --output DIR --version VERSION [--firmware-build DIR] [--platform windows-x64|linux-x64] [--esptool-archive FILE] [--wintun-archive FILE] [--cache-dir DIR]\n  wayline-tools package-build --input DIR --output DIR --version VERSION\n  wayline-tools package-verify MANIFEST\n  wayline-tools release-verify DIR\n  wayline-tools service-install --binary PATH [--resident]\n  wayline-tools service-uninstall"
    );
    std::process::exit(2);
}

fn run_cargo(
    root: &PathBuf,
    subcommand: &str,
    packages: &[&str],
    release: bool,
    all_targets: bool,
) {
    let mut command = Command::new("cargo");
    command
        .current_dir(root)
        .arg(subcommand)
        .arg("--manifest-path")
        .arg(root.join("Cargo.toml"))
        .arg("--locked");
    for package in packages {
        command.args(["-p", package]);
    }
    if release {
        command.arg("--release");
    }
    if all_targets {
        command.arg("--all-targets");
    }
    let status = command.status().unwrap_or_else(|error| {
        eprintln!("failed to start cargo {subcommand}: {error}");
        std::process::exit(1);
    });
    if !status.success() {
        std::process::exit(status.code().unwrap_or(1));
    }
}

fn host_build(args: &[String], root: PathBuf) {
    let mut release = false;
    let mut test = false;
    for arg in args {
        match arg.as_str() {
            "--release" => release = true,
            "--test" => test = true,
            _ => usage(),
        }
    }

    let common = [
        "wayline-cli",
        "wayline-core",
        "wayline-host",
        "wayline-proto",
        "wayline-radio",
        "wayline-transport",
    ];
    let windows = ["wayline-bridge", "wayline-desktop", "wayline-service"];
    let mut packages = common.to_vec();
    if cfg!(windows) {
        packages.extend(windows);
    }
    run_cargo(&root, "build", &packages, release, false);
    if test {
        run_cargo(&root, "test", &packages, false, true);
    }
}

fn desktop_build(args: &[String], root: PathBuf) {
    let mut release = false;
    for arg in args {
        match arg.as_str() {
            "--release" => release = true,
            _ => usage(),
        }
    }
    run_cargo(&root, "build", &["wayline-desktop"], release, false);
}

fn bridge_build(args: &[String], root: PathBuf) {
    let mut release = false;
    let mut test = false;
    for arg in args {
        match arg.as_str() {
            "--release" => release = true,
            "--test" => test = true,
            _ => usage(),
        }
    }
    run_cargo(&root, "build", &["wayline-bridge"], release, false);
    if test {
        run_cargo(
            &root,
            "test",
            &["wayline-awdl", "wayline-bridge"],
            false,
            true,
        );
    }
}

fn package_verify(path: &str) {
    let package =
        wayline_flash::validate_package(std::path::Path::new(path)).unwrap_or_else(|error| {
            eprintln!("firmware package validation failed: {error:#}");
            std::process::exit(1);
        });
    println!(
        "{}",
        serde_json::json!({
            "board": package.manifest.board,
            "chip": package.manifest.chip,
            "firmwareVersion": package.manifest.firmware_version,
            "images": package.manifest.images.len(),
        })
    );
}

fn hash_file(path: &Path) -> Result<String, String> {
    artifacts::sha256(path).map_err(|error| format!("{error:#}"))
}

fn copy_tree(source: &Path, root: &Path, destination: &Path, files: &mut Vec<serde_json::Value>) {
    let entries = fs::read_dir(source).unwrap_or_else(|error| {
        eprintln!("cannot read {}: {error}", source.display());
        std::process::exit(1);
    });
    for entry in entries {
        let entry = entry.unwrap_or_else(|error| {
            eprintln!("cannot enumerate {}: {error}", source.display());
            std::process::exit(1);
        });
        let path = entry.path();
        let metadata = fs::symlink_metadata(&path).unwrap_or_else(|error| {
            eprintln!("cannot inspect {}: {error}", path.display());
            std::process::exit(1);
        });
        if metadata.file_type().is_symlink() {
            eprintln!(
                "package inputs may not contain symlinks: {}",
                path.display()
            );
            std::process::exit(1);
        }
        let relative = path.strip_prefix(root).unwrap();
        let target = destination.join(relative);
        if metadata.is_dir() {
            fs::create_dir_all(&target).unwrap_or_else(|error| {
                eprintln!("cannot create {}: {error}", target.display());
                std::process::exit(1);
            });
            copy_tree(&path, root, destination, files);
        } else if metadata.is_file() {
            if relative == Path::new("release.json") {
                continue;
            }
            if let Some(parent) = target.parent() {
                fs::create_dir_all(parent).unwrap_or_else(|error| {
                    eprintln!("cannot create {}: {error}", parent.display());
                    std::process::exit(1);
                });
            }
            fs::copy(&path, &target).unwrap_or_else(|error| {
                eprintln!("cannot copy {}: {error}", path.display());
                std::process::exit(1);
            });
            let relative = relative.to_string_lossy().replace('\\', "/");
            files.push(serde_json::json!({
                "path": relative,
                "length": metadata.len(),
                "sha256": hash_file(&target).unwrap_or_else(|error| { eprintln!("{error}"); std::process::exit(1); }),
            }));
        }
    }
}

fn package_build(args: &[String], root: &Path) {
    if args.chunks(2).any(|pair| pair[0] == "--host-dir") {
        finish(packaging::bundle(args, root));
        return;
    }
    let mut input = None;
    let mut output = None;
    let mut version = None;
    let mut index = 0;
    while index < args.len() {
        let value = args[index].as_str();
        let next = |index: &mut usize, name: &str, args: &[String]| -> String {
            *index += 1;
            args.get(*index).cloned().unwrap_or_else(|| {
                eprintln!("{name} requires a value");
                std::process::exit(2);
            })
        };
        match value {
            "--input" => input = Some(PathBuf::from(next(&mut index, "--input", args))),
            "--output" => output = Some(PathBuf::from(next(&mut index, "--output", args))),
            "--version" => version = Some(next(&mut index, "--version", args)),
            _ => usage(),
        }
        index += 1;
    }
    let input = input.unwrap_or_else(|| {
        eprintln!("--input is required");
        std::process::exit(2)
    });
    let output = output.unwrap_or_else(|| {
        eprintln!("--output is required");
        std::process::exit(2)
    });
    let version = version.unwrap_or_else(|| {
        eprintln!("--version is required");
        std::process::exit(2)
    });
    let input = if input.is_absolute() {
        input
    } else {
        root.join(input)
    };
    let output = if output.is_absolute() {
        output
    } else {
        root.join(output)
    };
    let input = fs::canonicalize(&input).unwrap_or_else(|error| {
        eprintln!("cannot resolve package input {}: {error}", input.display());
        std::process::exit(2);
    });
    if !input.is_dir() || output.exists() {
        eprintln!("package input must be a directory and output must not exist");
        std::process::exit(2);
    }
    let output_parent = output.parent().unwrap_or(root).to_path_buf();
    let output_parent = fs::canonicalize(&output_parent).unwrap_or_else(|error| {
        eprintln!("cannot resolve package output parent: {error}");
        std::process::exit(2);
    });
    let output = output_parent.join(output.file_name().unwrap());
    if output.starts_with(&input) {
        eprintln!("package output must not be inside the input tree");
        std::process::exit(2);
    }
    let staging = output.with_extension("staging");
    if staging.exists() {
        eprintln!("staging directory already exists: {}", staging.display());
        std::process::exit(2);
    }
    if let Some(parent) = output.parent() {
        fs::create_dir_all(parent).unwrap_or_else(|error| {
            eprintln!("cannot create package parent: {error}");
            std::process::exit(1);
        });
    }
    fs::create_dir_all(&staging).unwrap_or_else(|error| {
        eprintln!("cannot create staging directory: {error}");
        std::process::exit(1)
    });
    let mut files = Vec::new();
    copy_tree(&input, &input, &staging, &mut files);
    files.sort_by(|a, b| a["path"].as_str().cmp(&b["path"].as_str()));
    let manifest = serde_json::json!({
        "schemaVersion": 1,
        "platform": if cfg!(windows) { "windows-x64" } else { "host" },
        "version": version,
        "files": files,
    });
    fs::write(
        staging.join("release.json"),
        serde_json::to_vec_pretty(&manifest).unwrap(),
    )
    .unwrap_or_else(|error| {
        eprintln!("cannot write release manifest: {error}");
        std::process::exit(1)
    });
    fs::rename(&staging, &output).unwrap_or_else(|error| {
        eprintln!("cannot finalize package: {error}");
        std::process::exit(1)
    });
    println!("{}", output.display());
}

fn release_verify(path: &str, root: &Path) {
    let package = PathBuf::from(path);
    let package = if package.is_absolute() {
        package
    } else {
        root.join(package)
    };
    let manifest_path = package.join("release.json");
    let value: serde_json::Value =
        serde_json::from_slice(&fs::read(&manifest_path).unwrap_or_else(|error| {
            eprintln!("cannot read release manifest: {error}");
            std::process::exit(1)
        }))
        .unwrap_or_else(|error| {
            eprintln!("invalid release manifest: {error}");
            std::process::exit(1)
        });
    if value["schemaVersion"] != 1
        || value["platform"].as_str().is_none()
        || value["version"].as_str().is_none()
    {
        eprintln!("unsupported release manifest");
        std::process::exit(1);
    }
    let files = value["files"].as_array().unwrap_or_else(|| {
        eprintln!("release manifest has no files array");
        std::process::exit(1)
    });
    let mut seen = HashSet::new();
    for entry in files {
        let relative = entry["path"].as_str().unwrap_or_else(|| {
            eprintln!("release entry has no path");
            std::process::exit(1)
        });
        let relative_path = Path::new(relative);
        if !seen.insert(relative.to_owned()) || relative == "release.json" {
            eprintln!("duplicate or reserved release path: {relative}");
            std::process::exit(1);
        }
        if relative_path.is_absolute()
            || relative_path
                .components()
                .any(|component| !matches!(component, std::path::Component::Normal(_)))
        {
            eprintln!("unsafe release path: {relative}");
            std::process::exit(1);
        }
        let file = package.join(relative_path);
        let symlink = fs::symlink_metadata(&file).unwrap_or_else(|error| {
            eprintln!("missing release file {relative}: {error}");
            std::process::exit(1)
        });
        if symlink.file_type().is_symlink() || !symlink.is_file() {
            eprintln!("release file must be a regular file: {relative}");
            std::process::exit(1);
        }
        let metadata = symlink;
        let expected_length = entry["length"].as_u64().unwrap_or(u64::MAX);
        let expected_hash = entry["sha256"].as_str().unwrap_or("");
        let actual_hash = hash_file(&file).unwrap_or_default();
        if metadata.len() != expected_length || actual_hash != expected_hash {
            eprintln!("release file verification failed: {relative}");
            std::process::exit(1);
        }
    }
    fn walk(root: &Path, dir: &Path, seen: &HashSet<String>) {
        for entry in fs::read_dir(dir).unwrap_or_else(|error| {
            eprintln!("cannot enumerate release directory: {error}");
            std::process::exit(1);
        }) {
            let path = entry
                .unwrap_or_else(|error| {
                    eprintln!("cannot enumerate release entry: {error}");
                    std::process::exit(1);
                })
                .path();
            let metadata = fs::symlink_metadata(&path).unwrap_or_else(|error| {
                eprintln!("cannot inspect release entry: {error}");
                std::process::exit(1);
            });
            if metadata.file_type().is_symlink() {
                eprintln!("release package may not contain symlinks");
                std::process::exit(1);
            }
            if metadata.is_dir() {
                walk(root, &path, seen);
            } else if metadata.is_file() {
                let relative = path
                    .strip_prefix(root)
                    .unwrap()
                    .to_string_lossy()
                    .replace('\\', "/");
                if relative != "release.json" && !seen.contains(&relative) {
                    eprintln!("unlisted release file: {relative}");
                    std::process::exit(1);
                }
            }
        }
    }
    walk(&package, &package, &seen);
    println!("verified {} files", files.len());
}

#[cfg(windows)]
fn wait_for_service_state(
    service: &windows_service::service::Service,
    expected: windows_service::service::ServiceState,
) -> Result<(), String> {
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(20);
    loop {
        let state = service
            .query_status()
            .map_err(|error| format!("cannot query Wayline state: {error}"))?
            .current_state;
        if state == expected {
            return Ok(());
        }
        if std::time::Instant::now() >= deadline {
            return Err(format!(
                "timed out waiting for Wayline to become {expected:?}"
            ));
        }
        std::thread::sleep(std::time::Duration::from_millis(100));
    }
}

#[cfg(windows)]
fn restore_service_config(
    service: &windows_service::service::Service,
    previous: &windows_service::service::ServiceConfig,
) -> Result<(), String> {
    use std::{os::windows::ffi::OsStrExt, ptr};
    use windows_sys::Win32::System::Services::{ChangeServiceConfigW, SERVICE_NO_CHANGE};

    let command: Vec<u16> = previous
        .executable_path
        .as_os_str()
        .encode_wide()
        .chain(std::iter::once(0))
        .collect();
    // SAFETY: the service handle and NUL-terminated command are valid; null arguments preserve
    // every unrelated configuration field.
    let changed = unsafe {
        ChangeServiceConfigW(
            service.raw_handle(),
            SERVICE_NO_CHANGE,
            previous.start_type.to_raw(),
            previous.error_control.to_raw(),
            command.as_ptr(),
            ptr::null(),
            ptr::null_mut(),
            ptr::null(),
            ptr::null(),
            ptr::null(),
            ptr::null(),
        )
    };
    if changed == 0 {
        Err(format!(
            "failed to restore Wayline config: {}",
            std::io::Error::last_os_error()
        ))
    } else {
        Ok(())
    }
}

#[cfg(windows)]
fn grant_interactive_service_control(
    service: &windows_service::service::Service,
) -> Result<(), String> {
    use std::{ffi::c_void, ptr};
    use windows_sys::Win32::{
        Foundation::LocalFree,
        Security::Authorization::ConvertStringSecurityDescriptorToSecurityDescriptorW,
        Security::{DACL_SECURITY_INFORMATION, PSECURITY_DESCRIPTOR},
        System::Services::SetServiceObjectSecurity,
    };
    let sddl: Vec<u16> = "D:P(A;;GA;;;SY)(A;;GA;;;BA)(A;;0x34;;;IU)"
        .encode_utf16()
        .chain(std::iter::once(0))
        .collect();
    let mut descriptor: PSECURITY_DESCRIPTOR = ptr::null_mut();
    // SAFETY: the SDDL is NUL-terminated and the output pointer is valid.
    let converted = unsafe {
        ConvertStringSecurityDescriptorToSecurityDescriptorW(
            sddl.as_ptr(),
            1,
            &mut descriptor,
            ptr::null_mut(),
        )
    };
    if converted == 0 {
        return Err(format!(
            "cannot create Wayline service permissions: {}",
            std::io::Error::last_os_error()
        ));
    }
    // SAFETY: descriptor came from the conversion API and remains valid for this call.
    let applied = unsafe {
        SetServiceObjectSecurity(service.raw_handle(), DACL_SECURITY_INFORMATION, descriptor)
    };
    let result = if applied == 0 {
        Err(format!(
            "cannot set Wayline service permissions: {}",
            std::io::Error::last_os_error()
        ))
    } else {
        Ok(())
    };
    // SAFETY: LocalFree owns the descriptor returned by the conversion API.
    unsafe {
        LocalFree(descriptor.cast::<c_void>());
    }
    result
}

#[cfg(windows)]
fn service_install(args: &[String]) {
    use std::ffi::OsString;
    use windows_service::{
        service::{
            ServiceAccess, ServiceAction, ServiceActionType, ServiceErrorControl,
            ServiceFailureActions, ServiceFailureResetPeriod, ServiceInfo, ServiceStartType,
            ServiceType,
        },
        service_manager::{ServiceManager, ServiceManagerAccess},
    };

    let mut binary = None;
    let mut resident = false;
    let mut index = 0;
    while index < args.len() {
        match args[index].as_str() {
            "--binary" => {
                index += 1;
                binary = args.get(index).cloned();
            }
            "--resident" => resident = true,
            _ => usage(),
        }
        index += 1;
    }
    let Some(binary) = binary else {
        eprintln!("--binary is required");
        std::process::exit(2);
    };
    let path = PathBuf::from(binary);
    if !path.is_absolute() || !path.is_file() {
        eprintln!(
            "service binary must be an existing absolute path: {}",
            path.display()
        );
        std::process::exit(2);
    }
    let manager = ServiceManager::local_computer(
        None::<&str>,
        ServiceManagerAccess::CONNECT | ServiceManagerAccess::CREATE_SERVICE,
    )
    .unwrap_or_else(|error| {
        eprintln!("cannot open Windows service manager: {error}");
        std::process::exit(1);
    });
    let info = ServiceInfo {
        name: OsString::from("Wayline"),
        display_name: OsString::from("Wayline AirDrop radio"),
        service_type: ServiceType::OWN_PROCESS,
        start_type: if resident {
            ServiceStartType::AutoStart
        } else {
            ServiceStartType::OnDemand
        },
        error_control: ServiceErrorControl::Normal,
        executable_path: path,
        launch_arguments: vec![
            OsString::from("--service"),
            OsString::from(if resident {
                "--resident"
            } else {
                "--on-demand"
            }),
        ],
        dependencies: vec![],
        account_name: None,
        account_password: None,
    };
    let access = ServiceAccess::QUERY_STATUS
        | ServiceAccess::START
        | ServiceAccess::STOP
        | ServiceAccess::CHANGE_CONFIG
        | ServiceAccess::DELETE;
    let (service, existed, old_config, was_running) = match manager.create_service(&info, access) {
        Ok(service) => (service, false, None, false),
        Err(_) => {
            let service = manager
                .open_service("Wayline", access)
                .unwrap_or_else(|error| {
                    eprintln!("cannot install or update Wayline: {error}");
                    std::process::exit(1);
                });
            let old = service.query_config().unwrap_or_else(|error| {
                eprintln!("cannot read existing Wayline configuration: {error}");
                std::process::exit(1);
            });
            let was_running = service
                .query_status()
                .map(|status| {
                    status.current_state != windows_service::service::ServiceState::Stopped
                })
                .unwrap_or(false);
            if was_running {
                service.stop().unwrap_or_else(|error| {
                    eprintln!("cannot stop Wayline before update: {error}");
                    std::process::exit(1);
                });
                wait_for_service_state(&service, windows_service::service::ServiceState::Stopped)
                    .unwrap_or_else(|error| {
                        eprintln!("{error}; service configuration was not changed");
                        std::process::exit(1);
                    });
            }
            service.change_config(&info).unwrap_or_else(|error| {
                let rollback = restore_service_config(&service, &old);
                eprintln!(
                    "cannot update Wayline: {error}; rollback: {}",
                    rollback.err().unwrap_or_else(|| "complete".into())
                );
                std::process::exit(1);
            });
            (service, true, Some(old), was_running)
        }
    };
    if !existed {
        let setup = (|| -> Result<(), String> {
            service
                .set_description("Wayline AirDrop radio service")
                .map_err(|error| format!("cannot set Wayline description: {error}"))?;
            service
                .update_failure_actions(ServiceFailureActions {
                    reset_period: ServiceFailureResetPeriod::After(std::time::Duration::from_secs(
                        86_400,
                    )),
                    reboot_msg: None,
                    command: None,
                    actions: Some(vec![
                        ServiceAction {
                            action_type: ServiceActionType::Restart,
                            delay: std::time::Duration::from_secs(5),
                        },
                        ServiceAction {
                            action_type: ServiceActionType::Restart,
                            delay: std::time::Duration::from_secs(30),
                        },
                    ]),
                })
                .map_err(|error| format!("cannot set Wayline recovery actions: {error}"))?;
            service
                .set_failure_actions_on_non_crash_failures(true)
                .map_err(|error| format!("cannot enable Wayline recovery actions: {error}"))?;
            grant_interactive_service_control(&service)?;
            Ok(())
        })();
        if let Err(error) = setup {
            let _ = service.delete();
            eprintln!("{error}; failed service registration was removed");
            std::process::exit(1);
        }
    }
    if resident || was_running {
        let start_result = service
            .start::<&str>(&[])
            .map_err(|error| format!("cannot start Wayline: {error}"))
            .and_then(|()| {
                wait_for_service_state(&service, windows_service::service::ServiceState::Running)
            });
        if let Err(error) = start_result {
            if let Some(old) = old_config.as_ref() {
                if let Err(rollback_error) = restore_service_config(&service, old) {
                    eprintln!("{error}; rollback failed: {rollback_error}");
                    std::process::exit(1);
                }
                if was_running {
                    let _ = service.start::<&str>(&[]);
                }
            } else if let Err(rollback_error) = service.delete() {
                eprintln!("{error}; failed to remove service registration: {rollback_error}");
                std::process::exit(1);
            }
            eprintln!("{error}; previous service state was restored");
            std::process::exit(1);
        }
    }
    println!("installed Wayline");
}

#[cfg(not(windows))]
fn service_install(_: &[String]) {
    eprintln!("service-install requires Windows");
    std::process::exit(1);
}

#[cfg(windows)]
fn service_uninstall() {
    use windows_service::{
        service::ServiceAccess,
        service_manager::{ServiceManager, ServiceManagerAccess},
    };
    let manager = ServiceManager::local_computer(None::<&str>, ServiceManagerAccess::CONNECT)
        .unwrap_or_else(|error| {
            eprintln!("cannot open Windows service manager: {error}");
            std::process::exit(1);
        });
    match manager.open_service("Wayline", ServiceAccess::DELETE | ServiceAccess::STOP) {
        Ok(service) => {
            if let Ok(status) = service.query_status() {
                if status.current_state != windows_service::service::ServiceState::Stopped {
                    service.stop().unwrap_or_else(|error| {
                        eprintln!("cannot stop Wayline: {error}");
                        std::process::exit(1);
                    });
                    wait_for_service_state(
                        &service,
                        windows_service::service::ServiceState::Stopped,
                    )
                    .unwrap_or_else(|error| {
                        eprintln!("{error}; service registration was not removed");
                        std::process::exit(1);
                    });
                }
            }
            service.delete().unwrap_or_else(|error| {
                eprintln!("cannot remove Wayline: {error}");
                std::process::exit(1);
            })
        }
        Err(error) => {
            eprintln!("cannot open Wayline: {error}");
            std::process::exit(1);
        }
    }
    println!("uninstalled Wayline");
}

#[cfg(not(windows))]
fn service_uninstall() {
    eprintln!("service-uninstall requires Windows");
    std::process::exit(1);
}

fn firmware_build(args: &[String], root: PathBuf) {
    let mut board = None;
    let mut target = None;
    let mut output = None;
    let mut idf_path = None;
    let mut python = None;
    let mut firmware_dir = root.join("firmware");
    let mut sdkconfig_defaults = None;
    let mut no_component_manager = false;
    let mut index = 0;
    while index < args.len() {
        let value = args[index].as_str();
        let next = |index: &mut usize, name: &str, args: &[String]| -> String {
            *index += 1;
            args.get(*index).cloned().unwrap_or_else(|| {
                eprintln!("{name} requires a value");
                std::process::exit(2);
            })
        };
        match value {
            "--board" => board = Some(next(&mut index, "--board", args)),
            "--target" => target = Some(next(&mut index, "--target", args)),
            "--output" => output = Some(PathBuf::from(next(&mut index, "--output", args))),
            "--idf" => idf_path = Some(next(&mut index, "--idf", args)),
            "--python" => python = Some(next(&mut index, "--python", args)),
            "--firmware-dir" => {
                firmware_dir = PathBuf::from(next(&mut index, "--firmware-dir", args))
            }
            "--sdkconfig-defaults" => {
                sdkconfig_defaults = Some(next(&mut index, "--sdkconfig-defaults", args))
            }
            "--no-component-manager" => no_component_manager = true,
            _ => usage(),
        }
        index += 1;
    }

    if let Some(board) = board.as_deref() {
        if target.is_some() || sdkconfig_defaults.is_some() || no_component_manager {
            eprintln!("--board cannot be combined with --target, --sdkconfig-defaults or --no-component-manager");
            std::process::exit(2);
        }
        let (chip, defaults, headless) =
            firmware_package::defaults(board).unwrap_or_else(|error| {
                eprintln!("{error:#}");
                std::process::exit(2);
            });
        target = Some(chip.into());
        sdkconfig_defaults = Some(defaults.into());
        no_component_manager = headless;
        if output.is_none() {
            output =
                Some(firmware_package::default_build_dir(&root, board).expect("validated board"));
        }
    }
    let target = target.unwrap_or_else(|| {
        eprintln!("--target is required");
        std::process::exit(2);
    });
    if target != "esp32s3" && target != "esp32c5" {
        eprintln!("unsupported firmware target: {target}");
        std::process::exit(2);
    }
    let output = output.unwrap_or_else(|| {
        eprintln!("--output is required");
        std::process::exit(2);
    });
    let output = if output.is_absolute() {
        output
    } else {
        root.join(output)
    };
    let firmware_dir = if firmware_dir.is_absolute() {
        firmware_dir
    } else {
        root.join(firmware_dir)
    };
    if !firmware_dir.is_dir() {
        eprintln!(
            "firmware directory does not exist: {}",
            firmware_dir.display()
        );
        std::process::exit(2);
    }

    let invocation = idf::Invocation::resolve(idf_path.as_deref(), python.as_deref())
        .unwrap_or_else(|error| {
            eprintln!("{error}");
            std::process::exit(1);
        });
    let mut command = invocation.command();
    command
        .current_dir(&root)
        .arg("-C")
        .arg(&firmware_dir)
        .arg("-B")
        .arg(&output)
        .args(["-D", &format!("IDF_TARGET={target}")])
        .args([
            "-D",
            &format!("SDKCONFIG={}", output.join("sdkconfig").display()),
        ]);
    if let Some(defaults) = sdkconfig_defaults {
        command.args(["-D", &format!("SDKCONFIG_DEFAULTS={defaults}")]);
    }
    if no_component_manager {
        command.env("IDF_COMPONENT_MANAGER", "0");
        command.args(["-D", "EXCLUDE_COMPONENTS=m5stack__m5gfx;m5stack__m5unified"]);
    }
    command.arg("build");
    let status = command.status().unwrap_or_else(|error| {
        eprintln!("failed to start {command:?}: {error}\nRun firmware-check in an activated ESP-IDF environment; see docs/DEVELOPMENT.md.");
        std::process::exit(1);
    });
    if !status.success() {
        std::process::exit(status.code().unwrap_or(1));
    }
}

fn firmware_check(args: &[String]) {
    let mut idf_path = None;
    let mut python = None;
    let mut values = args.iter();
    while let Some(option) = values.next() {
        let value = values.next().unwrap_or_else(|| usage());
        match option.as_str() {
            "--idf" => idf_path = Some(value.as_str()),
            "--python" => python = Some(value.as_str()),
            _ => usage(),
        }
    }
    if let Err(error) = idf::check(idf_path, python) {
        eprintln!("{error}");
        std::process::exit(1);
    }
}

fn finish(result: anyhow::Result<()>) {
    if let Err(error) = result {
        eprintln!("{error:#}");
        std::process::exit(1);
    }
}

fn main() {
    let mut args = env::args().skip(1);
    let root = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../..");
    match args.next().as_deref() {
        Some("tui") => finish(tui::run(&args.collect::<Vec<_>>(), &root)),
        Some("host-build") => host_build(&args.collect::<Vec<_>>(), root),
        Some("desktop-build") => desktop_build(&args.collect::<Vec<_>>(), root),
        Some("bridge-build") => bridge_build(&args.collect::<Vec<_>>(), root),
        Some("firmware-check") => firmware_check(&args.collect::<Vec<_>>()),
        Some("firmware-build") => firmware_build(&args.collect::<Vec<_>>(), root),
        Some("firmware-package") => finish(packaging::firmware(&args.collect::<Vec<_>>(), &root)),
        Some("tool-package") => finish(packaging::tool(&args.collect::<Vec<_>>(), &root)),
        Some("package-verify") => {
            let Some(path) = args.next() else { usage() };
            if args.next().is_some() {
                usage();
            }
            package_verify(&path);
        }
        Some("package-build") => package_build(&args.collect::<Vec<_>>(), &root),
        Some("release-verify") => {
            let Some(path) = args.next() else { usage() };
            if args.next().is_some() {
                usage();
            }
            release_verify(&path, &root);
        }
        Some("service-install") => service_install(&args.collect::<Vec<_>>()),
        Some("service-uninstall") => {
            if args.next().is_some() {
                usage();
            }
            service_uninstall();
        }
        _ => usage(),
    }
}
