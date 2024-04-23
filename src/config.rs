use clap::Parser;

#[derive(Parser, Debug)]
#[clap(version = "1.0", author = "Jon Bennett <jon.bennett12@gmail.com>")]
pub struct Config {
    #[clap(short, long)]
    pub run: bool,
    #[clap(short, long)]
    pub event_id: Option<i64>,
}

impl Config {
    pub fn new() -> Config {
        Config::parse()
    }
}

impl Default for Config {
    fn default() -> Self {
        Self::new()
    }
}
