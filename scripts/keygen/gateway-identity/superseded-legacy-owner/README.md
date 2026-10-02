# Superseded offline plan — not deployed

These four JSON plan/calculation artifacts are retained byte-for-byte from the
earlier 2026-09-28 offline derivation. `asset-hashes.json` is the earlier complete
package inventory, retained as provenance rather than an assertion that every
older reproduction asset is duplicated in this directory.

The intended owner/security council `0x5b612a8914c3a891d235d0744a7d77685c6998da`
was observed on the legacy deployment, but its signer custody was not recovered.
The fresh V32 plan now uses the authenticated preprovisioned V32 administrator.
Consequently the old target `0xd3f0b1e7793d784668f8169f8b8927dddd8bbd6a` and guest
tree `84b1d7dd2ae1cb359894ce7a5709870930ef48f6` are superseded candidates, not
deployment or release inputs. No candidate VK was promoted under this plan.

The old deployment and the separate historical integration identity at
`local-chains/v32.0/gateway-identity.v1.json` remain unchanged. Consult the parent
directory for the active offline plan; its production and regeneration gates
remain in force.
