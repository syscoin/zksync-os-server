- start server `BATCHER_BLOCKS_PER_BATCH_LIMIT=1 cargo run --release`

- send eth to address
```
cast send 0xD56da2e2Ef0C2AfF4c6aC0f73f05C794C3C9f162 -r localhost:3050 --gas-limit 210000 \
        --private-key 0x7726827caac94a7f9e1b160f7ea819f172f7b6f9d2a97f992c38edeab82d4110 \
        --value 1000000000000000000

```

- restart server
```
BATCHER_BLOCKS_PER_BATCH_LIMIT=1 PROVER_API_FAKE_FRI_PROVERS_ENABLED=false PROVER_API_FAKE_SNARK_PROVERS_ENABLED=false \
SEQUENCER_BLOCK_TIME="1 hour" SEQUENCER_MAX_TRANSACTIONS_IN_BLOCK="10000" OBSERVABILITY_LOG_FORMAT="logfmt" \
SEQUENCER_BLOCK_PUBDATA_LIMIT_BYTES="1100000" SEQUENCER_BLOCK_GAS_LIMIT="2000000000" cargo run --release &> server_log.txt
```

- start provers

- update run.sh with tests you want to run, e.g.
```
cargo run --release -- modexp 10
cargo run --release -- modexp 20
cargo run --release -- modexp 50
cargo run --release -- modexp 100
```

- `./run.sh &> run.txt`
- wait for batches to be proven, load prover logs to prover_logs.txt
- run `python ./process.py ./run.txt ../server_log.txt ./prover_logs.txt`
- it will output statistic e.g.
```
Flow: ("modexp", 10). Block number: 9. computaional_native_used: 218227580. seconds: 45.710877499. native/ms: 4774.0842
Flow: ("modexp", 20). Block number: 12. computaional_native_used: 435838460. seconds: 32.896037998. native/ms: 13248.9651
Flow: ("modexp", 50). Block number: 15. computaional_native_used: 1089029600. seconds: 59.157741091. native/ms: 18408.9112
Flow: ("modexp", 100). Block number: 18. computaional_native_used: 2176928900. seconds: 101.18823053. native/ms: 21513.6572
```