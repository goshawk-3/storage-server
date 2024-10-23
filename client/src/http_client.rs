use hyper::{body::HttpBody as _, Client};
use hyper::{Body, Method, Request, StatusCode};

use chacha20::cipher::{KeyIvInit, StreamCipher};
use chacha20::ChaCha20;
use rand::{self, RngCore};
use sha2::{Digest, Sha256};

use std::ffi::OsString;
use std::fs;
use std::path::Path;
use thiserror::Error;
use tracing::{error, info};
use warp::reply::Response;

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

    /// Encrypt and upload a file to the storage server
    ///
    /// Returns the hash of the encrypted file and the response from the server
    async fn encrypt_and_upload(
        &self,
        file_path: &String,
        file_id: &String,
    ) -> Result<(Hash, Response), Box<dyn std::error::Error>> {
        let http_client = Client::new();

        info!(event = "encrypting file", file = file_id, file_path);
        let mut data = fs::read(file_path)?;

        // encrypt the file with ChaCha20
        let mut cipher = ChaCha20::new(&CHACHA_KEY.into(), &[0x24; 12].into());
        cipher.apply_keystream(&mut data);

        let hash: [u8; 32] = Sha256::digest(&data).into();

        info!(event = "uploading a file", file = file_id);

        // Upload the file to the storage server
        let req = Request::builder()
            .method(Method::POST)
            .uri(format!(
                "{}/upload/{}/{}",
                self.server_url,
                self.bucket_id(),
                file_id
            ))
            .header("Content-Type", "application/octet-stream")
            .body(Body::from(data))?;

        Ok((hash, http_client.request(req).await?))
    }

    /// Upload a batch of files to the storage server
    pub async fn upload_files(
        &mut self,
        files: &Vec<(OsString, String)>,
    ) -> Result<(), Box<dyn std::error::Error>> {
        let mut leaves: Vec<Hash> = self.merkle_tree.leaves();

        // Upload files to the server
        for (file, file_path) in files {
            let path: &Path = Path::new(file_path);
            let file_id = file.clone().into_string().unwrap();

            // Upload an encrypted form of the file to the storage server
            let (hash, res) =
                self.encrypt_and_upload(file_path, &file_id).await?;

            let status = res.status();

            if status == 200 {
                info!(event = "uploaded file", file_id);
                leaves.push(hash);

                // File successfully uploaded. Delete local file
                fs::remove_file(path)?;
                info!(event = "local copy deleted ", file_id);
            } else {
                let body = hyper::body::to_bytes(res.into_body())
                    .await
                    .unwrap_or_default();
                let reply =
                    String::from_utf8(body.to_vec()).unwrap_or_default();

                error!(
                    event = "failed to upload",
                    file_id,
                    status = ?status,
                    reply
                );
            }
        }

        // Recalculate the Merkle tree for both old and new files
        self.merkle_tree = merkle::Tree::build_from_leaves(leaves);
        self.persist_state()?;

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
