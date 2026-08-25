# Docker

```
sudo docker build -t zksync_os_sequencer .
sudo docker run -d --name sequencer -p 3050:3050 -p 3312:3312 -e batcher_maximum_in_flight_blocks=15  -v /mnt/localssd/db:/db   zksync_os_sequencer
```

<!-- SYSCOIN: Port publishing cannot reach a listener bound to the container's own loopback. -->
Do not publish prover port `3124` directly. For remote workers, run the generated buffering HTTPS
proxy in the node's network namespace (or use host networking with the host proxy) so it alone can
reach the loopback prover listener.
