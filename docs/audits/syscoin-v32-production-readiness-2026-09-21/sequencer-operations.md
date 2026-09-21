# Sequencer host evidence

Read-only SSH to the supplied `ubuntu@148.251.44.149`, observed September 21, 2026 around 19:56–20:04 UTC. Private key content, full environment values and secret configuration fields were not collected. No service was restarted and no transaction was submitted.

## Expected v31 baseline

- Host `zktest-seq`; uptime 123 days; root filesystem 22% used (181/905 GiB, 679 GiB available); 62 GiB RAM, approximately 58 GiB available. RAID1 members reported healthy `[UU]`. Reboot-required marker exists.
- Gateway PID 2361920: executable `/home/ubuntu/gateway/.gateway-launch/target/gateway/release/zksync-os-server`, config `/home/ubuntu/gateway/os-server-configs/gateway/config.yaml`. Binary SHA-256 `c53b653dc7cdb178c84e9f163ffc6932a8eabf3c0c575144c7d7c197bfb07e3b`.
- zkSYS PID 2364459: executable `/home/ubuntu/gateway/.gateway-launch/target/zksys/release/zksync-os-server`, config `/home/ubuntu/gateway/os-server-configs/zksys/config.yaml`. Binary SHA-256 `19d95dc543de1cfcfc6151b83fa1cf801e7b78f50324247dc5313e5e4bad3fb6`.
- Both start scripts explicitly set `PROTOCOL_VERSION="v31.0"`. Main source checkout is `14346c9c714f14b0546e940e84816b54d676d530` (August 18); the separate idle v32-test checkout is `ed44d21aaf7f7085ddef7d68e0742ee5ab0f4fc4`. These source locations are provenance context, not proof of reproducible binary correspondence.
- Both runtime configs have `prover_api.enabled=false`, `fake_fri_provers.enabled=true` and `fake_snark_provers.enabled=true`. Process environment also contained `PROVER_MODE=gpu`; that environment label does not override the observed fake-proof configuration and must not be treated as proof of real proving.
- Chain IDs: root NEVM 5700, Gateway 57001, edge 57057. Gateway RPC reports `zksync-os/v0.22.0`; NEVM reports `Geth/v5.1.0-stable-3e3248c5/linux-amd64/go1.26.3`, syncing false. Gateway also reports syncing false. Runtime execution-version metric is 6, consistent with the intentional v31 baseline.

## Liveness and settlement snapshot

- Gateway block/tree/executor height 14423; most recent completed batcher/commit/prove/execute batch 3767, through block 14416. The seven-block difference is a snapshot of the pipeline, not proof of a stall.
- Read-only L1 calls to Gateway diamond `0x11debe8d1c1d352256f658fe3ba4c048d08055e8` independently returned committed=verified=executed=3767, protocol version `133143986176` (`31 << 32`), verifier `0x5269a0a2b49b6c0a9aa9e9f705f9928add23604b`, admin `0x7603928e527deebeb9c47542faac8fb83a27681f`.
- Edge block/tree/executor height 6454; batcher/commit/prove/execute metrics all 4757. The explorer-host independent EN and explorer snapshot agreed on block 6454 and its hash. These edge batch counters are runtime telemetry; this audit did not resolve and independently verify the currently selected edge diamond on Gateway. The L1 config diamond address returned empty code/results when queried on Gateway and was not misreported as its active identity.
- Recent bounded log samples had no ERROR entries; warnings were priority-fee-floor application and edge discovery reporting no closest peers. Public EN agreement is stronger evidence than treating the discovery warning alone as a connectivity failure.
- The loopback edge RPC on port 3050 timed out, while metrics and the independent EN were available. This route's access policy was not resolved; it is not classified as a network-wide RPC outage. Gateway and NEVM loopback RPCs responded.

## Production operating gaps to close before reset/launch

1. Metrics explicitly report `replay_archive_type=Noop` for both chains. Independent encrypted replay archives and successful cold restore were not demonstrated. EN disks are not a substitute for an independent archive.
2. Both node processes are children of shell wrappers in login-session scopes. No zksync/Gateway/zkSYS service unit was found in the unit-file inventory; Ubuntu has no crontab. This does not rule out every external supervisor, but supervised reboot/restart behavior is unproven. Use an explicit service manager and rehearse restart, operator-key availability and failover.
3. Both chain producers, root Syscoin/sysgeth and settlement workflows are colocated on this host. No multi-host Gateway consensus topology was observed. Current configs omit `consensus`; do not assume a quorum-canonized head for v32 merely because P2P ENs exist. Raft and settlement-owner availability require separate acceptance.
4. Runtime metrics show event watcher confirmations=2 and DA mode=Confirmations; generated config showed DA confirmations=1. Those are testnet policies, not production finality evidence. Define and verify the v32 L1/DA/Gateway policies separately.
5. Certbot, snap Certbot renewal and Postfix units were failed at inspection. Certificate validity and renewal remediation for this host were not established. Fix actual renewal ownership and add expiry/failure alerts before relying on its TLS endpoints. Both hosts require planned maintenance/reboot and independent recovery capability.
6. No identifiable membership reconciler was seen among node processes, service units or the inspected cron inventory. The explorer-host review likewise found no dedicated service. A generic or externally hosted keeper remains possible and must be identified before asserting reward correctness under membership changes.

## Ingress and permission limits

Listeners included public binds for RPC 3050/3052, status 3071/3072, metrics 3312/3313 and P2P 3060. An external bounded TCP check to those ports plus 8000 timed out; therefore public bind addresses alone were not labeled public exposure on this host. Effective firewall rules were unavailable because `sudo -n` required a password. The explorer host has separately confirmed public exposures described in its report.

No backup content, signing key, password, cookie, full config environment or private RPC credential was included in this evidence. No load test, failure injection, dependency upgrade, firewall edit or production permission change was performed.
