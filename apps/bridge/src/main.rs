#[cfg(windows)]
mod windows;

fn main() -> std::process::ExitCode {
    if std::env::args_os().skip(1).any(|a| a == "--version") {
        println!("omdrop-bridge {}", env!("CARGO_PKG_VERSION"));
        return std::process::ExitCode::SUCCESS;
    }
    #[cfg(windows)]
    match windows::main() {
        Ok(()) => std::process::ExitCode::SUCCESS,
        Err(e) => {
            eprintln!("omdrop-bridge: {e}");
            std::process::ExitCode::FAILURE
        }
    }
    #[cfg(not(windows))]
    {
        eprintln!("omdrop-bridge requires Windows");
        std::process::ExitCode::FAILURE
    }
}
