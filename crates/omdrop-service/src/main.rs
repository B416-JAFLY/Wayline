#[cfg(windows)]
fn main() -> Result<(), Box<dyn std::error::Error>> {
    if std::env::args_os().skip(1).any(|arg| arg == "--version") {
        println!("omdrop-service {}", env!("CARGO_PKG_VERSION"));
        return Ok(());
    }
    use omdrop_service::service::{self, Options};
    let Some(options) = Options::parse(std::env::args_os().skip(1))? else {
        println!("omdrop-service [--console | --service] [--on-demand | --resident] [--data-dir PATH] [--pipe NAME]\n\nDefault: resident foreground console, %ProgramData%\\omdrop-owl, pipe omdrop-owl.\n--service is for the Windows Service Control Manager (OmdropOwl).\nCtrl+C stops the console service and its OWL child gracefully.");
        return Ok(());
    };
    if options.scm {
        service::dispatch()?;
        return Ok(());
    }
    service::runtime()?.block_on(async {
        let (shutdown, _) = tokio::sync::watch::channel(false);
        let stop = shutdown.clone();
        let signal = tokio::spawn(async move {
            if tokio::signal::ctrl_c().await.is_ok() {
                stop.send_replace(true);
            }
        });
        let result = service::run(options.clone(), shutdown, || Ok(())).await;
        signal.abort();
        let _ = signal.await;
        if let Err(e) = &result {
            service::log(&options, format!("service failed: {e}"));
        }
        result
    })?;
    Ok(())
}

#[cfg(not(windows))]
fn main() {
    eprintln!("omdrop-service requires Windows");
    std::process::exit(1);
}
