# Prover-owned rental tooling

Runpod rental control, GPU worker adapters, policies, image builds, tests and the
continuous supervisor live in **zksync-airbender-prover**, under
`scripts/prover-rental/`. The prover operator runs the controller on a trusted
machine; its rented pods execute FRI/SNARK work. The sequencer does not rent GPUs.

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

The server keeps job queues, leases, verification and contract-specific service
accounting. Its [service handoff](../prover-service/rental-handoff.md) still binds
external FRI duties and selected SNARK turns before compute is scheduled. The
shims set `ZKSYNC_OS_SERVER_DIR` for that checkout when it is not already set.
Neither moving the tools nor starting a dry run creates cloud resources.
