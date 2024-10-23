use hyper::{body::HttpBody as _, Client};
use hyper::{Body, Method, Request, StatusCode};

use chacha20::cipher::{KeyIvInit, StreamCipher};
use chacha20::ChaCha20;
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
const TREE_FILE: &str = "./merkle_tree.bin";
const BUCKET_ID: &str = "my_bucket_id";
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
    bucket_id: String,

    server_url: String,
    merkle_tree: Option<merkle::Tree>,
}

impl ClientApp {
    pub fn new(server_url: &str) -> Self {
        // Load the Merkle tree from disk, if it exists
        let merkle_tree = fs::read(TREE_FILE).map_or_else(
            |_| {
                info!(event = "no merkle tree found", file = TREE_FILE);
                None
            },
            |bytes| {
                let tree: merkle::Tree = bincode::deserialize(&bytes).ok()?;
                info!(
                    event = "loaded merkle tree",
                    leaves = tree.leaves().len()
                );
                Some(tree)
            },
        );

        ClientApp {
            bucket_id: BUCKET_ID.to_owned(),
            server_url: server_url.to_owned(),
            merkle_tree,
        }
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
                self.server_url, self.bucket_id, file_id
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
        let mut leaves: Vec<Hash> = self
            .merkle_tree
            .as_ref()
            .map_or_else(Vec::new, |t| t.leaves());

        // Upload files to the server
        for (file, file_path) in files {
            let path: &Path = Path::new(file_path);
            let file_id = file.clone().into_string().unwrap();

            // Upload an encrypted form of the file to the storage server
            let (hash, res) =
                self.encrypt_and_upload(file_path, &file_id).await?;

            if res.status() == 200 {
                info!(event = "uploaded file", file_id);
                leaves.push(hash);

                // File successfully uploaded. Delete local file
                fs::remove_file(path)?;
                info!(event = "local copy deleted ", file_id);
            } else {

                

                error!(
                    event = "failed to upload",
                    file_id,
                    status = ?res.status()
                );
            }
        }

        // Recalculate the Merkle tree for both old and new files
        let count = leaves.len();
        self.merkle_tree = Some(merkle::Tree::build_from_leaves(leaves));

        info!(event = "merkle tree updated", leaves = count);

        // Save on disk
        let bytes = bincode::serialize(&self.merkle_tree)?;

        fs::write(Path::new(TREE_FILE), bytes)?;
        info!(event = "save merkle tree on-disk", file = TREE_FILE);

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
        self.decrypt_and_save_file(file_index, &file_data)?;

        Ok(())
    }

    /// Verify the downloaded merkle path for a file
    async fn verify(
        &self,
        proof: Vec<(Hash, u8)>,
        hash: &Hash,
    ) -> Result<(), Error> {
        if let Some(merkle_root) =
            self.merkle_tree.as_ref().and_then(|t| t.root_hash())
        {
            info!(
                event = "checking proof",
                hash = hex::encode(hash),
                proof_len = proof.len(),
                merkle_root = hex::encode(merkle_root)
            );

            // Verify the file with the proof
            if !merkle::Tree::verify_proof(hash, &proof, &merkle_root) {
                return Err(Error::InvalidProof);
            }

            return Ok(());
        }

        Err(Error::MissingMerkleRoot)
    }

    /// Decrypt and save the file to the downloads folder
    fn decrypt_and_save_file(
        &self,
        file_id: &String,
        data: &[u8],
    ) -> Result<(), Box<dyn std::error::Error>> {
        let mut cipher = ChaCha20::new(&CHACHA_KEY.into(), &[0x24; 12].into());
        let mut data = data.to_owned();
        cipher.apply_keystream(&mut data);

        let _ = fs::create_dir_all(LOCAL_REPO);
        let path = format!("{}/{}", LOCAL_REPO, file_id);

        fs::write(Path::new(&path), data)?;
        info!(event = "valid file saved", file = path);

        Ok(())
    }

    /// Downloads a blob/binary object from the storage server
    async fn download_blob(
        &self,
        file_index: &str,
        resource: &str,
    ) -> Result<Vec<u8>, Box<dyn std::error::Error>> {
        let client = Client::new();
        let mut res = client
            .get(
                format!(
                    "{}/{}/{}/{}",
                    self.server_url, resource, self.bucket_id, file_index
                )
                .parse()?,
            )
            .await?;

        let mut bytes = Vec::new();
        while let Some(chunk) = res.data().await {
            bytes.extend_from_slice(&chunk?);
        }

        if res.status() != 200 {
            return Err(Error::FailedDownload(
                resource.to_owned(),
                file_index.to_owned(),
                res.status(),
            )
            .into());
        }

        Ok(bytes)
    }
}
