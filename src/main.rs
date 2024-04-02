use rating_generator::run;

#[tokio::main]
async fn main() {
    if let Err(err) = run().await {
        eprintln!("ERROR: {}", err);
        err.chain()
            .skip(1)
            .for_each(|cause| eprintln!("because: {}", cause));
        std::process::exit(1);
    }
}
