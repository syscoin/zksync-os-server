# Docker

```
sudo docker build --pull --no-cache -t zksync_os_sequencer .
sudo docker run -d --name sequencer -p 3050:3050 -p 3312:3312 -e batcher_maximum_in_flight_blocks=15  -v /mnt/localssd/db:/db   zksync_os_sequencer
```

<!-- SYSCOIN: Port publishing cannot reach a listener bound to the container's own loopback. -->
Do not publish prover port `3124` directly. For remote workers, run the generated buffering HTTPS
proxy in the node's network namespace (or use host networking with the host proxy) so it alone can
reach the loopback prover listener.

Release images must refresh both the locked Rust TLS dependencies and the system
CA store. The Dockerfile disables emSign ECC Root CA - G3 by its exact certificate
SHA-256 fingerprint, rebuilds Debian's CA bundle, and checks the effective SSL
trust store during the build. Other emSign roots are retained. `--pull --no-cache`
ensures a release build refreshes the base image and package metadata instead of
reusing an older CA-install layer.

Native installations and custom CA bundles require the same root removal before
deployment; rebuild/restart clients after changing their trust store. Check the
effective store with `python3 scripts/check-trust-store.py`. On Debian, running
the script as root with `--refresh-system` disables the matching packaged root
and regenerates the system bundle. A custom `SSL_CERT_FILE` or `SSL_CERT_DIR` is
still checked and must independently exclude the distrusted root. The Rust
`rustls-webpki` update also rejects the eight compromised intermediate public
keys; refreshing the root bundle alone does not replace that verifier fix.
