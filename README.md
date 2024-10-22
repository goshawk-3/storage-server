
## Storage server

HTTP APIs
 

- File Upload
`POST /upload/:bucket_id/:file_id`

- File request
`GET /file/:bucket_id/:file_id`

- Proof request
`GET /proof/:bucket_id/:file_id`

## Merkle tree benches

#### Bench test details:

- 14th Gen Intel(R) Core(TM) i9-14900k
- Build a merkle tree with leaves count 10_000, 100_000, 1000_000

 #### Results

```
 build_merkle_10000      time:   [832.36 µs 832.70 µs 833.02 µs]
 build_merkle_100000     time:   [8.3247 ms 8.3281 ms 8.3315 ms]
 build_merkle_1000000    time:   [85.651 ms 85.723 ms 85.800 ms]
```

- 14th Gen Intel(R) Core(TM) i9-14900k
- Generate and verify a proof for a leaf in 100_000 leaves tree

```
verify_merkle_proof_100000 time:   [1.2044 µs 1.2055 µs 1.2069 µs]
```



## Example HTTP Client
Encrypts and uploads all files from folder `/app/client_files`.
It downloads and verifies all uploaded files.



```
docker-compose up --build
```