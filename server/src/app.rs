use std::collections::{BTreeMap, HashMap};
use std::io;
use std::sync::Arc;

use sha2::{Digest, Sha256};
use std::net::SocketAddr;
use tokio::fs;
use tokio::sync::RwLock;
use tracing::{error, info};
use warp::Filter;

use merkle::tree as merkle;

const UPLOADS_DIR: &str = "./buckets";

/// Represents a bucket of files uploaded by a client together with calculated
/// Merkle tree
#[derive(Default, Clone)]
struct ClientBucket {
    pub bucket_id: String,

    /// Map file hash to file path
    pub files: BTreeMap<[u8; 32], String>,
    pub merkle_tree: merkle::Tree,
}

impl ClientBucket {
    fn new(bucket_id: String) -> Self {
        ClientBucket {
            bucket_id,
            files: BTreeMap::new(),
            merkle_tree: merkle::Tree::default(),
        }
    }

    /// Calculates the Merkle tree
    async fn calculate_merkle_tree(&mut self) {
        let leaves: Vec<[u8; 32]> = self.files.keys().cloned().collect();
        self.merkle_tree = merkle::Tree::build_from_leaves(leaves);
    }

    fn get_filepath(&self, index: usize) -> Option<&String> {
        self.files.iter().nth(index).map(|(_, path)| path)
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
    /// Map a Bucket id to a (MerkleTree, files) pair
    buckets: HashMap<String, ClientBucket>,
}

pub async fn run_server(addr: &str) {
    let state = Arc::new(RwLock::new(ServerState::default()));

    // File upload_file
    // POST /upload/:bucket_id/:file_id
    let upload = warp::path("upload_file")
        .and(warp::post())
        .and(warp::path::param())
        .and(warp::path::param())
        .and(warp::body::bytes())
        .and(with_state(state.clone()))
        .and_then(handle_upload_file);

    // File complete_upload
    // POST /upload/:bucket_id/
    let complete_upload = warp::path("complete_upload")
        .and(warp::post())
        .and(warp::path::param())
        .and(with_state(state.clone()))
        .and_then(handle_complete_upload);

    // File request
    // GET /file/:bucket_id/:file_id
    let download = warp::path("file")
        .and(warp::get())
        .and(warp::path::param())
        .and(warp::path::param())
        .and(with_state(state.clone()))
        .and_then(handle_download_file);

    // Proof request
    // GET /proof/:bucket_id/:file_id
    let proof = warp::path("proof")
        .and(warp::get())
        .and(warp::path::param())
        .and(warp::path::param())
        .and(with_state(state.clone()))
        .and_then(handle_download_proof);

    let addr: SocketAddr = addr.parse().expect("parsable address");

    warp::serve(upload.or(complete_upload).or(download).or(proof))
        .run(addr)
        .await;
}

fn with_state(
    state: Arc<RwLock<ServerState>>,
) -> impl Filter<
    Extract = (Arc<RwLock<ServerState>>,),
    Error = std::convert::Infallible,
> + Clone {
    warp::any().map(move || state.clone())
}

/// Handles handle_complete_upload request
async fn handle_complete_upload(
    bucket_id: String,
    state: Arc<RwLock<ServerState>>,
) -> Result<impl warp::Reply, warp::Rejection> {
    let mut guard = state.write().await;
    let bucket = guard
        .buckets
        .entry(bucket_id.clone())
        .or_insert(ClientBucket::new(bucket_id.clone()));

    let bucket_dir = bucket.create_dir().await.expect("valid bucket dir");
    info!(request = "complete upload", bucket_dir);

    bucket.calculate_merkle_tree().await;

    if let Some(root) = bucket.merkle_tree.root_hash() {
        let root_hex = hex::encode(root);
        info!(event = "complete upload", bucket_id, root = root_hex);
    }

    Ok(warp::reply::with_status(
        "File upload completed",
        warp::http::StatusCode::OK,
    ))
}

/// Handles file upload request
///
/// Duplicated files per a bucket are not allowed
async fn handle_upload_file(
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

    let file_hash = Sha256::digest(&body).into();

    // Check if file already exists in the bucket
    if bucket.files.contains_key(&file_hash) {
        let reply = "file already uploaded";
        error!(event = "failed to upload", file_id, bucket_id, reply);

        return Ok(warp::reply::with_status(
            reply,
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

    bucket.files.insert(file_hash, file_path.clone());

    info!(event = "file uploaded", file_path, bucket_id, file_id,);

    Ok(warp::reply::with_status(
        "File uploaded",
        warp::http::StatusCode::OK,
    ))
}

/// Handles file download request
///
/// Returns `404 Not Found` if the (bucket_id-file_index) does not exist
async fn handle_download_file(
    bucket_id: String,
    file_index: String,
    state: Arc<RwLock<ServerState>>,
) -> Result<impl warp::Reply, warp::Rejection> {
    let guard = state.read().await;

    info!(request = "download_file", bucket_id, file_index);

    // Get bucket by id
    let bucket = guard
        .buckets
        .get(&bucket_id)
        .ok_or(warp::reject::not_found())?;

    let index = file_index
        .parse::<usize>()
        .map_err(|_| warp::reject::not_found())?;

    let file_path = bucket
        .get_filepath(index)
        .ok_or(warp::reject::not_found())?;

    let data = fs::read(file_path)
        .await
        .map_err(|_| warp::reject::not_found())?;

    info!(event = "file downloaded", file_path);
    Ok(warp::reply::with_status(data, warp::http::StatusCode::OK))
}

/// Handles proof download request
///
/// Returns `404 Not Found` if the (bucket_id-file_index) does not exist
async fn handle_download_proof(
    bucket_id: String,
    file_index: String,
    state: Arc<RwLock<ServerState>>,
) -> Result<impl warp::Reply, warp::Rejection> {
    let guard = state.read().await;

    info!(request = "download_proof", bucket_id, file_index);

    // Get bucket by id
    let bucket = guard
        .buckets
        .get(&bucket_id)
        .ok_or(warp::reject::not_found())?;

    let index = file_index
        .parse::<usize>()
        .map_err(|_| warp::reject::not_found())?;

    let file_path = bucket
        .get_filepath(index)
        .ok_or(warp::reject::not_found())?;

    // Generate merkle path for the file
    let proof: Vec<([u8; 32], u8)> = bucket.merkle_tree.get_proof(index);
    let proof_bytes =
        bincode::serialize(&proof).expect("valid proof serialization");

    info!(event = "proof downloaded", file_path, index);

    Ok(warp::reply::with_status(
        proof_bytes,
        warp::http::StatusCode::OK,
    ))
}
