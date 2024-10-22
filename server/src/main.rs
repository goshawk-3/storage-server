mod app;

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

    let subscriber = Subscriber::builder()
        .with_max_level(tracing::Level::INFO)
        .with_writer(std::io::stderr);

    let subscriber = subscriber.json().flatten_event(true).finish();
    tracing::subscriber::set_global_default(subscriber).expect("setting default subscriber failed");

    app::run_server(&args.listen_addr).await;
}
