# loadbase

`loadbase` is a lightweight benchmark client for measuring sequencer throughput and inclusion latency against a running `zksync-os-server` node.

It is used in two modes:

- local benchmarking while iterating on server changes
- CI benchmarking with structured outputs uploaded to GitHub artifacts and bencher.dev

## What It Measures

Each run reports:

- `TPS10`: rolling 10-second throughput
- `TPSavg`: average throughput since the start of the run
- submit p50 latency
- inclusion p50 latency
- receipt timeouts and receipt RPC errors

The terminal output is intended for quick feedback during a run; the structured files are the source of truth for CI and historical tracking.

## Prerequisites

You need:

- a running `zksync-os-server` JSON-RPC endpoint
- a funded rich account private key
- Rust toolchain available for building/running `loadbase`

For local runs in this repository, the default node endpoint is usually:

- `http://127.0.0.1:3050`

## Recommended Local Setup

For local benchmarking in this repo, run the server in ephemeral mode. This avoids reusing stale RocksDB state and prevents local DB conflicts from affecting the benchmark.

Start the server:

```bash
general_ephemeral=true cargo run --release --bin zksync-os-server
```

Then run `loadbase`:

```bash
cargo run --release --manifest-path loadbase/Cargo.toml -- \
  --rpc-url http://127.0.0.1:3050 \
  --rich-privkey 0x7726827caac94a7f9e1b160f7ea819f172f7b6f9d2a97f992c38edeab82d4110 \
  --duration 2m \
  --max-in-flight 15 \
  --wallets 20 \
  --dest random \
  --output-dir ./benchmark-out \
  --output-format all
```

For heavier local stress testing, increase `--wallets`, `--max-in-flight`, and `--duration` gradually instead of jumping straight to large values.

## Key CLI Knobs

- `--duration`: total benchmark runtime
- `--wallets`: number of funded test wallets used for load generation
- `--max-in-flight`: upper bound on concurrently outstanding transactions
- `--dest random`: distribute transfers across random recipients
- `--output-dir`: enable structured artifact output
- `--output-format`: choose `text`, `json`, `csv`, or `all`

In practice:

- raise `--max-in-flight` to push throughput harder
- raise `--wallets` to reduce nonce bottlenecks per wallet
- lower both when the environment becomes unstable or CI starts stalling

## Output Files

When `--output-dir` is set, `loadbase` always writes:

- `summary.json`
- `report.md`
- `bencher.json`

Additional files depend on `--output-format`:

- `metrics.json` with `json` or `all`
- `metrics.csv` with `csv` or `all`

### File Roles

- `summary.json`: compact run summary used by CI summaries and external integrations
- `report.md`: human-readable markdown report
- `bencher.json`: Bencher Metric Format payload for bencher.dev
- `metrics.json`: per-sample time series in JSON
- `metrics.csv`: per-sample time series in CSV

## Terminal Output Example

```text
⏱   58s | sent    6191 | in-fl    52 | incl    6139 | TPS10  103.6 | TPSavg  105.8 | sub p50 10s   1 ms / tot   1 | inc p50 10s  0.51 s / tot  0.51 s
⏱   59s | sent    6266 | in-fl    52 | incl    6214 | TPS10  103.4 | TPSavg  105.3 | sub p50 10s   1 ms / tot   1 | inc p50 10s  0.51 s / tot  0.51 s
⏱   60s | sent    6373 | in-fl    52 | incl    6321 | TPS10  114.1 | TPSavg  105.3 | sub p50 10s   1 ms / tot   1 | inc p50 10s  0.51 s / tot  0.51 s
```

Field meanings:

- `sent`: total submitted transactions
- `in-fl`: currently in-flight transactions
- `incl`: included transactions
- `TPS10`: rolling 10-second throughput
- `TPSavg`: average throughput over the whole run
- `sub p50`: median submit latency
- `inc p50`: median inclusion latency

## CI Workflow

The benchmark workflow lives in [.github/workflows/loadtest.yml](/Users/abalias/projects/matter-labs/zksync-os-server/.github/workflows/loadtest.yml).

It runs in three modes:

- `pull_request`
- `workflow_dispatch`
- scheduled daily run on `main`

Current benchmark profiles:

- PR runs:
  - `--duration 90s`
  - `--wallets 20`
  - `--max-in-flight 15`
- manual/scheduled runs:
  - `--duration 3m`
  - `--wallets 30`
  - `--max-in-flight 25`

These CI settings are intentionally milder than aggressive local stress runs. The goal in CI is repeatable regression tracking, not maximum saturation at all costs.

The workflow also starts the server with `general_ephemeral=true` to avoid stale local database state interfering with the run.

## Bencher Integration

CI uploads `bencher.json` to bencher.dev after each benchmark run.

Current exported benchmarks:

- `sequencer/tps10_median` as `throughput`
- `sequencer/tps10_p95` as `throughput`
- `sequencer/include_latency_p50` as `seconds`
- `sequencer/receipt_timeouts` as `count`
- `sequencer/receipt_errors` as `count`

Interpretation:

- throughput metrics are the primary regression signal
- inclusion latency is tracked in seconds
- timeout/error counts are health signals, not latency measurements

Bencher becomes useful only after repeated runs accumulate on a stable branch/testbed combination. A single run mostly confirms that the upload worked.

To get alerts or regression comments, configure thresholds in bencher.dev for the relevant branch and testbed.

## Monitoring and CI Artifacts

The workflow uploads benchmark artifacts including:

- server log
- `loadbase` log
- `summary.json`
- `report.md`
- `metrics.json`
- `metrics.csv`
- `bencher.json`

When Pushgateway is configured, CI also exports:

- `loadbase_median_tps10`
- `loadbase_p95_tps10`
- `loadbase_median_include_p50_10s_seconds`
- `loadbase_final_included`
- `loadbase_sample_count`
- `loadbase_receipt_timeouts`
- `loadbase_receipt_errors`

## Troubleshooting

### Server never becomes ready

If the server hangs before opening port `3050`, check whether it is running without ephemeral mode and reusing stale on-disk state. In this repo, prefer:

```bash
general_ephemeral=true cargo run --release --bin zksync-os-server
```

### Throughput is unstable or CI appears stuck

Lower:

- `--max-in-flight`
- `--wallets`
- `--duration`

The first knob to reduce is usually `--max-in-flight`.

### Receipt timeouts or errors are non-zero

These counters are exported intentionally and are non-fatal diagnostics in CI. Treat them as signals that the environment may be overloaded or unhealthy, even when the benchmark completes.

### Bencher report looks unhelpful

Common reasons:

- only one run exists, so there is no trend yet
- thresholds are not configured
- the branch/testbed combination is not stable across runs

For useful Bencher charts, keep using the same branch baseline and testbed naming in CI.
