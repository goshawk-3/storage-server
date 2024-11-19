use merkle::tree as merkle;
use std::collections::BTreeMap;
use std::io;
use tokio::fs;

pub(crate) const UPLOADS_DIR: &str = "./buckets";

/// Represents a bucket of files uploaded by a client together with calculated
/// Merkle tree
#[derive(Default, Clone, serde::Serialize, serde::Deserialize)]
pub(crate) struct ClientBucket {
    pub bucket_id: String,

    /// Map file hash to file path
    pub files: BTreeMap<[u8; 32], String>,
    pub merkle_tree: merkle::Tree,
}

impl ClientBucket {
    pub(crate) fn new(bucket_id: String) -> Self {
        ClientBucket {
            bucket_id,
            files: BTreeMap::new(),
            merkle_tree: merkle::Tree::default(),
        }
    }

    /// Calculates the Merkle tree
    pub(crate) fn calculate_merkle_tree(&mut self) {
        let leaves: Vec<[u8; 32]> = self.files.keys().cloned().collect();
        self.merkle_tree = merkle::Tree::build_from_leaves(leaves);
    }

    pub(crate) fn get_filepath(&self, index: usize) -> Option<&String> {
        self.files.iter().nth(index).map(|(_, path)| path)
    }

    /// Creates bucket folder if it does not exist
    pub(crate) async fn get_or_create_dir(&self) -> io::Result<String> {
        let bucket_dir: String = self.get_dir();
        fs::create_dir_all(&bucket_dir).await?;
        Ok(bucket_dir)
    }

    pub(crate) fn get_dir(&self) -> String {
        format!("{}/{}", UPLOADS_DIR, self.bucket_id)
    }
}
