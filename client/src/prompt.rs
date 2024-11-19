// Prompt module for the client

use crate::http_client::{ClientApp, LOCAL_REPO};
use requestty::Question;
use std::{ffi::OsString, fs, io, path::Path};

use tracing::error;

pub(crate) enum Commands {
    BucketID,
    ListFiles,
    UploadAll,
    DownloadFile(usize),
    ListDownloadedFiles,
    Exit,
}

fn prompt() -> requestty::Result<Commands> {
    let answer = requestty::prompt_one(
        Question::select("command")
            .message("Client")
            .choice("My Bucket ID")
            .choice("List available files")
            .choice("Upload all files")
            .choice("Download file by index")
            .choice("List downloaded files")
            .choice("Exit")
            .build(),
    )?;

    match answer.as_list_item().unwrap().index {
        0 => Ok(Commands::BucketID),
        1 => Ok(Commands::ListFiles),
        2 => Ok(Commands::UploadAll),
        3 => {
            // Ask for the file index after selecting "Download file by index"
            let index_question = Question::int("index")
                .message("Enter the file index to download")
                .validate(|index, _| {
                    if index >= 0 {
                        Ok(())
                    } else {
                        Err("Index must be a non-negative number".into())
                    }
                })
                .build();

            let index_answer = requestty::prompt_one(index_question)?;

            if let Some(index) = index_answer.as_int() {
                Ok(Commands::DownloadFile(index as usize))
            } else {
                Err(io::Error::new(
                    io::ErrorKind::InvalidInput,
                    "Invalid index",
                )
                .into())
            }
        }
        4 => Ok(Commands::ListDownloadedFiles),
        5 => Ok(Commands::Exit),
        _ => unreachable!(),
    }
}

pub(crate) async fn run_loop(
    server_url: String,
    src_folder: &Path,
    client_dir: &str,
) {
    let mut client = ClientApp::new(server_url.as_str(), client_dir);

    loop {
        match prompt().unwrap() {
            // List all files in the SRC folder
            Commands::BucketID => {
                println!("My bucket_id: {}", client.bucket_id());
            }

            // List all files in the SRC folder
            Commands::ListFiles => {
                let files = read_files(src_folder);
                for (index, file) in files.iter().enumerate() {
                    println!("{}: {:?}", index, file.0);
                }
            }
            // Upload all files from SRC folder to the server
            Commands::UploadAll => {
                let files = read_files(src_folder);
                if let Err(err) = client.upload_files(&files).await {
                    error!("Error uploading: {:?}", err);
                }
            }
            // Download a file by index
            Commands::DownloadFile(file_index) => {
                if let Err(err) =
                    client.download_and_verify(&file_index.to_string()).await
                {
                    error!("Error downloading file: {:?}", err);
                }
            }
            // List all files in the download folder
            Commands::ListDownloadedFiles => {
                let local_repo = client_dir.to_owned() + LOCAL_REPO;
                let files = read_files(&local_repo);
                for (_, file) in files.iter() {
                    println!("downloaded file: {}", file);
                }
            }

            Commands::Exit => {
                break;
            }
        }
    }
}

fn read_files<P: AsRef<Path>>(src_folder: P) -> Vec<(OsString, String)> {
    if let Ok(dir) = fs::read_dir(src_folder) {
        dir.filter_map(|entry| {
            entry.ok().and_then(|e| {
                if e.file_type().ok()?.is_file() {
                    e.path().to_str().map(|s| (e.file_name(), s.to_string()))
                } else {
                    None
                }
            })
        })
        .collect()
    } else {
        Vec::new()
    }
}
