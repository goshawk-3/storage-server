mod http_client;
mod prompt;

use clap::Parser;
use std::path::Path;
use tracing::info;
use tracing_subscriber::fmt::Subscriber;

#[derive(Parser)]
struct Config {
    /// Storage server URL
    server_url: String,
    /// The path to the folder to upload
    upload_dir: std::path::PathBuf,
}

#[tokio::main]
async fn main() {
    let args = Config::parse();
    let url = args.server_url;
    let src_folder: &Path = args.upload_dir.as_ref();

    let subscriber = Subscriber::builder()
        .with_max_level(tracing::Level::INFO)
        .with_writer(std::io::stderr);

    let subscriber = subscriber.json().flatten_event(true).finish();
    tracing::subscriber::set_global_default(subscriber)
        .expect("setting default subscriber failed");

    info!(
        "Start client with source folder: {:?}, server_url: {}",
        &src_folder, url
    );

    prompt::run_loop(url, src_folder).await;
}
