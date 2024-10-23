
# Storage server

###  This server stores files in logical buckets. Each bucket maintains a Merkle tree, which enables on-demand Merkle proofs.

REST APIs
 
- File Upload `POST /upload/:bucket_id/:file_name`
    - Upload a file to a specific bucket

- File request `GET /file/:bucket_id/:file_index`
    - Retrieve a file by its index from a specified bucket.

- Proof request `GET /proof/:bucket_id/:file_index`
    - Retrieve a Merkle proof for a specific file in a bucket.

## Merkle tree

### build_merkle benchmark 

- Build a merkle tree with leaves count 10_000, 100_000, 1000_000
- 14th Gen Intel(R) Core(TM) i9-14900k

```
 build_merkle_10000      time:   [832.36 µs 832.70 µs 833.02 µs]
 build_merkle_100000     time:   [8.3247 ms 8.3281 ms 8.3315 ms]
 build_merkle_1000000    time:   [85.651 ms 85.723 ms 85.800 ms]
```

### verify_merkle_proof benchmark
- Generate and verify a proof for a leaf in 100_000 leaves tree
- 14th Gen Intel(R) Core(TM) i9-14900k

```
verify_merkle_proof_100000 time:   [1.2044 µs 1.2055 µs 1.2069 µs]
```



## Example HTTP Client

Features:

- Uploads all files from a source folder to the server in encrypted form.
- Maintains a Merkle root of the uploaded files.
- Requests both a file and its Merkle proof from the server.
- If the proof is valid, the client decrypts the file and stores it locally.
- Simple prompt

## How to run

```
docker-compose build

# Bootstrap the server and run client with example files
docker-compose run client
docker-compose logs server -f
```

## Run unit tests and benches
```
cargo test
cargo bench
```
