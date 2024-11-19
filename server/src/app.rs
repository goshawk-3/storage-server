use std::collections::HashMap;

use std::sync::Arc;

use sha2::{Digest, Sha256};
use std::net::SocketAddr;
use tokio::fs;
use tokio::sync::RwLock;
use tracing::{error, info};
use warp::Filter;

use crate::{client_bucket::ClientBucket, database::DB};

#[derive(Clone)]
pub struct ServerState {
    /// Map a Bucket id to a (MerkleTree, files) pair
    buckets: HashMap<String, Arc<RwLock<ClientBucket>>>,
    db: Arc<RwLock<DB>>,
}

impl ServerState {
    fn load_buckets_from_db() -> Self {
        //  Load buckets from the database
        let db = DB::create_or_open("./db");
        let buckets = db.read_all_buckets().expect("bucket is persisted");

        let buckets = buckets
            .into_iter()
            .map(|(bucket_id, mut bucket)| {
                info!(
                    event = "load bucket from db",
                    bucket_id,
                    files_count = bucket.files.len()
                );
                bucket.calculate_merkle_tree();
                (bucket_id, Arc::new(RwLock::new(bucket)))
            })
            .collect();

        ServerState {
            buckets,
            db: Arc::new(RwLock::new(db)),
        }
    }

    /// Persists the bucket to the database
    async fn persist_bucket_lockless(
        &self,
        bucket: &ClientBucket,
    ) -> Result<(), String> {
        let db_handle = self.db.read().await;
        db_handle.update_bucket(bucket)?;
        db_handle.flush()
    }
}

pub async fn run_server(addr: &str) {
    let state = Arc::new(RwLock::new(ServerState::load_buckets_from_db()));

    // File upload_file
    // POST /upload/:bucket_id/:filename
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
    // GET /file/:bucket_id/:file_index
    let download = warp::path("file")
        .and(warp::get())
        .and(warp::path::param())
        .and(warp::path::param())
        .and(with_state(state.clone()))
        .and_then(handle_download_file);

    // Proof request
    // GET /proof/:bucket_id/:file_index
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
///
/// Completes a async-upload of bucket of files by calculating the Merkle tree
async fn handle_complete_upload(
    bucket_id: String,
    state: Arc<RwLock<ServerState>>,
) -> Result<impl warp::Reply, warp::Rejection> {
    let bucket: Arc<RwLock<ClientBucket>> =
        get_or_create_bucket(bucket_id.clone(), state.clone()).await;

    let mut bucket = bucket.write().await;

    let bucket_dir =
        bucket.get_or_create_dir().await.expect("valid bucket dir");
    info!(request = "complete upload", bucket_dir);

    bucket.calculate_merkle_tree();

    if let Some(root) = bucket.merkle_tree.root_hash() {
        let root_hex = hex::encode(root);
        info!(event = "complete upload", bucket_id, root = root_hex);
    }

    info!(event = "persist new bucket state");
    state
        .read()
        .await
        .persist_bucket_lockless(&bucket)
        .await
        .expect("bucket is persisted");

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
    filename: String,
    body: bytes::Bytes,
    state: Arc<RwLock<ServerState>>,
) -> Result<impl warp::Reply, warp::Rejection> {
    let bucket: Arc<RwLock<ClientBucket>> =
        get_or_create_bucket(bucket_id.clone(), state.clone()).await;

    let mut bucket = bucket.write().await;

    let bucket_dir =
        bucket.get_or_create_dir().await.expect("valid bucket dir");

    info!(request = "upload", bucket_dir, filename);

    let file_hash = Sha256::digest(&body).into();

    // Check if file already exists in the bucket
    if bucket.files.contains_key(&file_hash) {
        let reply = "file already uploaded";
        error!(event = "failed to upload", filename, bucket_id, reply);

        return Ok(warp::reply::with_status(
            reply,
            warp::http::StatusCode::BAD_REQUEST,
        ));
    }

    // Save the file on disk
    let file_path: String = format!("{}/{}", bucket_dir, filename);
    if let Err(err) = fs::write(file_path.clone(), body).await {
        error!(event = "Failed to write file", filename, bucket_id, error = ?err);

        return Ok(warp::reply::with_status(
            "Failed to write file",
            warp::http::StatusCode::INTERNAL_SERVER_ERROR,
        ));
    }

    bucket.files.insert(file_hash, file_path.clone());

    info!(event = "file uploaded", file_path, bucket_id, filename);

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
    let bucket: Arc<RwLock<ClientBucket>> =
        get_bucket(bucket_id.clone(), state.clone())
            .await
            .ok_or(warp::reject::not_found())?;

    let bucket = bucket.read().await;

    info!(request = "download_file", bucket_id, file_index);

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
    let bucket: Arc<RwLock<ClientBucket>> =
        get_bucket(bucket_id.clone(), state.clone())
            .await
            .ok_or(warp::reject::not_found())?;

    let bucket = bucket.read().await;

    info!(request = "download_proof", bucket_id, file_index);

    let index = file_index
        .parse::<usize>()
        .map_err(|_| warp::reject::not_found())?;

    let file_path = bucket
        .get_filepath(index)
        .ok_or(warp::reject::not_found())?;

    // Generate merkle path for the file
    //
    let proof: Vec<([u8; 32], u8)> = bucket.merkle_tree.get_proof(index);
    let proof_bytes =
        bincode::serialize(&proof).expect("valid proof serialization");

    info!(event = "proof downloaded", file_path, index);

    Ok(warp::reply::with_status(
        proof_bytes,
        warp::http::StatusCode::OK,
    ))
}

/// Returns an existing bucket or creates a new one
///
/// This function tries to get a bucket from the state.
/// If the bucket does not exist, it locks the entire state and creates a new bucket.
async fn get_or_create_bucket(
    bucket_id: String,
    state: Arc<RwLock<ServerState>>,
) -> Arc<RwLock<ClientBucket>> {
    let res = get_bucket(bucket_id.clone(), state.clone()).await;

    match res {
        Some(bucket) => bucket,
        None => {
            // Lock the entire state and create a new bucket
            let mut state_guard = state.write().await;

            state_guard
                .buckets
                .entry(bucket_id.clone())
                .or_insert(Arc::new(RwLock::new(ClientBucket::new(bucket_id))))
                .clone()
        }
    }
}

/// Returns an existing bucket or none
/// This function acquires only a read lock on the state
async fn get_bucket(
    bucket_id: String,
    state: Arc<RwLock<ServerState>>,
) -> Option<Arc<RwLock<ClientBucket>>> {
    let state_guard = state.read().await;

    // Get bucket by id
    state_guard.buckets.get(&bucket_id).cloned()
}
