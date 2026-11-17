# Syscoin v32 port to public Airbender V2

This record follows the pre-launch testnet candidate. The comparison release is Syscoin server v0.24.4 (`d22deb8a`) and prover v0.9.1 (`b607b5cc`). The public proving bases are Airbender V2 `0decdcdb` and the protocol monorepo `2a2ecb3b`. Syscoin’s Airbender `main` fast-forwarded to the exact upstream commit in [PR #5](https://github.com/syscoin/zksync-airbender/pull/5). The circuit port now targets that branch directly. The server owns the patches for both Matter Labs OS v0.4.0 (`69bc4305`) and Era contracts (`8fb7c29a`). The v32 guest, ordered batch statement, and edge → Gateway → NEVM route are the integration targets.

## Changes first made in this port

| Change | Reason and observed result | Review |
| --- | --- | --- |
| Initialize Rust verifier scratch storage before forming typed values or references. Use field zero for small arrays and fill the large proof and query buffers through raw pointers. | The old Syscoin fork and public V2 both constructed unwritten integer-containing values with `assume_init()`. This is undefined behavior in Rust. The transcript still supplies the FRI folding challenges. Acceptance of an invalid proof has not been demonstrated. | [Verifier implementation](https://github.com/syscoin/zksync-airbender/commit/7166ba7f7f71feee3fbf81e8a92dc68914df05ca) |
| Propagate verifier thread panics and failures from concurrent recursive binary builds. | The old checks could report success after a verifier thread panicked or a child build failed. These changes improve the reliability of qualification. | [Thread results](https://github.com/syscoin/zksync-airbender/commit/e18e08617) · [Build results](https://github.com/syscoin/zksync-airbender/commit/46068ebbe) |
| Generate the recursive wrapper's runtime layout JSON and inline constraints from the same pinned circuit. | The public wrapper's checked-in runtime layout differed from its Airbender source. The regenerated candidate JSON matches the pinned native layout field by field. | [Generator and generated artifact](https://github.com/syscoin/zksync-protocol/pull/1) |
| Make the commitment helper's recursion security level explicit. | The earlier helper selected Security80 internally. The candidate can independently derive the base and final Security100 commitments for the Syscoin guest. | [Commitment helper](https://github.com/syscoin/zksync-airbender/commit/bbe1b4ea8) |
| Reject a nonzero reserved word on the native V8 settlement route. | Backport of Matter Labs' v32 correction. The existing VK and PLONK runtime were reproduced. All 40 router tests passed. Single-batch fuzzing checked 256 zero-carry cases for exact truncation and 256 nonzero-carry cases for the exact custom error. Both the Solidity PR and server patch contain the tests. The final router accepted the original two-batch proof and rejected a nonzero reserved word on isolated Anvil. Three of 278 inventory entries changed. The patched router also accepted the fresh v0.9.1 benchmark SNARK on isolated Anvil (calldata SHA-256 `2626dded25b2331c90fd9723efc69d6057f28afcbaf177013bfc02fa7121d824`). Slither 0.11.3 ran 56 configured detectors on the patched router and its dependencies and returned zero findings. | [Solidity review](https://github.com/syscoin/era-contracts/pull/20) · [Server-owned patch and pins](https://github.com/syscoin/zksync-os-server/pull/380) |
| Stream authenticated FRI queries through one scratch record. | A separate experimental change reduces the number of simultaneously retained query records from 87 to 1 at Security100. In the emitted Security100 RV32 code the largest identified function frame fell from 895,104 to 96,896 bytes for unified recursion and to 98,368 bytes for unrolled recursion. Across six alternating executions of the fresh batch-1 Security100 proof, both unrolled binaries consumed all 1,548,360 input words and matched all 16 native result registers. Executed cycles fell from 32,031,187 to 32,013,920 (0.054%). The interpreter timings did not establish a speedup. The default worker uses the non-streaming verifier. | [Streaming experiment](https://github.com/syscoin/zksync-airbender/pull/6) |
| Bind cached RISC setup to the recursion end parameters and pin the Era guest SDK used by the wrapper. | A cache built for another recursion program must be discarded before key generation or proving. | [Wrapper cache check](https://github.com/syscoin/zksync-protocol/pull/1) |
| Use the full log22 ceremony file for CPU key generation. | Compared all 4,194,304 G1 encodings and both G2 bases byte for byte against the authenticated log25 file. Inputs fell from 2,147,483,920 to 268,435,728 bytes. RSS and key generation time are measured separately. | [Worker implementation](https://github.com/syscoin/zksync-airbender-prover/pull/27) |
| Update the server's host oracle transpiler for the V2 Rust nightly. | The old transpiler relied on the former return type of `u32::widening_mul`. The server Cargo patch selects the same transpiler as its FRI verifier. The RV32 guest binaries used for the comparison come from the v32 release. | Server native integration |
| Initialize GPU host scratch and keep pooled host allocations alive through asynchronous CUDA work. | Completed callbacks and copies release their buffers for reuse. Pool pressure can force a stream synchronization. The implementation also corrects a CPU FFT scratch array. These changes affect prover execution and keep the guest artifacts byte-identical. | [GPU and FFT changes](https://github.com/syscoin/zksync-airbender/pull/7) |

## Existing fixes and optimizations carried forward

| Change | Verification or measurement | Review |
| --- | --- | --- |
| Require an active execution row for CSR delegation. Restore the 17-bit worst-case Security100 memory/delegation work setting in the generator and generated schedules. | The generator added one quadratic relation to the unified layout (112 to 113). It enforces `should_delegate × (1 − execute) = 0` in both verifier copies and the quotient. Native and recursive artifacts need the same schedule and layout. The port keeps the public V2 degree-lift transcript. | [Airbender port](https://github.com/syscoin/zksync-airbender/pull/4) |
| Bind the resident GPU program to the input bytes, preserve pinned buffer lifetimes, and stream empty initialization/teardown regions. | All seven targeted scheduling tests passed on the build server. CUDA-stub compilation checks the host interface. Real GPU testing is recorded separately. | [GPU port](https://github.com/syscoin/zksync-airbender/pull/4) |
| Reject incorrect Boojum query counts, require the caller's expected proof configuration, and constrain recursive Goldilocks query bits canonically. | The focused canonical query-bit test passed. Public V2 already covers the Goldilocks addition boundary. | [Recursive verification](https://github.com/syscoin/zksync-protocol/pull/1) |
| Reuse constrained FRI openings and folding arithmetic. Buffer fresh OS randomness at both final PLONK padding call sites. | With the same RISC VK and canonical-bit constraint, occupied Security100 compression rows fell from **53,789 to 41,640** (22.59%). Both circuits pad to 65,536 rows. Native-versus-circuit arithmetic checks and five randomness-buffer tests passed. | [Wrapper implementation and synthesis test](https://github.com/syscoin/zksync-protocol/pull/1) |
| Use public V2 recursion setup snapshots and preserve release identity checks, warm-worker scheduling, and host cache management. Adopt the public final PLONK domain of 2²². | The earlier Syscoin domain was 2²⁵. The GPU combiner still has its own device allocator, so the PLONK change alone cannot establish an 8 GB worker. | [Worker port](https://github.com/syscoin/zksync-airbender-prover/pull/27) · [Wrapper port](https://github.com/syscoin/zksync-protocol/pull/1) |

The worker build now binds its Cargo target and release receipts to the full source content. Reusing an older target had returned a stale unregistered binary for the updated registry source. Startup rejected it before a lease. The regression covers older file modification times, changes to ordinary Rust files and build-script inputs, and unchanged-input cache reuse. [Worker fix](https://github.com/syscoin/zksync-airbender-prover/commit/30eebe3) · [Regression and artifact checks](https://github.com/syscoin/zksync-airbender-prover/commit/0985777d073c67b26e8bd28e64e864e70dc2cde9)

The Linux lockfile scan also identified three November 17 HTTP parser advisories in Sentry's optional Actix integration. The patched version is 3.21.3. Both scoped cargo-audit and cargo-deny scans passed on the updated server port and the release branch. [Dependency PR](https://github.com/syscoin/zksync-os-server/pull/383)

## OS changes requiring a different guest

The candidate OS patch validates EVM memory operands at the same 32-bit width on the host and RV32. It also adds 808,664 native units to the fixed v32 block reserve for three storage reads, one derived key and finalization of the L2-to-L1 log tree. The server holds this patch separately from the v32 guest used for the proving comparison. Its replacement guest, commitment and settlement VK must be derived together before activation. [OS source](https://github.com/syscoin/zksync-os/pull/1) · [Server-owned candidate patch](https://github.com/syscoin/zksync-os-server/pull/382)

## Proving performance

The Syscoin v32 worker produces per-batch FRI proofs. The range worker merges consecutive FRI proofs, performs RISC wrapping and compression, and produces the final PLONK SNARK. Per-batch proving can be distributed. The final wrap depends on the completed range.

The comparison uses the released worker and the candidate on the same RTX 5090 with the same two contiguous native witnesses, Security100, and matching app-bound keys. Measurements track cold setup, warm FRI, range merge, RISC wrapping, compression, and final PLONK, along with process peak RSS and sampled GPU occupancy. These two minimal batches are a compatibility and latency workload. Sustained production throughput requires a representative transaction mix.


| Released v0.9.1 on the RTX 5090 | Observed value |
| --- | ---: |
| Startup to first FRI job lease | 104.351 s |
| First batch from lease to proof submission | 7.647 s |
| Second batch from lease to proof submission | 4.035 s |
| FRI worker process maximum resident set | 48,420,920 KiB |
| FRI card-wide peak at 500 ms intervals, 16 MiB before attach | 29,432 MiB |
| Cold SNARK startup to range lease | 231.862 s |
| Two-batch range lease to submission | 164.063 s |
| Range merge, including both Airbender proving passes | 7.769 s |
| RISC wrapper GPU proving | 52.514 s |
| Compression GPU proving | 1.252 s |
| Final PLONK GPU proving | 99.406 s |
| SNARK process maximum resident set | 87,554,396 KiB |
| SNARK card-wide peak sampled every 500 ms | 32,144 MiB |
| Device pool reservation reported at SNARK setup | 33,139,720,192 bytes |

The released worker's sampled card occupancy peaked at 22,594 MiB during the range merge, 32,140 MiB during RISC wrapping and again during compression, and 30,846 MiB during the final PLONK phase. The V2 GPU combiner requests 21,820 one-MiB allocator blocks. Its source records an upstream Security100 high-water mark of 21,564 blocks. The remaining 256 blocks provide fragmentation headroom. This configuration requires more than an 8 GiB GPU for the merge alone. The sampled occupancy and allocator suballocation peaks answer different questions. [Combiner configuration](https://github.com/syscoin/zksync-airbender/blob/d8965007df1bd7f11a5279a10fe36bbfebaaac58/tools/cli/src/prover_utils.rs#L872-L882)

### Normal V2 worker on the same RTX 5090

| Security100 measurement | Released v0.9.1 | Public V2 port |
| --- | ---: | ---: |
| Cold FRI startup to first lease | 104.351 s | 51.373 s |
| FRI first batch, lease to submit | 7.647 s | 7.313 s |
| FRI second batch, lease to submit | 4.035 s | 4.835 s |
| FRI process peak RSS | 46.18 GiB | 46.28 GiB |
| FRI board peak | 29,432 MiB | 29,434 MiB |
| Cold SNARK startup to lease | 231.862 s | 86.665 s |
| Two-batch range, lease to submission | 164.063 s | 71.660 s |
| RISC wrapper GPU proving | 52.514 s | 50.785 s |
| Compression GPU proving | 1.252 s | 1.402 s |
| Final PLONK GPU proving | 99.406 s | 13.405 s |
| Final PLONK board peak | 30,846 MiB | 5,296 MiB |
| Full SNARK worker process peak RSS | 83.50 GiB | 59.70 GiB |
| Full SNARK worker board peak | 32,144 MiB | 32,144 MiB |

The same V2 process repeated the two-batch range in 69.551 s. The smaller PLONK domain cut final-stage proving time by 86.5%. RISC wrapping now accounts for most of the warm range latency. In these samples the whole-worker peak board occupancy persisted. The server's normal byte decoder and app-bound verifier accepted both exact V2 packets against their independently derived batch statements, and rejected incorrect public input and trailing data. The native CLI boundary tests also passed. The fresh range SNARK was accepted by both normal compiled settlement contracts. [EVM receipt](syscoin-v32-airbender-v2-router-replay-2026-11-17.json)

### Streaming FRI verifier queries

The experimental verifier authenticates each opening against its committed Merkle caps, compares its index to the Fiat–Shamir transcript and completes its FRI folds before reusing the query storage. The Security100 query count and transcript inputs are the same. In RV32 replays, each workload consumed every oracle word and matched all 16 native result registers. [Streaming implementation](https://github.com/syscoin/zksync-airbender/pull/6)

| Genuine proof input | Distinct guest RAM words, default → streaming | RV32 cycles, default → streaming |
| --- | ---: | ---: |
| Unrolled base verifier | 282,607 → 85,666 | 32,031,187 → 32,013,920 |
| Unified single-batch verifier | 246,961 → 51,488 | 11,957,098 → 11,942,196 |
| Unified two-batch combiner | 247,143 → 51,670 | 23,917,963 → 23,888,159 |

The two-batch runs performed 697,648 Blake delegations and consumed 1,110,338 words in both implementations. CPU simulation took 163–170 ms with the default and 157–167 ms with streaming. These runs measure guest RAM and instructions. Device allocator high-water marks and end-to-end proof latency require a separate GPU comparison. The default contract proposal binds the non-streaming verifier. Adopting the streaming verifier changes the program commitment and requires a separately generated key.
