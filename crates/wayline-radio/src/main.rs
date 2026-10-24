use std::env;
use std::time::Duration;

use wayline_radio::{format_mac, RadioClient};

fn usage() {
    eprintln!("usage: wayline-radio <COM port> [--baud BAUD] [hello|stats]");
}

fn main() -> Result<(), Box<dyn std::error::Error>> {
    enum Command {
        Hello,
        Stats,
    }

    let mut args = env::args().skip(1);
    let Some(port) = args.next() else {
        usage();
        std::process::exit(2);
    };
    let mut baud = 3_000_000;
    let mut command = Command::Hello;
    while let Some(arg) = args.next() {
        match arg.as_str() {
            "--baud" => baud = args.next().ok_or("--baud needs a value")?.parse()?,
            "hello" => command = Command::Hello,
            "stats" => command = Command::Stats,
            _ => {
                usage();
                std::process::exit(2);
            }
        }
    }

    let mut radio = RadioClient::open_serial(&port, baud, Duration::from_millis(500))?;
    match command {
        Command::Hello => {
            let hello = radio.hello(Duration::from_secs(2))?;
            println!(
                "{} mac {} proto {} IDF {}",
                hello.target,
                format_mac(hello.mac),
                hello.protocol,
                hello.idf
            );
        }
        Command::Stats => println!("{:?}", radio.stats(Duration::from_secs(2))?),
    }
    Ok(())
}
