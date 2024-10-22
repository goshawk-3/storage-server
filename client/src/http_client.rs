use hyper::{body::HttpBody as _, Client};
use hyper::{Body, Method, Request};

use chacha20::cipher::{KeyIvInit, StreamCipher};
use chacha20::ChaCha20;
use secp256k1::{rand, Keypair, PublicKey, Secp256k1};
use sha2::{Digest, Sha256};

use std::ffi::OsString;
use std::fs;
use std::path::Path;
use tracing::{error, info};
use warp::reply::Response;

use merkle::tree as merkle;
use merkle::Hash;

#[derive(Debug)]
enum Error {
    InvalidProof,
    MissingMerkleRoot,
}
pub struct ClientApp {
    keypair: Keypair,
    bucket_id: String,

    server_url: String,
    merkle_root: Option<Hash>,
}

impl ClientApp {
    pub fn new(server_url: &str) -> Self {
        let secp = Secp256k1::new();
        let keypair = Keypair::new(&secp, &mut rand::thread_rng());
        let public_key = PublicKey::from_keypair(&keypair);

        let pubkey: String = public_key
            .serialize()
            .iter()
            .map(|b| format!("{:02x}", b))
            .collect();

        ClientApp {
            bucket_id: pubkey,
            server_url: server_url.to_owned(),
            merkle_root: None,
            keypair,
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

        info!(event = "Encrypting file", file = file_id, file_path);
        let mut data = fs::read(file_path)?;

        // encrypt the file with ChaCha20
        let key = self.keypair.secret_bytes();
        let mut cipher = ChaCha20::new(&key.into(), &[0x24; 12].into());
        cipher.apply_keystream(&mut data);

        let hash: [u8; 32] = Sha256::digest(&data).into();

        info!(event = "Uploading file", file = file_id);

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

    pub async fn upload_files(
        &mut self,
        files: &Vec<(OsString, String)>,
    ) -> Result<(), Box<dyn std::error::Error>> {
        let mut uploaded_files: Vec<Hash> = vec![];

        // Upload files to the server
        for (file, file_path) in files {
            let path: &Path = Path::new(file_path);
            let file_id = file.clone().into_string().unwrap();

            // Upload an encrypted form of the file to the storage server
            let (hash, res) =
                self.encrypt_and_upload(file_path, &file_id).await?;

            if res.status() == 200 {
                info!(event = "Uploaded file", file_id);
                uploaded_files.push(hash);

                // File successfully uploaded. Delete local file
                fs::remove_file(path)?;
                info!(event = "Deleted local copy", file_id);
            } else {
                error!(
                    event = "Failed to upload",
                    file_id,
                    status = ?res.status()
                );
            }
        }

        // Build the Merkle tree for all uploaded files
        self.merkle_root =
            merkle::Tree::build_from_leaves(uploaded_files).root_hash();

        Ok(())
    }

    /// Download and verify a file from the storage server
    ///
    /// If a valid proof is received, the file is decrypted and saved to the downloads folder
    pub async fn download_and_verify(
        &self,
        file_id: &String,
    ) -> Result<(), Box<dyn std::error::Error>> {
        let client = Client::new();

        //TODO: refactor
        // shorter function
        // error handling

        // Download the file and proof
        let mut res = client
            .get(
                format!(
                    "{}/file/{}/{}",
                    self.server_url, self.bucket_id, file_id
                )
                .parse()?,
            )
            .await?;

        let mut file_data = Vec::new();
        while let Some(chunk) = res.data().await {
            file_data.extend_from_slice(&chunk?);
        }

        if res.status() != 200 {
            error!(
                event = "Failed to download file",
                file = file_id,
                status = ?res.status()
            );
            return Ok(());
        }

        let hash: Hash = Sha256::digest(&file_data).into();
        info!(
            event = "Download file",
            file = file_id,
            hash = hex::encode(hash),
        );

        info!(event = "Request proof", file = file_id);

        // Download the proof
        let mut res = client
            .get(
                format!(
                    "{}/proof/{}/{}",
                    self.server_url, self.bucket_id, file_id
                )
                .parse()?,
            )
            .await?;

        let mut bytes = Vec::new();
        while let Some(chunk) = res.data().await {
            bytes.extend_from_slice(&chunk?);
        }

        if res.status() != 200 {
            error!(
                event = "Failed to download proof",
                file = file_id,
                status = ?res.status()
            );
            return Ok(());
        }

        let proof: Vec<([u8; 32], u8)> = bincode::deserialize(&bytes)?;

        // Verify the file with the proof
        match self.verify(proof, &hash, file_id).await {
            Ok(_) => {
                self.decrypt_and_save_file(file_id, &file_data)?;
            }
            Err(err) => {
                error!(event = "Download failed", file = file_id, err = ?err);
            }
        }

        Ok(())
    }

    /// Verify the downloaded merkle path for a file
    async fn verify(
        &self,
        proof: Vec<(Hash, u8)>,
        hash: &Hash,
        file_name: &str,
    ) -> Result<(), Error> {
        if let Some(merkle_root) = self.merkle_root {
            info!(event = "Checking proof", file = file_name);

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
        let key = self.keypair.secret_bytes();
        let mut cipher = ChaCha20::new(&key.into(), &[0x24; 12].into());
        let mut data = data.to_owned();
        cipher.apply_keystream(&mut data);

        let _ = fs::create_dir_all("./downloads");
        let path = format!("./downloads/{}", file_id);
        fs::write(Path::new(&path), data)?;
        info!(event = "Valid file saved", file = path);

        Ok(())
    }
}
