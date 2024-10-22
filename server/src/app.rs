use std::collections::HashMap;
use std::io;
use std::{path::Path, sync::Arc};

use sha2::{Digest, Sha256};
use std::net::SocketAddr;
use tokio::fs;
use tokio::sync::RwLock;
use tracing::{error, info};
use warp::Filter;

use merkle::tree as merkle;

const UPLOADS_DIR: &str = "./buckets";

/// Represents a bucket of files uploaded by a client together with calculated Merkle tree
#[derive(Default, Clone)]
struct ClientBucket {
    pub bucket_id: String,
    pub files: Vec<String>,
    pub merkle_tree: merkle::Tree,
}

impl ClientBucket {
    fn new(bucket_id: String) -> Self {
        ClientBucket {
            bucket_id,
            files: Vec::new(),
            merkle_tree: merkle::Tree::default(),
        }
    }

    /// Adds a file to the bucket and recalculates the Merkle tree
    async fn calculate_merkle_tree(&mut self, file: String) {
        self.files.push(file);

        let mut leaves = Vec::new();
        // Hash each file
        for file_path in &self.files {
            let data = fs::read(file_path).await.expect("valid file path");
            let hash = Sha256::digest(&data);
            leaves.push(hash.into());
        }

        self.merkle_tree = merkle::Tree::build_from_leaves(leaves);
    }

    /// Returns position, if exists
    fn file_exists(&self, file_id: &str) -> Option<usize> {
        self.files.iter().position(|file_path| {
            let path = Path::new(file_path);
            let f = path.file_name().unwrap().to_owned().into_string().unwrap();

            f == file_id
        })
    }

    /// Creates bucket folder if it does not exist
    async fn create_dir(&self) -> io::Result<String> {
        let bucket_dir: String = self.get_dir();
        fs::create_dir_all(&bucket_dir).await?;
        Ok(bucket_dir)
    }

    fn get_dir(&self) -> String {
        format!("{}/{}", UPLOADS_DIR, self.bucket_id)
    }
}

#[derive(Default, Clone)]
pub struct ServerState {
    /// Bucket id to a Bucket
    buckets: HashMap<String, ClientBucket>,
}

pub async fn run_server(addr: &str) {
    let state = Arc::new(RwLock::new(ServerState::default()));

    // File upload
    // POST /upload/:bucket_id/:file_id
    let upload = warp::path("upload")
        .and(warp::post())
        .and(warp::path::param())
        .and(warp::path::param())
        .and(warp::body::bytes())
        .and(with_state(state.clone()))
        .and_then(upload_file);

    // File request
    // GET /file/:bucket_id/:file_id
    let download = warp::path("file")
        .and(warp::get())
        .and(warp::path::param())
        .and(warp::path::param())
        .and(with_state(state.clone()))
        .and_then(download_file);

    // Proof request
    // GET /proof/:bucket_id/:file_id
    let proof = warp::path("proof")
        .and(warp::get())
        .and(warp::path::param())
        .and(warp::path::param())
        .and(with_state(state.clone()))
        .and_then(download_proof);

    let addr: SocketAddr = addr.parse().expect("expect parsable address");

    warp::serve(upload.or(download).or(proof)).run(addr).await;
}

fn with_state(
    state: Arc<RwLock<ServerState>>,
) -> impl Filter<
    Extract = (Arc<RwLock<ServerState>>,),
    Error = std::convert::Infallible,
> + Clone {
    warp::any().map(move || state.clone())
}

async fn upload_file(
    bucket_id: String,
    file_id: String,
    body: bytes::Bytes,
    state: Arc<RwLock<ServerState>>,
) -> Result<impl warp::Reply, warp::Rejection> {
    let mut guard = state.write().await;
    let bucket = guard
        .buckets
        .entry(bucket_id.clone())
        .or_insert(ClientBucket::new(bucket_id.clone()));

    let bucket_dir = bucket.create_dir().await.expect("valid bucket dir");

    info!(request = "upload", bucket_dir, file_id);

    // Check if file already exists in the bucket
    if bucket.file_exists(&file_id).is_some() {
        return Ok(warp::reply::with_status(
            "File already uploaded",
            warp::http::StatusCode::BAD_REQUEST,
        ));
    }

    // Save the file on disk
    let file_path: String = format!("{}/{}", bucket_dir, file_id);
    if let Err(err) = fs::write(file_path.clone(), body).await {
        error!(event = "Failed to write file", file_id, bucket_id, error = ?err);

        return Ok(warp::reply::with_status(
            "Failed to write file",
            warp::http::StatusCode::INTERNAL_SERVER_ERROR,
        ));
    }

    bucket.calculate_merkle_tree(file_path.clone()).await;

    info!(
        event = "file uploaded",
        file_path,
        bucket_id,
        file_id,
        leaves_count = bucket.merkle_tree.leaves_count()
    );

    Ok(warp::reply::with_status(
        "File uploaded",
        warp::http::StatusCode::OK,
    ))
}

async fn download_file(
    bucket_id: String,
    file_id: String,
    state: Arc<RwLock<ServerState>>,
) -> Result<impl warp::Reply, warp::Rejection> {
    let guard = state.read().await;

    info!(request = "download_file", bucket_id, file_id);

    // Get bucket by id
    let bucket = guard
        .buckets
        .get(&bucket_id)
        .ok_or(warp::reject::not_found())?;

    if bucket.file_exists(&file_id).is_none() {
        return Ok(warp::reply::with_status(
            vec![],
            warp::http::StatusCode::BAD_REQUEST,
        ));
    }

    let path = format!("{}/{}", bucket.get_dir(), file_id);
    let data = fs::read(&path).await.unwrap();

    info!(event = "file downloaded", file_id, path);
    Ok(warp::reply::with_status(data, warp::http::StatusCode::OK))
}

async fn download_proof(
    bucket_id: String,
    file_id: String,
    state: Arc<RwLock<ServerState>>,
) -> Result<impl warp::Reply, warp::Rejection> {
    let guard = state.read().await;

    info!(request = "download_proof", bucket_id, file_id);

    // Get bucket by id
    let bucket = guard
        .buckets
        .get(&bucket_id)
        .ok_or(warp::reject::not_found())?;

    if let Some(index) = bucket.file_exists(&file_id) {
        // Generate merkle path for the file

        let proof: Vec<([u8; 32], u8)> = bucket.merkle_tree.get_proof(index);

        let proof_bytes =
            bincode::serialize(&proof).expect("valid proof serialization");

        info!(event = "proof downloaded", file_id, index);

        return Ok(warp::reply::with_status(
            proof_bytes,
            warp::http::StatusCode::OK,
        ));
    }

    Ok(warp::reply::with_status(
        vec![],
        warp::http::StatusCode::BAD_REQUEST,
    ))
}
