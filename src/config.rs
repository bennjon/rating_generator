use clap::Parser;

#[derive(Parser, Debug)]
#[clap(version = "1.0", author = "Jon Bennett <jon.bennett12@gmail.com>")]
pub struct Config {
    #[clap(short, long)]
    pub run: bool,
    #[clap(short, long)]
    pub event_id: Option<i64>,
    #[clap(long)]
    pub refresh: bool,
}

impl Config {
    pub fn new() -> Config {
        let args = Config::parse();

        if args.event_id.is_some() && args.run {
            panic!("Event id and run are mutually exclusive");
        }
        if args.event_id.is_some() && args.refresh {
            panic!("Event id and refresh are mutually exclusive");
        }
        if args.run && args.refresh {
            panic!("Run and refresh are mutually exclusive");
        }
        args
    }
}

impl Default for Config {
    fn default() -> Self {
        Self::new()
    }
}
