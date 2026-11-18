# Syscoin V32 mainnet deployment plan

**Status** Draft for review. Approve the public deployment inputs below before deriving the mainnet application and verification key.

| Role | Approved chain ID | Planned endpoint |
| --- | ---: | --- |
| Syscoin NEVM settlement | 57 | Operator-managed NEVM RPC |
| Syscoin Gateway | 57570 | `https://rpc-gw.syscoin.org` |
| zkSYS | 5757 | `https://rpc-zk.syscoin.org` |

Gateway and zkSYS use native SYS with 18 decimals. The Gateway settles on NEVM, and zkSYS settles through Gateway. The registry submission records both L2 identities as incubating. [Chain registry PR](https://github.com/ethereum-lists/chains/pull/9066)

## Public inputs to approve

| Input | Draft selection or decision needed |
| --- | --- |
| CREATE2 namespace | Proposed label `syscoin-v32-airbender-v2-mainnet-57-gateway-57570-zksys-5757`. Its SHA-256 over UTF-8 without a trailing newline is `0x25c2179ba05741511f849b0d92fa0a55534c6599556705ed98a9b86c96d82a13`. Review the label and check its derived addresses for collisions on chain 57. |
| Root governance | Supply the mainnet Governance admin and security-council addresses and the minimum delay in seconds. Verify public signer control and the handoff policy. |
| Root deployment | Supply the approved mainnet deployer and funder addresses and the reviewed root configuration, including the wrapped SYS address and AssetRouter inputs. |
| zkSYS token and fee ledger | Supply the token administrator and bootstrap signer addresses. Confirm the token initialization profile and the expected gas-tank address. Pali expects `0xb49943ea232624dd4aa63e18186076c6c99a68ef` on chain 5757. Verify that the final token and gas-tank init code derives that address and the expected runtime hash. [Pali #1529](https://github.com/syscoin/pali-wallet/pull/1529) |
| Compact Edge-DA | Use the reviewed deterministic relay init code and salt, verify the factory and relay runtime identities, and derive the Gateway ValidatorTimelock and its runtime from the approved Governance alias and namespace. |

## Qualification sequence

1. Derive the root Governance address, its alias, and the Gateway-local ValidatorTimelock and relay subtree with the existing offline tools. Record constructor inputs, source and tree IDs, compiler and artifact hashes, and verify the results independently. [Derivation inputs](../../scripts/keygen/gateway-identity/REPRODUCTION.md)
2. Build the mainnet guest and native server from the same reviewed deployment identities. Bind Gateway `57570`, the ValidatorTimelock, relay and fee ledger in the guest and host mirror. Reproduce the guest bytes, program commitment, Security100 recursion chain and app-bound verification key. Record the PLONK domain and generate both verifier source copies and the Era inventory. Complete the root and CTM derivation from those artifacts, the final genesis and force-deployment inputs, and reconcile the resulting addresses with the guest. [Release bundle](../../scripts/releases/era-v32/README.md)
3. Rehearse the resulting package in isolation with signed transactions and genuine proofs. Compare native and guest public inputs, accept FRI through the server, and verify SNARKs in the production-mode dual router. Exercise Gateway-to-NEVM commit, prove and execute, withdrawal, and recovery against the derived topology.
4. Confirm the launch preflight accepts the signing domains and matching app release before funding or broadcast. Record the intended signer roles, exact artifacts and chain-57 deployment configuration for the operator’s launch approval.
