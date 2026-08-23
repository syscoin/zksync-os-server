# Replay Archive

`zksync_os_replay_archive` stores cold-storage copies of block replay records. The archive is an
extra safety layer for cases where local node storage is lost or corrupted: replay records are
written outside the node RocksDB path and can later be used to rebuild the node replay WAL.

The archive stores replay records only. It does not store batch metadata. Batch information can be
recovered from L1 committed batch range events once block replay records are available.

## Storage Layout

> **SYSCOIN:** Upstream currently uses a shared flat first-writer namespace. Syscoin keeps writer
> sessions so archive presence cannot be supplied by a different writer.

Each node process creates a writer-owned session. Replay records use:

```text
<timestamp_millis>-<random_nonce>-<node_id>/<block_number>/<block_hash>
```

For the filesystem backend, the full path is:

```text
<archive_root>/<timestamp_millis>-<random_nonce>-<node_id>/<block_number>/<block_hash>
```

S3 and GCS use the same session-prefixed value as the object key.

> **SYSCOIN:** Flat `<block_number>/<block_hash>` archives are deliberately not read because
> they do not carry trustworthy writer provenance. This format has not been released; production
> deployments must start with an empty archive bucket or prefix. Experimental flat archives must be
> recovered with the pre-session tooling and re-archived, never copied into the session namespace.

The object value is the replay record payload only. There is no wrapper, batch number, block range,
or extra archive metadata in the object body.

Implementations of `ReplayArchiveStorage` must provide isolated, append-only writes:

- `init` must create a fresh random session and fail if that session already exists.
- `contains_object` checks the exact object identity published by this process in its current
  session. Its successful result consumes the process-local identity because the single L1 commit
  gate checks each block once.
- `append_object` creates a missing key and fails if the key already exists.
- Existing archive data must never be accepted as this writer's successful append.

## Write Path

The node constructs a `ReplayArchiver` from the configured backend and starts
`ReplayArchiveComponent`.

`ReplayArchivingWriteReplay` writes records to replay storage and sends `(block_hash, ReplayRecord)`
to the component through a bounded channel. The actual archive write happens asynchronously in the
component. If the queue is full, backpressure is applied to replay storage writes.

The current queue size is `REPLAY_ARCHIVE_QUEUE_SIZE`.

### Concurrent Writes

`ReplayArchiveComponent` processes up to `MAX_PARALLEL_OBJECT_PUTS` archive operations
concurrently. Every operation writes inside the process's unique session.

For each replay record, the archiver builds the payload and performs an atomic conditional create:

- GCS uses `if_generation_match(0)`.
- S3 uses `If-None-Match: *`.
- The filesystem writes a complete temporary file and hard-links it to the final path.

Several nodes can archive the same `(block_number, block_hash)` without racing because their
session prefixes differ. An existing key inside the current session is an error and fails the
archive component. This makes the L1 commit gate depend on an object successfully published by the
local writer rather than on unverified presence created by another writer.

> **SYSCOIN:** S3 and GCS requests carry a fresh random upload token in object metadata. A
> conditional-create conflict is accepted only when the stored token and payload identity match
> that exact request. S3 verifies its SHA-256 checksum; GCS reads and hashes the exact returned
> generation on this rare path. This handles an SDK retry after a lost success response without
> accepting another writer's object.

> **SYSCOIN:** The L1 commit gate also revalidates the locally published object identity. S3 uses
> the service-validated SHA-256 checksum returned by `PutObject`, GCS uses the immutable object
> generation, and the filesystem backend hashes the current bytes. An overwrite by another holder
> of the archive credentials therefore fails closed instead of satisfying the gate.

> **SYSCOIN:** ENs and batcher-disabled main nodes do not install the L1 commit gate. Their archive
> component verifies and consumes each publication receipt immediately after append, preventing an
> unbounded per-block receipt map while retaining the same fail-closed object identity check.

> **SYSCOIN:** Rejected replay writes are archived only once for keys in the startup WAL range.
> Those startup keys are retained for the process session, while rejected writes above the startup
> tip are skipped because their successful insertion already queued them. This preserves restart
> backfill without letting a stale duplicate hit append-only storage after the short queue retry
> window, and without retaining every block produced for the lifetime of the process.

## Implementations

Current archive implementations:

- `FileSystemReplayArchiveStorage`: conditional object creation on local disk.
- `FileSystemReplayArchiver`: filesystem archiver that stores plaintext JSON replay records.
- `S3ReplayArchiveStorage`: conditional object creation in S3 or an S3-compatible service.
- `GcsReplayArchiveStorage`: conditional object creation in Google Cloud Storage.
- `AgeEncryptedReplayArchiver`: wrapper that JSON-encodes replay records and encrypts them with
  age before storing them in any `ReplayArchiveStorage`. Supports X25519 recipients and GCP KMS
  asymmetric keys.

Current reader implementation:

- `FileSystemReplayArchiveReader`: lists archive objects from the filesystem layout.
- `S3ReplayArchiveReader`: lists archive objects from S3.
- `GcsReplayArchiveReader`: lists archive objects from Google Cloud Storage.

Other storage backends should implement:

- `ReplayArchiveStorage` for node-side conditional-create/check operations.
- `ReplayArchiveStorageReader` for recovery-side object listing.

## Encryption

Encrypted archives use the age format with one of two recipient types. GCP KMS is the primary
mode for our deployments; age X25519 is available as a KMS-independent alternative.

With GCP KMS, the node is configured with the resource name of an `ASYMMETRIC_DECRYPT` key version
using an `RSA_DECRYPT_OAEP_*_SHA256` algorithm:

```text
projects/../locations/../keyRings/../cryptoKeys/../cryptoKeyVersions/..
```

The node fetches the public key once at startup (requiring only
`cloudkms.cryptoKeyVersions.viewPublicKey`) and wraps the per-record age file key locally with
RSA-OAEP; no private key material exists outside KMS. During recovery, unwrapping a record's file
key takes one KMS `AsymmetricDecrypt` call (requiring
`cloudkms.cryptoKeyVersions.useToDecrypt`), so key access can be revoked and audited. Recovery
currently decodes each archived record once during the canonical chain walk and again when writing
to RocksDB, so budget roughly two `AsymmetricDecrypt` calls per stored record.
Note that KMS-encrypted objects use a custom age stanza and can only be decrypted by the recovery
tool, not by the stock `age` CLI.

The key version resource name is embedded in the age header of every archived object, so it can be
recovered from the archive itself even if the node configuration is lost:

```console
$ head -c 300 <downloaded_object> | strings | head -2
age-encryption.org/v1
-> gcp-kms-rsa-oaep projects/../locations/../keyRings/../cryptoKeys/../cryptoKeyVersions/..
```

With age X25519, the node needs only the public recipient key:

```text
age1...
```

The private identity should be stored separately and used only during recovery:

```text
AGE-SECRET-KEY-...
```

Encryption is randomized, so the live gate verifies presence only inside its writer-owned session.
Recovery decrypts every session copy and requires the decoded records for a block/hash to agree.

## Recovery

Recovery has two steps.

First, download all archive objects into a local recovery layout:

```text
<output_root>/<block_number>/<block_hash>/<timestamp_millis>-<random_nonce>-<node_id>
```

Second, rebuild the node replay RocksDB from a canonical anchor:

```text
anchor = (latest_block_number, latest_block_hash)
```

The anchor must come from a trusted source, e.g. `eth_getBlockByNumber("latest")` on a healthy
replica, or a block explorer. When testing recovery (rather than responding to actual data loss),
the highest `<block_number>/<block_hash>` in the downloaded layout can be used as the anchor: it
is the latest record the archive contains.

If the archive was encrypted, recovery decrypts downloaded objects in memory when a GCP KMS key
version (`--kms-key-version`) or an age identity (`--identity-file` / `--age-secret-key`) is
provided. Decrypted replay records are not written to disk.

The recovery logic requires all session copies for each canonical block/hash to decode to the same
replay record. It then starts from the anchor, extracts the previous block hash from each record,
walks backward until block `0`, and writes the canonical chain into RocksDB from genesis upward.

## CLI

The recovery utility binary is `replay_archive_recovery`.

Download archive objects:

```bash
cargo run -p zksync_os_replay_archive --bin replay_archive_recovery -- \
  download \
  --archive-root ./db/replay_archive \
  --output-root ./replay_archive_downloaded
```

Download archive objects from S3:

```bash
cargo run -p zksync_os_replay_archive --bin replay_archive_recovery -- \
  download \
  --s3-bucket-base-url my-replay-archive \
  --s3-credential-file-path ./s3-credentials \
  --s3-region us-east-2 \
  --output-root ./replay_archive_downloaded
```

Download archive objects from GCS using Application Default Credentials. The caller needs
`storage.objects.list` and `storage.objects.get` on the bucket:

```bash
cargo run -p zksync_os_replay_archive --bin replay_archive_recovery -- \
  download \
  --gcs-bucket-base-url my-replay-archive \
  --output-root ./replay_archive_downloaded
```

On GKE, ADC uses Workload Identity without additional configuration. For external workload
identity federation, point ADC at the external-account configuration before starting the process:

```bash
GOOGLE_APPLICATION_CREDENTIALS=./wif-credentials.json \
cargo run -p zksync_os_replay_archive --bin replay_archive_recovery -- \
  download \
  --gcs-bucket-base-url my-replay-archive \
  --output-root ./replay_archive_downloaded
```

For local testing, initialize local ADC with `gcloud auth application-default login`.

Rebuild replay RocksDB from a KMS-encrypted archive (the primary mode for our deployments). The
caller needs `cloudkms.cryptoKeyVersions.useToDecrypt` on the key version. ADC requires no
credential CLI flags:

```bash
cargo run -p zksync_os_replay_archive --bin replay_archive_recovery -- \
  recover-rocksdb \
  --input-root ./replay_archive_downloaded \
  --replay-db-path ./db/block_replay_wal \
  --anchor-block-number 123 \
  --anchor-block-hash 0x... \
  --kms-key-version projects/../locations/../keyRings/../cryptoKeys/../cryptoKeyVersions/..
```

KMS uses the same ADC configuration as GCS. Every record decode costs one KMS
`AsymmetricDecrypt` call, and recovery decodes records during the chain walk and again while
writing to RocksDB (roughly two calls per stored record); `--decrypt-concurrency`
(default 32) bounds the number of in-flight KMS requests.

Rebuild replay RocksDB from an unencrypted archive:

```bash
cargo run -p zksync_os_replay_archive --bin replay_archive_recovery -- \
  recover-rocksdb \
  --input-root ./replay_archive_downloaded \
  --replay-db-path ./db/block_replay_wal \
  --anchor-block-number 123 \
  --anchor-block-hash 0x...
```

For age-X25519-encrypted archives, pass the age identity file to `recover-rocksdb`:

```bash
cargo run -p zksync_os_replay_archive --bin replay_archive_recovery -- \
  recover-rocksdb \
  --input-root ./replay_archive_downloaded \
  --replay-db-path ./db/block_replay_wal \
  --anchor-block-number 123 \
  --anchor-block-hash 0x... \
  --identity-file ./replay-archive.key
```

Alternatively, provide the `AGE-SECRET-KEY-...` value directly through
`REPLAY_ARCHIVE_AGE_SECRET_KEY`:

```bash
REPLAY_ARCHIVE_AGE_SECRET_KEY='AGE-SECRET-KEY-...' \
cargo run -p zksync_os_replay_archive --bin replay_archive_recovery -- \
  recover-rocksdb \
  --input-root ./replay_archive_downloaded \
  --replay-db-path ./db/block_replay_wal \
  --anchor-block-number 123 \
  --anchor-block-hash 0x...
```

`--replay-db-path` must point to the `block_replay_wal` RocksDB directory, not the parent node
storage directory.

## Node Configuration

Replay archiving is configured by `ReplayArchiveConfig`.

Default:

```yaml
replay_archive:
  type: Noop
```

Filesystem archive with age encryption:

```yaml
replay_archive:
  type: FileSystem
  root_path: ./db/replay_archive
  encryption:
    type: AgeX25519
    recipient: age1...
```

S3 archive with age encryption:

```yaml
replay_archive:
  type: S3WithCredentialFile
  bucket_base_url: my-replay-archive
  s3_credential_file_path: ./s3-credentials
  endpoint: null
  region: us-east-2
  encryption:
    type: AgeX25519
    recipient: age1...
```

The S3 backend follows the old object-store initialization path: credentials are loaded from the
configured credentials file, `endpoint` overrides S3 API endpoint for S3-compatible
providers, and `region` is used as the first region provider before falling back to the SDK
defaults and then `auto`.

> **SYSCOIN:** An S3-compatible endpoint must support SHA-256 checksums on `PutObject` and
> checksum mode on `HeadObject`. When bucket-level SSE-KMS is enabled, grant the node the KMS
> permissions required by the provider to retrieve object checksums; AWS S3 general-purpose
> buckets require `kms:Decrypt`.

GCS archive with GCP KMS encryption (the primary mode for our deployments):

```yaml
replay_archive:
  type: Gcs
  bucket_base_url: my-replay-archive
  encryption:
    type: GcpKms
    kms_key_version: projects/../locations/../keyRings/../cryptoKeys/../cryptoKeyVersions/..
```

Both GCS and KMS use the Google Cloud client libraries' Application Default Credentials chain.
This discovers GKE Workload Identity automatically, reads an external workload identity federation
configuration from `GOOGLE_APPLICATION_CREDENTIALS`, or uses local credentials created by
`gcloud auth application-default login`. The node only ever uses the KMS public key, so its
identity needs `cloudkms.cryptoKeyVersions.viewPublicKey` and should not be granted `useToDecrypt`.
