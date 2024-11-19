use std::{collections::HashMap, path::Path};

use crate::client_bucket::ClientBucket;

use rocksdb::{
    OptimisticTransactionDB, OptimisticTransactionOptions, WriteOptions,
};
use tracing::info;

pub(crate) struct DB {
    backend: OptimisticTransactionDB,
}

impl DB {
    /// Creates a new database or opens an existing one
    pub(crate) fn create_or_open<T>(path: T) -> Self
    where
        T: AsRef<Path>,
    {
        let path = path.as_ref().join("rocksdb");
        info!("Open database in {path:?}");

        let backend = OptimisticTransactionDB::open_default(path)
            .expect("should be a valid database in {path}");

        Self { backend }
    }

    /// Updates the bucket in the database
    pub(crate) fn update_bucket(
        &self,
        bucket: &ClientBucket,
    ) -> Result<(), String> {
        let key = bucket.bucket_id.as_bytes();
        let value = bincode::serialize(bucket).unwrap();

        // Create a new RocksDB transaction
        let write_options = WriteOptions::default();
        let tx_options = OptimisticTransactionOptions::default();
        let inner = self.backend.transaction_opt(&write_options, &tx_options);
        inner.put(key, value)?;
        inner.commit()?;

        Ok(())
    }

    /// Flushes the database
    pub(crate) fn flush(&self) -> Result<(), String> {
        self.backend.flush()?;

        Ok(())
    }

    pub(crate) fn read_all_buckets(
        &self,
    ) -> Result<HashMap<String, ClientBucket>, String> {
        let mut buckets = HashMap::new();

        let write_options = WriteOptions::default();
        let tx_options = OptimisticTransactionOptions::default();
        let inner = self.backend.transaction_opt(&write_options, &tx_options);

        let mut iter = inner.raw_iterator();
        iter.seek_to_first();

        while iter.valid() {
            let key = iter.key().expect("non empty key");
            let value = iter.value().expect("non empty value");

            let bucket_id = String::from_utf8_lossy(key).to_string();

            buckets.insert(
                bucket_id,
                bincode::deserialize(value)
                    .map_err(|_| "Failed to deserialize bucket")?,
            );
            iter.next();
        }

        Ok(buckets)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempdir::TempDir;

    #[test]
    fn test_db() {
        let tmp_dir = TempDir::new("test_db").expect("valid temp dir");
        {
            let db = DB::create_or_open(tmp_dir.path());

            let mut dummy_bucket = ClientBucket::new("bucket_id".to_string());
            dummy_bucket.files.insert([1u8; 32], "file_1".to_string());
            dummy_bucket.files.insert([2u8; 32], "file_2".to_string());

            assert!(db.update_bucket(&dummy_bucket).is_ok());
            assert!(db.flush().is_ok());
        }

        let db = DB::create_or_open(tmp_dir.path());
        let buckets = db.read_all_buckets().expect("valid load");

        assert_eq!(buckets.len(), 1);

        let bucket = buckets.get("bucket_id").unwrap();
        assert_eq!(bucket.bucket_id, "bucket_id");
        assert_eq!(bucket.files.len(), 2);
    }
}
