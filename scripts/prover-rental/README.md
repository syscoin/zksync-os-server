# Prover-owned rental tooling

Runpod rental control, GPU worker adapters, policies, image builds, tests and the
continuous supervisor live in **zksync-airbender-prover**, under
`scripts/prover-rental/`. The prover operator runs the controller on a trusted
machine. New supervisor examples use Runpod Serverless with FlashBoot for FRI
and the existing rented Pods for SNARK. The sequencer does not rent GPUs.

These Python entry points are compatibility shims for existing service commands.
Configure the prover checkout before using them or the FRI dispatcher:

```sh
export ZKSYNC_AIRBENDER_PROVER_DIR=/absolute/zksync-airbender-prover
python3 "$ZKSYNC_AIRBENDER_PROVER_DIR/scripts/prover-rental/supervisor.py" --help
```

Adjacent repository checkouts are discovered automatically. For separate
worktrees, set the variable explicitly. Existing private state directories and
journals stay at their operator-chosen paths; do not reset them during migration.
Read `scripts/prover-rental/README.md` in the prover checkout for setup, image
qualification and the independently supervised provider watchdog.

For new setups, initialize the separate Serverless FRI policy and leave
`serverless_fri.enabled` set to `true`. The endpoint must explicitly enable
FlashBoot, with zero minimum workers, one maximum worker and a five-second idle
timeout. Set `serverless_fri` to `{"enabled": false}` before supervisor
initialization to use the existing Pods workflow for both stages. Older saved
configurations without that field continue using Pods. Existing journals are not
converted automatically.

The FRI image contains the compiled CUDA prover and pinned dependencies; it needs
no CRS. FlashBoot can retain its initialized process and GPU state between jobs,
but eviction still requires a cold start from the published image. Setup and
endpoint validation commands are in the prover-owned guide.

The server keeps job queues, leases, verification and contract-specific service
accounting. Its [service handoff](../prover-service/rental-handoff.md) still binds
external FRI duties and selected SNARK turns before compute is scheduled. The
shims set `ZKSYNC_OS_SERVER_DIR` for that checkout when it is not already set.
Neither moving the tools nor starting a dry run creates cloud resources.
