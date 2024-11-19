mod app;
mod client_bucket;
mod database;

use clap::Parser;
use tracing_subscriber::fmt::Subscriber;

#[derive(Parser)]
struct Config {
    /// Storage server URL
    listen_addr: String,
}

#[tokio::main]
async fn main() {
    let args = Config::parse();

    let s = Subscriber::builder()
        .with_max_level(tracing::Level::INFO)
        .with_writer(std::io::stderr);

    tracing::subscriber::set_global_default(
        s.json().flatten_event(true).finish(),
    )
    .expect("valid default subscriber");

    app::run_server(&args.listen_addr).await;
}
