use hyper::{body::HttpBody as _, Client};
use hyper::{Body, Method, Request, StatusCode};

use chacha20::cipher::{KeyIvInit, StreamCipher};
use chacha20::ChaCha20;
use rand::{self, RngCore};
use sha2::{Digest, Sha256};
use tokio::sync::Mutex;

use std::ffi::OsString;
use std::fs;
use std::path::Path;
use std::sync::Arc;
use thiserror::Error;
use tokio::task::JoinSet;
use tracing::{error, info};

use merkle::tree as merkle;
use merkle::Hash;

pub(crate) const LOCAL_REPO: &str = "./local_repo";
const STATE_FILE: &str = "state_file.bin";
const CHACHA_KEY: [u8; 32] = [0x24; 32];

#[derive(Debug, Error)]
enum Error {
    #[error("invalid proof")]
    InvalidProof,
    #[error("client is missing the Merkle root")]
    MissingMerkleRoot,
    #[error("failed to download resource {0}: index: {1} status: {2}")]
    FailedDownload(String, String, StatusCode),
    #[error("failed to upload filename: {0}")]
    FailUpload(String),
}

pub struct ClientApp {
    server_url: String,

    bucket_id: [u8; 32],
    merkle_tree: merkle::Tree,
}

impl ClientApp {
    pub fn new(server_url: &str) -> Self {
        // Load state from disk
        let (bucket_id, merkle_tree) = Self::read_from_file();

        ClientApp {
            bucket_id,
            server_url: server_url.to_owned(),
            merkle_tree,
        }
    }

    /// Loads both bucket_id and the Merkle tree from disk, if STATE_FILE exists
    ///
    /// If state file is not found then a new bucket id is generated
    pub fn read_from_file() -> ([u8; 32], merkle::Tree) {
        fs::read(STATE_FILE).map_or_else(
            |_| {
                info!(event = "no state found", file = STATE_FILE);
                let mut bucket_id = [0u8; 32];
                rand::thread_rng().fill_bytes(&mut bucket_id[..]);
                info!(
                    event = "new bucket id",
                    bucket_id = hex::encode(bucket_id)
                );

                (bucket_id, merkle::Tree::default())
            },
            |bytes| {
                let s: State =
                    bincode::deserialize(&bytes).expect("valid state file");

                info!(
                    event = "loaded state from disk",
                    leaves = s.merkle_tree.leaves().len(),
                    bucket_id = hex::encode(s.bucket_id)
                );

                (s.bucket_id, s.merkle_tree)
            },
        )
    }

    /// Persist the current state to disk
    pub fn persist_state(&self) -> Result<(), Box<dyn std::error::Error>> {
        fs::write(
            STATE_FILE,
            bincode::serialize(&State {
                merkle_tree: self.merkle_tree.clone(),
                bucket_id: self.bucket_id,
            })?,
        )?;
        info!(event = "state saved on disk", file = STATE_FILE);
        Ok(())
    }

    /// Upload a batch of files to the storage server
    pub async fn upload_files(
        &mut self,
        files: &Vec<(OsString, String)>,
    ) -> Result<(), Box<dyn std::error::Error>> {
        let leaves = Arc::new(Mutex::new(self.merkle_tree.leaves()));

        // Async upload of all files to the server
        let mut async_clients = JoinSet::new();

        for (file, file_path) in files {
            let file_name = file.to_string_lossy().to_string();
            let leaves = Arc::clone(&leaves);
            let url = self.server_url.clone();
            let bucket_id = self.bucket_id();
            let file_path = file_path.clone();

            // Spawn a new task per a file upload
            async_clients.spawn(async move {
                match Self::encrypt_and_upload(
                    &url,
                    &bucket_id,
                    file_name.clone(),
                    &file_path,
                )
                .await
                {
                    Ok(hash) => {
                        info!(event = "file uploaded", file_name);
                        leaves.lock().await.push(hash);

                        // Remove the file from the local repo
                        fs::remove_file(file_path).expect("file removed");
                    }
                    Err(err) => {
                        error!(
                            event = "failed to upload file",
                            file_name,
                            ?err
                        );
                    }
                }
            });
        }

        // Wait for all the uploaders to finish
        async_clients.join_all().await;

        // Instruct the server to close the upload session
        self.close_upload().await;

        // Recalculate the Merkle trees
        self.merkle_tree =
            merkle::Tree::build_from_leaves(leaves.lock().await.clone());
        self.persist_state()?;

        if let Some(root_hex) = self.merkle_tree.root_hash() {
            info!(
                event = "completed upload",
                bucket_id = self.bucket_id(),
                root = hex::encode(root_hex)
            );
        }

        Ok(())
    }

    /// Download and verify a file from the storage server
    ///
    /// If a valid proof is received, the file is decrypted and saved to the
    /// downloads folder
    pub async fn download_and_verify(
        &self,
        file_index: &String,
    ) -> Result<(), Box<dyn std::error::Error>> {
        // Download the file
        let file_data = self.download_blob(file_index, "file").await?;
        let hash: Hash = Sha256::digest(&file_data).into();
        info!(
            event = "file data received",
            file_index,
            hash = hex::encode(hash),
        );

        // Download the proof
        info!(event = "request proof", file_index);
        let bytes = self.download_blob(file_index, "proof").await?;

        let proof: Vec<([u8; 32], u8)> = bincode::deserialize(&bytes)?;

        // Verify the file with the proof
        self.verify(proof, &hash).await?;
        self.decrypt_and_save_file(&hash, &file_data)?;

        Ok(())
    }

    /// Verify the provided merkle path for a file
    async fn verify(
        &self,
        proof: Vec<(Hash, u8)>,
        hash: &Hash,
    ) -> Result<(), Error> {
        if let Some(merkle_root) = self.merkle_tree.root_hash() {
            info!(
                event = "checking proof",
                hash = hex::encode(hash),
                proof_len = proof.len(),
                merkle_root = hex::encode(merkle_root)
            );

            if !merkle::Tree::verify_proof(hash, &proof, &merkle_root) {
                return Err(Error::InvalidProof);
            }

            return Ok(());
        }

        Err(Error::MissingMerkleRoot)
    }

    /// Decrypt and save the file to the downloads folder
    /// File is named after the hash of the content
    fn decrypt_and_save_file(
        &self,
        file_id: &[u8],
        data: &[u8],
    ) -> Result<(), Box<dyn std::error::Error>> {
        let mut cipher = ChaCha20::new(&CHACHA_KEY.into(), &[0x24; 12].into());
        let mut data = data.to_owned();
        cipher.apply_keystream(&mut data);

        let _ = fs::create_dir_all(LOCAL_REPO);
        let path = format!("{}/{}", LOCAL_REPO, hex::encode(file_id));

        fs::write(Path::new(&path), data)?;
        info!(event = "valid file saved", file = path);

        Ok(())
    }

    /// Encrypt and upload a file to the storage server
    ///
    /// Returns the hash of the encrypted file on successful upload
    async fn encrypt_and_upload(
        url: &str,
        bucket_id: &str,
        file_name: String,
        file_path: &String,
    ) -> Result<Hash, Error> {
        info!(event = "encrypting file", file_name, file_path);
        let mut data = fs::read(file_path).expect("valid file path");

        // encrypt the file with ChaCha20
        let mut cipher = ChaCha20::new(&CHACHA_KEY.into(), &[0x24; 12].into());
        cipher.apply_keystream(&mut data);

        let hash: [u8; 32] = Sha256::digest(&data).into();
        info!(event = "uploading a file", file_name);

        // Upload the file to the storage server
        let req = Request::builder()
            .method(Method::POST)
            .uri(format!("{}/upload_file/{}/{}", url, bucket_id, file_name))
            .header("Content-Type", "application/octet-stream")
            .body(Body::from(data))
            .expect("TODO");

        let http_client = Client::new();
        let res = http_client.request(req).await.expect("valid request");

        if res.status() != StatusCode::OK {
            Err(Error::FailUpload(file_name))
        } else {
            info!(event = "file uploaded", file_name);
            Ok(hash)
        }
    }

    /// Terminates the upload session on the server
    async fn close_upload(&self) {
        let http_client = Client::new();
        if let Ok(req) = Request::builder()
            .method(Method::POST)
            .uri(format!(
                "{}/complete_upload/{}",
                self.server_url,
                self.bucket_id()
            ))
            .header("Content-Type", "application/octet-stream")
            .body(Body::empty())
        {
            let res = http_client.request(req).await.expect("response");

            if res.status() != StatusCode::OK {
                error!(event = "failed to close upload file");
            } else {
                info!(event = "file uploaded CLOSESD");
            };
        };
    }

    /// Downloads a blob/binary object from the storage server
    async fn download_blob(
        &self,
        file_index: &str,
        resource_type: &str,
    ) -> Result<Vec<u8>, Box<dyn std::error::Error>> {
        let uri = format!(
            "{}/{}/{}/{}",
            self.server_url,
            resource_type,
            self.bucket_id(),
            file_index
        );

        let client = Client::new();
        let mut res = client.get(uri.parse()?).await?;

        let mut bytes = Vec::new();
        while let Some(chunk) = res.data().await {
            bytes.extend_from_slice(&chunk?);
        }

        if res.status() != hyper::StatusCode::OK {
            return Err(Error::FailedDownload(
                resource_type.to_owned(),
                file_index.to_owned(),
                res.status(),
            )
            .into());
        }

        Ok(bytes)
    }

    fn bucket_id(&self) -> String {
        hex::encode(self.bucket_id)
    }
}

#[derive(serde::Serialize, serde::Deserialize)]
struct State {
    merkle_tree: merkle::Tree,
    bucket_id: [u8; 32],
}
