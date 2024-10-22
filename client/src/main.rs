mod http_client;

use http_client::ClientApp;
use std::{ffi::OsString, fs};

use clap::Parser;
use tracing::{error, info};
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
    let src_folder = args.upload_dir;

    let mut client = ClientApp::new(url.as_str());

    let subscriber = Subscriber::builder()
        .with_max_level(tracing::Level::INFO)
        .with_writer(std::io::stderr);

    let subscriber = subscriber.json().flatten_event(true).finish();
    tracing::subscriber::set_global_default(subscriber)
        .expect("setting default subscriber failed");

    info!(
        "Start client with source folder: {:?}, server_url: {}",
        src_folder, url
    );

    // read all files from folder
    let files: Vec<(OsString, String)> = fs::read_dir(src_folder)
        .expect("empty folder")
        .filter_map(|entry| {
            entry.ok().and_then(|e| {
                if e.file_type().ok()?.is_file() {
                    e.path().to_str().map(|s| (e.file_name(), s.to_string()))
                } else {
                    None
                }
            })
        })
        .collect();

    // Upload files to server
    if let Err(err) = client.upload_files(&files).await {
        error!("Error uploading: {:?}", err);
        return;
    }

    // Download and verify each of the uploaded file
    for (file_index, _) in files.iter().enumerate() {
        if let Err(err) =
            client.download_and_verify(&file_index.to_string()).await
        {
            error!("Error downloading file: {:?}", err);
        }
    }
}
