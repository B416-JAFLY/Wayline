use std::env;
use std::time::Duration;

use wayline_proto::FilterMode;
use wayline_radio::{format_mac, RadioClient};

fn usage() -> ! {
    eprintln!(
        "usage: wayline-radio <port> [--baud BAUD] [hello|stats|time|channel N|filter awdl|mgmt|all]"
    );
    std::process::exit(2);
}

fn main() -> Result<(), Box<dyn std::error::Error>> {
    enum Command {
        Hello,
        Stats,
        Time,
        Channel(u8),
        Filter(FilterMode),
    }

    let mut args = env::args().skip(1);
    let Some(port) = args.next() else {
        usage();
    };
    let mut baud = 3_000_000;
    let mut command = Command::Hello;
    while let Some(arg) = args.next() {
        match arg.as_str() {
            "--baud" => baud = args.next().ok_or("--baud needs a value")?.parse()?,
            "hello" => command = Command::Hello,
            "stats" => command = Command::Stats,
            "time" => command = Command::Time,
            "channel" => {
                command = Command::Channel(args.next().ok_or("channel needs a value")?.parse()?)
            }
            "filter" => {
                command = Command::Filter(match args.next().as_deref() {
                    Some("awdl") => FilterMode::Awdl,
                    Some("mgmt") => FilterMode::ManagementAndData,
                    Some("all") => FilterMode::All,
                    _ => {
                        usage();
                    }
                })
            }
            _ => {
                usage();
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
        Command::Time => println!("{} us", radio.time_us(Duration::from_secs(2))?),
        Command::Channel(channel) => {
            radio.set_channel(channel, Duration::from_secs(3))?;
            println!("channel {channel}");
        }
        Command::Filter(filter) => {
            radio.set_filter(filter, Duration::from_secs(3))?;
            println!("filter set");
        }
    }
    Ok(())
}
