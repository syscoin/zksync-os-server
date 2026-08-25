# Exposed Ports

<!-- SYSCOIN: The prover application port is intentionally not remotely exposed. -->
* `3050` - L2 JSON RPC
* `3060` - P2P communication (e.g. replay transport)
* `3124` - Loopback-only prover API (only enabled with the prover component); remote workers must
  use the generated buffering HTTPS proxy in the same network namespace
* `3312` - Prometheus
