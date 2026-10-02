# Offline critical Gateway identity (not a deployment attestation)

This public-only record binds the planned fresh V32 namespace to the new guest
target, without rewriting the historical integration candidate stored under
`local-chains/v32.0/gateway-identity.v1.json`.

`offline-critical-identity.json` is the unmodified independent-check output from
2026-09-28. Its SHA256 is
`2efa802311ab4efb3827e820ba41df33867820d11e7f08c3ec62f27c7dc3bf3a` (10,173 bytes).
It explicitly reports `scope=critical-subtree`, `deployed=false`,
`production_attested=false`, `independent_host_reproduction=false` and
`full_ctm_calculate_addresses_executed=false`. The user selected one keygen host;
the independent CREATE2 implementation is not an independent-host attestation.

The exact planned namespace salt and Governance constructor policy are in
`planned-v32-namespace.json`. The namespace is unchanged, but governance uses
the recovered preprovisioned V32 administrator
`0x622a54ea3a123127ca5fe8b98de90e957471093a`. Its supported keystore was decrypted
and its public address derived on the original host; no message-signature
challenge or broadcast is claimed. The plan binds the private readiness evidence
by digest without publishing secret-location metadata.

The planned timelock proxy is
`0xabb69e8e899c06e51414efde62d4423de4f35004`; the original integration candidate
`0xca38dbb6ea5f740cc8252f1450def4dcede94478` remains historical, not a current
deployment authorization. Relay address/runtime and proxy runtime are unchanged.

The record is bound to Era source-patched tree
`3eefa0f127d1deff365ebffcf489b183cde0e756` and newly repinned guest tree
`6935489bdbc7b1ed31e608677d1b2418b10691b5`. Absolute temporary artifact paths in
the retained result are provenance of that calculation, not paths CI must trust
or a claim that artifacts are present here. Compiled binaries, caches, private
inputs and signers are deliberately not included.

The fresh-CTM capacity correction changed only deployment/test sources. A new
offline Solidity-helper dry-run and independent metadata/CREATE2 check bound this
capacity-fixed source tree; the entire `derivations` object is unchanged from the earlier
`2a28a08` result (SHA256
`0b0b2f2f8032167257a179712783c14c7d6e6a3799848ec77b9a1d471683a48f`).
The original input, helper output and checked result remain in the task evidence;
this source rebind does not change the namespace, guest target or runtime hashes.

The subsequent source-only artifact refresh reproduced all 278 inventory rows
using each package's pinned compiler profile. It corrected only stale old Era
FFLONK/PLONK inventory rows and the two admin ABI snapshots; it did not introduce
the candidate app verifier or verifier-deployer hashes. A new pinned helper run
and independent check bound tree `331f5b8`; the complete `derivations` object is
also identical to the capacity-fixed `cd07acd` result (SHA256
`fad8fa83881e4412b49497a39568f90304c09e01d1068b03b3e58758f3b00877`).
Earlier evidence remains unchanged in the task records.

The PLONK generator was then corrected to derive its domain size and root of unity
from the verification key rather than assume 2^24. The Security100 key requires
2^25; the unchanged real SNARK passed direct PLONK and production dual-verifier
EVM tests after regeneration. A new genuine critical helper/checker run binds the
source-only generator correction at `3eefa0f`; the entire `derivations` object
still equals the source-inventory record (SHA256
`e6c5ced394e4934d64e92d98e850472b8c4200322a0ba6e93126a49e220b5777`).
This critical record does not attest the full generated contract graph or a live
deployment; those require their own rebuilt artifact and fixture evidence.

The earlier legacy-owner plan and its input/helper/calculation files remain
byte-identical under `superseded-legacy-owner/`. That owner's custody remains
unverified. Its target and guest artifacts are superseded, not release inputs;
the existing network was not changed by either offline plan.

## Reproduction assets

- `critical-input.json`: authenticated-admin input rebound to the domain-correct generator source.
- `planned-root-config.json`: explicit fresh root and token-role selections,
  bound to the namespace plan. This records choices and the existing WETH
  read observation, not derived root/CTM addresses or a deployed network.
- `helper-critical.json`: fresh pinned Solidity-helper dry-run output for that exact input.
- `check_identity.py`: independent stdlib Keccak/ABI/CREATE2 implementation.
- `DeriveGuestBoundIdentity.s.sol`: exact upstream helper subtree calls; full CTM
  mode exists but requires complete typed inputs and has not been attested here.
- `prepare_input.py`, `run_offline.sh`, `foundry.toml`: preparation and isolated
  no-network dry-run harness.
- `REPRODUCTION.md`: original task reproduction recipe, compiler hashes and
  exact full-CTM input requirements.
- `asset-hashes.json`: hashes for the public evidence and reproduction files.

To reproduce, create a fresh external task directory, materialize and attest the
documented Era sources/toolchain, and copy the harness files into its `identity/`
directory. Copy the namespace plan into `public-inputs/`. Use the canonical
compiler settings and separate Cancun/Prague output directories described in
`REPRODUCTION.md`; do not compile deployment bytecodes using the harness config.
This package renames `critical-input-domain-generator.json` to `critical-input.json`.
`helper-critical.json` is `helper-domain-generator.json` with only a final JSON newline
added. The checker was rerun against those exact published bytes; its unmodified
`guest-bound-domain-generator-published.json` is `offline-critical-identity.json`.
Use new output filenames when rerunning.

Run its offline unit tests from the repository root:

```sh
PYTHONDONTWRITEBYTECODE=1 python3 -m unittest discover \
  -s scripts/keygen/gateway-identity -p test_check_identity.py -v
```

After VK generation, the full new CTM configuration must still be assembled from
the planned new root deployment, final genesis and force-deployment data; rerun
full `calculateAddresses` and verify the critical subtree stays unchanged.
The public plan records the bounded signer-custody check separately; live
collision/ownership checks, full deployment derivation, production authorization
and deployment have not been established by this record. Existing production
authorizers and regeneration gates remain in force.
