# Changelog

## [0.24.1](https://github.com/syscoin/zksync-os-server/compare/v0.24.0...v0.24.1) (2026-10-30)


### Bug Fixes

* **launch:** defer staking until service accounting is configured ([7b714e1](https://github.com/syscoin/zksync-os-server/commit/7b714e1079921c4f15d65520b04130eccd6be5fa))
* **prover-service:** index receipt worker journals ([4896fbe](https://github.com/syscoin/zksync-os-server/commit/4896fbe5007124d548280170464cd6e3234bdaab))
* **prover-service:** scan only relevant gate logs ([32defdf](https://github.com/syscoin/zksync-os-server/commit/32defdf99110745f0af5eb5d1b3f565ebda7f138))
* **prover-service:** target event discovery and defer staking setup ([420d59d](https://github.com/syscoin/zksync-os-server/commit/420d59dae5059542a76a7bf2fbac27675969b287))
* **prover-service:** use indexed receipt worker journals ([88c645a](https://github.com/syscoin/zksync-os-server/commit/88c645a01878f8fea87efead79bc44dd6c5cf253))
* **release:** align generated workspace pins and lockfile ([8cd34c9](https://github.com/syscoin/zksync-os-server/commit/8cd34c9f89851a57a840c93e259b945abc0c2ee4))
* **release:** sync internal pins and lockfile automatically ([a622f6c](https://github.com/syscoin/zksync-os-server/commit/a622f6cfe159aedce9716983ef207d927dba0a9d))

## [0.24.0](https://github.com/syscoin/zksync-os-server/compare/v0.23.0...v0.24.0) (2026-10-30)


### ⚠ BREAKING CHANGES

* **prover_service:** Upgrade the opt-in service tools together; migrated journals and streamed audit archives require compatible readers.
* **prover:** add senior service for zkSYS and Gateway
* **v32:** bind guardian consent to epochs and detect L1 reorgs
* **protocol:** reset zkSYS to canonical V32 and proving V8
* remove the Compacted state backend ([#1538](https://github.com/syscoin/zksync-os-server/issues/1538))
* **prover_api:** move the V8 lane to the zksync-os v0.4.0 release ([#1532](https://github.com/syscoin/zksync-os-server/issues/1532))
* **l1_sender:** pipelined sending with a bounded in-flight window ([#1485](https://github.com/syscoin/zksync-os-server/issues/1485))

### Features

* **batcher:** add bounded Bitcoin DA recovery publication ([#356](https://github.com/syscoin/zksync-os-server/issues/356)) ([d5ac205](https://github.com/syscoin/zksync-os-server/commit/d5ac20590686f80b785fa6bf0517cbd9421d5c41))
* **batcher:** permit an eight-slot coordinated DA window ([312c8a1](https://github.com/syscoin/zksync-os-server/commit/312c8a1a36e8bc280ba7e57b88c60df36fdf1070))
* **batcher:** permit an eight-slot coordinated DA window ([118d4f3](https://github.com/syscoin/zksync-os-server/commit/118d4f3f43f8ac0b3f040a1456c723b7223e79eb))
* bump alloy to v2.4.1 and reth to v2.5.0 ([#1518](https://github.com/syscoin/zksync-os-server/issues/1518)) ([8f17d02](https://github.com/syscoin/zksync-os-server/commit/8f17d02a886f051dcff5da5e2a5ac29393748052))
* **gateway:** add settlement stop boundary ([b5933b1](https://github.com/syscoin/zksync-os-server/commit/b5933b14a92e294816d5fb1d8b244afbbf58c9d8))
* **gateway:** harden V32 mock launch ([4d61be3](https://github.com/syscoin/zksync-os-server/commit/4d61be37583330f213bf37f4af717c22dfa7d157))
* **gateway:** harden V32 Tanenbaum mock launch and settlement ([7c05d81](https://github.com/syscoin/zksync-os-server/commit/7c05d81f58c02bb5d835204867a5d409da228a85))
* **l1_sender:** pipelined sending with a bounded in-flight window ([#1485](https://github.com/syscoin/zksync-os-server/issues/1485)) ([83a0dc8](https://github.com/syscoin/zksync-os-server/commit/83a0dc8ab363ec2fdfd7babf3a00c3f9c560f7f1))
* **launch:** inspect full mock Gateway identity without broadcasting ([526cf3b](https://github.com/syscoin/zksync-os-server/commit/526cf3b63630bd8b29a1bc5c96769451ec8f200c))
* **node:** refuse main-node startup when replay WAL is behind L1 ([#1504](https://github.com/syscoin/zksync-os-server/issues/1504)) ([7b5d906](https://github.com/syscoin/zksync-os-server/commit/7b5d9060a107ad9adba0fe4419ceb11b45293c39))
* **ops:** add opt-in idle proof companion heartbeat ([936ba4d](https://github.com/syscoin/zksync-os-server/commit/936ba4d559a514534a92a5b4265b49597121cd64))
* **protocol:** reset zkSYS to canonical V32 and proving V8 ([ce89948](https://github.com/syscoin/zksync-os-server/commit/ce899484c9b612c0c105ed1fb156a5736bdbbaed))
* **prover_api:** move the V8 lane to the zksync-os v0.4.0 release ([#1532](https://github.com/syscoin/zksync-os-server/issues/1532)) ([cceb02c](https://github.com/syscoin/zksync-os-server/commit/cceb02c8215036eb96fcf719c343594c2da41bd0))
* **prover_api:** v32.0 / proving V8 - make real proofs settle on L1 ([#1510](https://github.com/syscoin/zksync-os-server/issues/1510)) ([0bafb13](https://github.com/syscoin/zksync-os-server/commit/0bafb13d89d7da2f7adb9bad6c12d5301351a80d))
* **prover:** add senior service for zkSYS and Gateway ([0156211](https://github.com/syscoin/zksync-os-server/commit/0156211b1ff3dc827a65eccff716eba9cf6c977e))
* **prover:** automate deterministic wrapper coordination ([1231744](https://github.com/syscoin/zksync-os-server/commit/123174413b852490a6d81df529270390fbed0fc0))
* **prover:** bring back v30.x (V6) proving support ([#1527](https://github.com/syscoin/zksync-os-server/issues/1527)) ([308f6d5](https://github.com/syscoin/zksync-os-server/commit/308f6d57626f0ccff1aa70f1a366b40ed0fc7101))
* **prover:** drop pre-0.3 proving support ([#1525](https://github.com/syscoin/zksync-os-server/issues/1525)) ([d89f354](https://github.com/syscoin/zksync-os-server/commit/d89f354216da4f1f2887e6a496744c7deefa4d84))
* **prover:** expose FlashBoot rental entry points ([18b0fcc](https://github.com/syscoin/zksync-os-server/commit/18b0fcca2c5fabeebd10030f05a8562634f7c497))
* **prover:** expose FlashBoot rental entry points ([d395a1a](https://github.com/syscoin/zksync-os-server/commit/d395a1a58ec6f2783b7059d49cefa90abe9dc59c))
* remove the Compacted state backend ([#1538](https://github.com/syscoin/zksync-os-server/issues/1538)) ([9656632](https://github.com/syscoin/zksync-os-server/commit/9656632e6415c7626585abb3c45d515a6ae4e0a1))
* **rpc:** L1 aggregation-hop proof for L1-settled chains (enable interop on L1) ([#1413](https://github.com/syscoin/zksync-os-server/issues/1413)) ([5974897](https://github.com/syscoin/zksync-os-server/commit/597489700bc45d61437e829305c8aa6517548958))
* **syscoin:** complete production V32 integration ([627f3fc](https://github.com/syscoin/zksync-os-server/commit/627f3fce92af033c9845e100fd1ca3a2172da7e3))
* **syscoin:** integrate V32 on upstream V8 sync ([bc0f29d](https://github.com/syscoin/zksync-os-server/commit/bc0f29d012c6f686d043c6149fef952d131a7e9d))
* **v32:** stage validated Syscoin app and verifier release ([71ae73a](https://github.com/syscoin/zksync-os-server/commit/71ae73afef5cb71ebdac29a09b8ea67db69c6c05))
* **v32:** stage validated Syscoin app and verifier release ([451bb86](https://github.com/syscoin/zksync-os-server/commit/451bb86ad7a18edf969a1a46cc7aec23a68d7fff))
* **v7:** add portable SLH-DSA SHA2 precompile ([ed3c35b](https://github.com/syscoin/zksync-os-server/commit/ed3c35b144a817eb9142fa1e21b8dc205449b49c))
* **v7:** bind portable SLH artifacts ([008820f](https://github.com/syscoin/zksync-os-server/commit/008820f0d79cbb3b99aaa30b7d6bdaff066b14b5))
* **zks:** add stable getBatchByNumber and getLatestBatchNumber RPC ([#1484](https://github.com/syscoin/zksync-os-server/issues/1484)) ([0852ab2](https://github.com/syscoin/zksync-os-server/commit/0852ab276891b7f2dd00af2d2f92b0be74bcc43c))
* zksync-os 0.4.0 ([#1421](https://github.com/syscoin/zksync-os-server/issues/1421)) ([59bb218](https://github.com/syscoin/zksync-os-server/commit/59bb218bc7b0bfe84dc3dca90a3df66a6d49069d))
* zksync-os-revm v0.2.0 ([#1528](https://github.com/syscoin/zksync-os-server/issues/1528)) ([3f6eff0](https://github.com/syscoin/zksync-os-server/commit/3f6eff0343bd08b7930a6ddca0443738825f2275))
* **zksys:** add Sentry membership keeper ([de90db6](https://github.com/syscoin/zksync-os-server/commit/de90db6cb9736d5e94cb870e212ad55ce3cf9878))
* **zksys:** add sync-only keeper bootstrap mode ([d6df631](https://github.com/syscoin/zksync-os-server/commit/d6df631796717d964a11d9a0a6bccb5a839145c4))
* **zksys:** prepare proving service launch ([f796479](https://github.com/syscoin/zksync-os-server/commit/f796479de67246b64369c1f78214c81ca53eec5b))
* **zksys:** sync membership and prepare proving launch ([27d3277](https://github.com/syscoin/zksync-os-server/commit/27d32770dae30f2988baa923ce2c545cb20eaaab))


### Bug Fixes

* align V8 verifier release with security-patched Airbender ([1dd4b24](https://github.com/syscoin/zksync-os-server/commit/1dd4b246c779b1810516898189d7f381c3ebbbfa))
* **batch_verification:** bound cache eviction to cached blocks ([#1502](https://github.com/syscoin/zksync-os-server/issues/1502)) ([418db6e](https://github.com/syscoin/zksync-os-server/commit/418db6e64f1ee9f7dd2abc497b33f74865c937b9))
* **batch_verification:** park instead of crashing when no signatures ([#1509](https://github.com/syscoin/zksync-os-server/issues/1509)) ([f3117c8](https://github.com/syscoin/zksync-os-server/commit/f3117c8560c7ccf8654afed18ab146b8231e1cb1))
* **batcher:** guard edge DA replay fallback ([1dc516b](https://github.com/syscoin/zksync-os-server/commit/1dc516bf77276a61f724496573b993ba47da1973))
* **batcher:** rebuild edge DA refs on replay ([63c0375](https://github.com/syscoin/zksync-os-server/commit/63c0375ac2b7e137c83acab22dae1042051cdf4f))
* **batcher:** rebuild edge DA refs on replay ([b6698d9](https://github.com/syscoin/zksync-os-server/commit/b6698d957df39cb232109253c2ee48f8248bbff1))
* **batcher:** reuse registered DA hashes without locators ([52a45d0](https://github.com/syscoin/zksync-os-server/commit/52a45d0d3a06f1852d90075fb714eba21fe059b0))
* **batcher:** reuse registered DA hashes without locators ([913e8e0](https://github.com/syscoin/zksync-os-server/commit/913e8e0a2419806ce3bcada8d907be0de85cbdc3))
* bind gateway repair journal intent ([71ba16f](https://github.com/syscoin/zksync-os-server/commit/71ba16f0f90dddcef364391edbe351baf43ec348))
* bind verifier release to canonical regenerated guests ([c439840](https://github.com/syscoin/zksync-os-server/commit/c439840f27e3bfba80573358d919cc7da6b071db))
* **blockscout:** forward verifier overrides to remote compose ([6237875](https://github.com/syscoin/zksync-os-server/commit/62378754ed95a0bd8f2789af9fb4f745a3ef7bb3))
* **blockscout:** pin compatible verifier deployment and compiler mirror ([c2b5395](https://github.com/syscoin/zksync-os-server/commit/c2b5395cc6275e8a586d74647fe33d811aa0d5a3))
* **blockscout:** pin compatible verifier deployment and compiler mirror ([21facf7](https://github.com/syscoin/zksync-os-server/commit/21facf7a39bf937a78de0183bc31fc256bdf0107))
* **ci:** align fixture tools and preserve nextest reports ([5487654](https://github.com/syscoin/zksync-os-server/commit/5487654185bd277fbd96da0e6192fd5523b02850))
* **ci:** allow recovery of manually started releases ([eab24b4](https://github.com/syscoin/zksync-os-server/commit/eab24b4ef2c56828d23ff54aeec8960169276a97))
* **ci:** check release scope before runner setup ([f64cb5a](https://github.com/syscoin/zksync-os-server/commit/f64cb5a2bdbc6da6a7f9b08ace48be810db23746))
* **ci:** isolate image releases by tag ([d73acc5](https://github.com/syscoin/zksync-os-server/commit/d73acc5cc24f20e9064d204e8f69b8a792538501))
* **ci:** isolate image workflow concurrency ([1546cb5](https://github.com/syscoin/zksync-os-server/commit/1546cb56a3f797d88818860e33fbb3efc53f21f1))
* **ci:** parse large integration test reports ([b2bbc8b](https://github.com/syscoin/zksync-os-server/commit/b2bbc8bb4c59b344a89e5974b58ee05e76e40cf5))
* **ci:** preserve release binary Linux ABI baseline ([8710291](https://github.com/syscoin/zksync-os-server/commit/871029111c21ddfa1d88b5e9e9b419b1d1c47ae5))
* **ci:** reject incomplete required jobs ([e36a5f2](https://github.com/syscoin/zksync-os-server/commit/e36a5f237a86813b77937cee7ada9a394b5abdf9))
* **ci:** restore V32 component fixture and live integration coverage ([022e653](https://github.com/syscoin/zksync-os-server/commit/022e65375e174b8fc83202ffb07127c98e7f4135))
* **ci:** resume releases from staged artifacts ([06efd86](https://github.com/syscoin/zksync-os-server/commit/06efd86406ea89d51c86d96a8c314871848bb98f))
* **ci:** scope patched workspace paths to steps ([96d7252](https://github.com/syscoin/zksync-os-server/commit/96d725283fc5c27d7e2cafc37f654fad95a34040))
* **ci:** separate fake prover age documentation from list ([f09b806](https://github.com/syscoin/zksync-os-server/commit/f09b806af6f7f1c89692b3dbf5166d14d63363ee))
* **ci:** use git CLI for pinned local dependency imports ([2d47666](https://github.com/syscoin/zksync-os-server/commit/2d47666a20dea9352bf2d9f8451fa23112fc63b8))
* **contracts:** allow Gateway CTM priority limit updates ([#346](https://github.com/syscoin/zksync-os-server/issues/346)) ([923fc96](https://github.com/syscoin/zksync-os-server/commit/923fc96faad9d878f171fce7bec6c7e9b55596dd))
* **contracts:** bound fresh priority admission at 2^24 gas ([80659b6](https://github.com/syscoin/zksync-os-server/commit/80659b67768d6cf9eeebc0c5e0e5e09b086c8732))
* **contracts:** bound fresh priority admission at 2^24 gas ([9accec9](https://github.com/syscoin/zksync-os-server/commit/9accec94efeba985dcbc6f478a612db0104d1786))
* **contracts:** reject priority mode opt-in while deposits are paused ([dfa3123](https://github.com/syscoin/zksync-os-server/commit/dfa3123d47690e7ccb542ff855b12a7a59ce32ca))
* **contracts:** reject priority mode opt-in while deposits are paused ([dbaf526](https://github.com/syscoin/zksync-os-server/commit/dbaf5269a97c0b40bfbab894b470fecc9d302350))
* **da:** authenticate hexadecimal cloud recovery payloads ([4726f87](https://github.com/syscoin/zksync-os-server/commit/4726f87e205a5869250dc3aefc2ce0f6ead1fed8))
* **da:** authenticate recovery bytes before republishing ([ceb5d09](https://github.com/syscoin/zksync-os-server/commit/ceb5d09f0928dbf7e4dc0179b28a5fcdfee03ff9))
* **da:** cap aggregate Gateway forwarded references ([fced68c](https://github.com/syscoin/zksync-os-server/commit/fced68c961b8664438a6c3e55d9cb159e8618dfa))
* **da:** fail closed on PoDA-only finality ([b1efcaa](https://github.com/syscoin/zksync-os-server/commit/b1efcaa25b71834e1e951a10b329fb0bd94c21cd))
* **da:** prime current legacy refs before pruning ([0e6b985](https://github.com/syscoin/zksync-os-server/commit/0e6b985042b177ca59de6dca7a0ceaea431e9732))
* **da:** prime legacy current-command recovery clocks ([a6a9707](https://github.com/syscoin/zksync-os-server/commit/a6a9707a28c2114eefdd674e58c7d54f20410e69))
* **da:** recheck native evidence before recovery spending ([a398754](https://github.com/syscoin/zksync-os-server/commit/a398754c023370a867342f485e789ed8c0f4f74a))
* **da:** reuse canonical publications before paid republication ([a713eb1](https://github.com/syscoin/zksync-os-server/commit/a713eb1a8cc7530b21e28dcc96aa7ea6a1671c13))
* **da:** reuse canonical publications before paying for recovery ([a4d2884](https://github.com/syscoin/zksync-os-server/commit/a4d2884b12349a9057f187540f7ce336fdce0300))
* **da:** wire native reuse into Edge publication mode ([66d36dd](https://github.com/syscoin/zksync-os-server/commit/66d36ddd6de58efea16f4f4e4e1e6f7ca1c6187e))
* **explorer:** deploy token metadata helper and manifest ([a5fc017](https://github.com/syscoin/zksync-os-server/commit/a5fc01758b06a0239fcb355895be51ff7b1055e7))
* **explorer:** publish reviewed zkSYS token icon metadata ([2cbac7a](https://github.com/syscoin/zksync-os-server/commit/2cbac7a7b48f600173b542d34e20219aafb7028f))
* **explorer:** publish reviewed zkSYS token icon metadata ([5797cec](https://github.com/syscoin/zksync-os-server/commit/5797cec98279c30b58a34a99c74178a3580b6757))
* **fri:** lift Airbender degree bound in server ([a796b7c](https://github.com/syscoin/zksync-os-server/commit/a796b7ce72ed62cca82f1baddcd8d767de7f2521))
* **fri:** pin corrected zero-opening prover sources ([058f7bc](https://github.com/syscoin/zksync-os-server/commit/058f7bc4878974249b037dadf325c79dab12cc30))
* **fri:** pin reviewed Airbender fork in server ([437b6f8](https://github.com/syscoin/zksync-os-server/commit/437b6f82d572487dfc706ecbddbfe3f1edee69c2))
* **fri:** pin sound prover stack in v32 server ([ca76234](https://github.com/syscoin/zksync-os-server/commit/ca76234f761b139f247828e3aedfe046a430fe82))
* **gateway-launch:** invoke registry attestor with bash ([954ac6d](https://github.com/syscoin/zksync-os-server/commit/954ac6d2bfa6bb8ebf4f7df408cf8debaf507d54))
* **gateway-launch:** keep cookie credential encoding stable ([8981968](https://github.com/syscoin/zksync-os-server/commit/8981968040f8b427a83c212bb90b528309590789))
* **gateway-launch:** keep cookie credential encoding stable ([121db07](https://github.com/syscoin/zksync-os-server/commit/121db07ef8fc17478bd00d748b6afbe227ebf6cf))
* **gateway-launch:** normalize inputs before direct source refresh ([64665f1](https://github.com/syscoin/zksync-os-server/commit/64665f1139eaab0ec9874dfe99854188fd0f7eab))
* **gateway-launch:** normalize issuance chain ID aliases ([cb1782b](https://github.com/syscoin/zksync-os-server/commit/cb1782bfb5dd37bf55990a62b136b9ab05043202))
* **gateway-launch:** normalize modes before Era postimage selection ([5fdc41c](https://github.com/syscoin/zksync-os-server/commit/5fdc41cb103d417a551b063dc41f60b464fedf3c))
* **gateway-launch:** stabilize registry helper extraction ([381b2b5](https://github.com/syscoin/zksync-os-server/commit/381b2b5c111c30bd484aad6f6328dd3ce3cd7f5f))
* **gateway-launch:** unblock canonical zkSYS bootstrap ([62b1580](https://github.com/syscoin/zksync-os-server/commit/62b15803112ab67173012bd1910e62f2f6826042))
* **gateway:** attest generated V32 inputs ([88fe378](https://github.com/syscoin/zksync-os-server/commit/88fe378029f6fa66147659cbefd3c7ee6b9b870b))
* **gateway:** avoid duplicate Forge slow flag ([e41524f](https://github.com/syscoin/zksync-os-server/commit/e41524f2f17af0714ac7d557cf9d2ada77036777))
* **gateway:** bind canonical relay artifact ([c933c93](https://github.com/syscoin/zksync-os-server/commit/c933c93203d8fea1c34eedcffca69689d0edc00e))
* **gateway:** bind migration artifact to live edge ([7b46240](https://github.com/syscoin/zksync-os-server/commit/7b46240bb4323540dad24c9ae5c35cf7b4f802b7))
* **gateway:** bind migration to checkpoint owner ([c3cc5f2](https://github.com/syscoin/zksync-os-server/commit/c3cc5f2108b91a1d8294fa98aefe8ba7bc6f31aa))
* **gateway:** bind settlement RPC to L1 identity ([83a275b](https://github.com/syscoin/zksync-os-server/commit/83a275b9b9d734db37b9e8acbec4f3bd471cabb1))
* **gateway:** bind zkstack build artifact ([c4ef2ce](https://github.com/syscoin/zksync-os-server/commit/c4ef2ceca47f25713bf393e87391f064f0b03348))
* **gateway:** bind zkstack stamp to binary ([3d92135](https://github.com/syscoin/zksync-os-server/commit/3d9213591045a74196c83cecfb3f6bf9ffdabe34))
* **gateway:** bound migration lifecycle waits ([1278170](https://github.com/syscoin/zksync-os-server/commit/1278170780d62ba1c686a38743eae198bc57c030))
* **gateway:** bound migration repair diagnostics ([61dd55a](https://github.com/syscoin/zksync-os-server/commit/61dd55ade3b983db60114a39fff3ca558bf85c3c))
* **gateway:** canonicalize zkstack build fingerprint ([737f175](https://github.com/syscoin/zksync-os-server/commit/737f175f0b2b5f3ef230978780ea0403c878cecf))
* **gateway:** fail closed on ecosystem recovery ([e18cc5a](https://github.com/syscoin/zksync-os-server/commit/e18cc5a951012d745475d84e91b03839eb606f61))
* **gateway:** freeze settlement dependency install ([f065565](https://github.com/syscoin/zksync-os-server/commit/f065565833aa5fc09df25db1f13f53cd4167f977))
* **gateway:** harden execute operator nonce lock ([0ac611c](https://github.com/syscoin/zksync-os-server/commit/0ac611cd000e39a0f6cad6c6ca14487df045e804))
* **gateway:** harden live launch and multi-edge preflight ([d66db0f](https://github.com/syscoin/zksync-os-server/commit/d66db0f5bcc30b77f1bd8735b7d016be0368d6c7))
* **gateway:** harden V32 chain-init inputs ([7a5ecc6](https://github.com/syscoin/zksync-os-server/commit/7a5ecc6af102fd35277c0981a9792bc46031b469))
* **gateway:** harden V32 launch recovery ([86f6238](https://github.com/syscoin/zksync-os-server/commit/86f62381e74fee06a9030286d54b547d53801ffc))
* **gateway:** include priority fee in provisioning ([8229915](https://github.com/syscoin/zksync-os-server/commit/8229915e345b81ac3ab3430b5d00f46451075231))
* **gateway:** isolate bootstrap reads from L1 gas overrides ([d27071a](https://github.com/syscoin/zksync-os-server/commit/d27071aea5b3baa8a3dfd610ea9e303538267177))
* **gateway:** isolate non-L1 cast context ([4765256](https://github.com/syscoin/zksync-os-server/commit/4765256f7515c2695f1548e8e45e83cc6b9c8ad1))
* **gateway:** keep materialized RPC config canonical ([c6e11a6](https://github.com/syscoin/zksync-os-server/commit/c6e11a6a6226b45bfafc0b7005253f8f49cc5e92))
* **gateway:** make migration repair replay-safe ([1f6ebcd](https://github.com/syscoin/zksync-os-server/commit/1f6ebcdde12bdecbda136873fd69dbda4a61b277))
* **gateway:** match cleanup config exactly ([cb648dd](https://github.com/syscoin/zksync-os-server/commit/cb648ddc21c6c5801f158dbda3326bca98f57a2e))
* **gateway:** normalize ecosystem path before checkpoints ([576e2c0](https://github.com/syscoin/zksync-os-server/commit/576e2c02faef145e2034ce58b9a85ae103ec63dc))
* **gateway:** pin wallet helper trust root ([bdea521](https://github.com/syscoin/zksync-os-server/commit/bdea521f6537f75a8fe834b155a5fb162305c6f5))
* **gateway:** preflight settlement provisioning gas ([e60fcca](https://github.com/syscoin/zksync-os-server/commit/e60fccaa5a1c636590a3d1bfd26d29ab45ab10f5))
* **gateway:** preserve compact DA scheme during migration ([b5a2451](https://github.com/syscoin/zksync-os-server/commit/b5a2451be329ab0c9f536e797cf7bfad6f32a54a))
* **gateway:** preserve external governors for deposit management ([151b4bb](https://github.com/syscoin/zksync-os-server/commit/151b4bbfede16624a83588512d5f1f5345885b86))
* **gateway:** preserve pending edge listener reservation ([27c68e7](https://github.com/syscoin/zksync-os-server/commit/27c68e72d0aa6d37b2442173e2e77be9bcbf6158))
* **gateway:** preserve protected accounts through migration ([1601dd2](https://github.com/syscoin/zksync-os-server/commit/1601dd2e118cf982b26a497c660933de65976113))
* **gateway:** preserve sealed resume workspace ([32fb487](https://github.com/syscoin/zksync-os-server/commit/32fb487a58873cf5df67b275559374c5aafc016e))
* **gateway:** preserve terminal deployment state and empty SSH arguments ([a47fcb9](https://github.com/syscoin/zksync-os-server/commit/a47fcb9c412609564c2c5b693ccb97ea0350956a))
* **gateway:** preserve terminal deployment state and empty SSH arguments ([d5378ba](https://github.com/syscoin/zksync-os-server/commit/d5378bac795fae1af32f7f0d18c20dc1e1a9f813))
* **gateway:** preserve zkstack address scalars ([b92b83b](https://github.com/syscoin/zksync-os-server/commit/b92b83b072d87baeea45dd44330feb99cbcb8935))
* **gateway:** preserve zkstack contract scalars ([a33ff4a](https://github.com/syscoin/zksync-os-server/commit/a33ff4af01802520ab9c2ff919cc0babbafe6c4b))
* **gateway:** recover exact post-admin edge init ([3daad1d](https://github.com/syscoin/zksync-os-server/commit/3daad1d30076c79e91cc025c787101e913f9f390))
* **gateway:** recover interrupted V32 ownership handoffs ([77cb028](https://github.com/syscoin/zksync-os-server/commit/77cb028bd91cbd1948b3e676e27478e68d771c9b))
* **gateway:** redact migration RPC diagnostics ([a284822](https://github.com/syscoin/zksync-os-server/commit/a28482230f819cff2365dd41500cd4271ecfcd28))
* **gateway:** reject unsupported canonical relay emissions ([2db4b38](https://github.com/syscoin/zksync-os-server/commit/2db4b3830f8820528ec1a2289e25a1621613b6bb))
* **gateway:** reject unsupported canonical relay emissions ([325c1b5](https://github.com/syscoin/zksync-os-server/commit/325c1b5dff736ed385161c5d8b78581245685759))
* **gateway:** repair V32 DA pair command ([96501e2](https://github.com/syscoin/zksync-os-server/commit/96501e237208a9009de48e7a44b29f21a5674ff1))
* **gateway:** repin forked Tanenbaum candidate ([5a57382](https://github.com/syscoin/zksync-os-server/commit/5a5738284a6d1606d04ec57f8ad99eb50cd79aa9))
* **gateway:** restore execute sender reserve ([ab5d7e8](https://github.com/syscoin/zksync-os-server/commit/ab5d7e8037c75859767cb84e8135ed25da16509d))
* **gateway:** resume attested source across rebuilds ([b36c000](https://github.com/syscoin/zksync-os-server/commit/b36c0000086b43658403e10d6fac4432ba6926f6))
* **gateway:** resume created-only edge checkpoint ([b710012](https://github.com/syscoin/zksync-os-server/commit/b71001240e0f431f67106a2d454f5b96f3c05968))
* **gateway:** resume validated V32 migration ([a65960c](https://github.com/syscoin/zksync-os-server/commit/a65960c285aa0078909a81a4138627bc37e1fb13))
* **gateway:** resume validated V32 migration ([62ce03c](https://github.com/syscoin/zksync-os-server/commit/62ce03c6d59744b9b83a97de1f7d110d5eff042e))
* **gateway:** retire late-success repair journals ([97b0a8b](https://github.com/syscoin/zksync-os-server/commit/97b0a8b2a705b846c43099f304af72982a9f7af3))
* **gateway:** route edge admin repairs through Gateway ([a6f4ff3](https://github.com/syscoin/zksync-os-server/commit/a6f4ff3ed536192e27c6166b766cf4cab44301a1))
* **gateway:** secure migration artifact creation ([7ced786](https://github.com/syscoin/zksync-os-server/commit/7ced786c04bd0f9b9e321b9df709bdd7944e2e86))
* **gateway:** select generated governor signer ([f3f8f34](https://github.com/syscoin/zksync-os-server/commit/f3f8f34fc6e610ff64f554fd1f9faa07cc952686))
* **gateway:** serialize external ownership recovery ([d91553b](https://github.com/syscoin/zksync-os-server/commit/d91553b1dbb8b4ab0dfef9df04997b96a8c16a51))
* **gateway:** supervise created-only repair process tree ([eb09dc2](https://github.com/syscoin/zksync-os-server/commit/eb09dc27b74aa177d71d11178fee6c7124492f68))
* **gateway:** supervise repair validator groups ([2bfcc10](https://github.com/syscoin/zksync-os-server/commit/2bfcc10d404424eba2d8292972d35c0a5d24da15))
* **gateway:** support protected accounts through migration ([d554591](https://github.com/syscoin/zksync-os-server/commit/d554591a04f95347d3a2ec2930d54ac409fd74b6))
* **gateway:** use V32 cut-data migration selector ([99c9720](https://github.com/syscoin/zksync-os-server/commit/99c9720a899228392775f0eee84e128bc5b4bee4))
* **gateway:** validate forge inspect bytecode ([3622b80](https://github.com/syscoin/zksync-os-server/commit/3622b80391ff6d2b5b5ce9d0bfd0627056bbeb55))
* **gateway:** verify completed migration on replay ([0bfb6bc](https://github.com/syscoin/zksync-os-server/commit/0bfb6bcec563151dc1be3a36363e3100b98e46e4))
* **gateway:** wait for replayed runtime before attestation ([2136b9a](https://github.com/syscoin/zksync-os-server/commit/2136b9a0993be9a3dc09dc1ca11216cf789130a9))
* **genesis:** authenticate archive L1 identity ([5fdb9d0](https://github.com/syscoin/zksync-os-server/commit/5fdb9d0c900b21e5ade1e2ad09f9476940774b36))
* **genesis:** bind archive history to live L1 ([3a7da47](https://github.com/syscoin/zksync-os-server/commit/3a7da4771187bb77c2d532e075ffc9f2c1325137))
* **genesis:** bind sentinel upgrade event history ([98ae6de](https://github.com/syscoin/zksync-os-server/commit/98ae6def80d26a8760d310f216858eca2f235ac7))
* **genesis:** use archive provider for historical L1 reads ([6a98055](https://github.com/syscoin/zksync-os-server/commit/6a980550d86dae8b0807a39754826a9a0821d0a3))
* **genesis:** use archive provider for historical L1 reads ([790be13](https://github.com/syscoin/zksync-os-server/commit/790be134eae63cf079072762dc7ee8b3fd289b69))
* harden checkpointed Gateway launch ([356e39b](https://github.com/syscoin/zksync-os-server/commit/356e39b71342ecdff832b4385d99016f26c3f8b4))
* **interop:** reject stale committed ranges ([83c0c65](https://github.com/syscoin/zksync-os-server/commit/83c0c65cf563de7c5a02c4ff35f4394a3170a8ee))
* **keygen:** align candidate wrapper with patched prover sources ([d765c5d](https://github.com/syscoin/zksync-os-server/commit/d765c5d7244aa10d774708da5743de0f8776860f))
* **keygen:** consume the pinned common crypto fork ([#345](https://github.com/syscoin/zksync-os-server/issues/345)) ([7d8cc92](https://github.com/syscoin/zksync-os-server/commit/7d8cc926412d0e9d03b19958dee1a9bc5f095668))
* **keygen:** pin bytecode-free release tooling ([07857d3](https://github.com/syscoin/zksync-os-server/commit/07857d349d5f61794b7782e5eb57164512e9bd0a))
* **keygen:** reuse filtered pinned-source checkouts ([b37c1a5](https://github.com/syscoin/zksync-os-server/commit/b37c1a52b127331fd9bd7f955742cedad735c1ff))
* **keygen:** reuse filtered pinned-source checkouts ([9203166](https://github.com/syscoin/zksync-os-server/commit/9203166569dd14f8decbd4ed33e6dc8e696a1534))
* **keygen:** transfer proving source closure to attestation ([c7ec78f](https://github.com/syscoin/zksync-os-server/commit/c7ec78f989c3c7e7492e72ecd9502856ebc602d2))
* **l1_sender:** apply fee caps when rebroadcasting ([3e0de83](https://github.com/syscoin/zksync-os-server/commit/3e0de8363a74ebc7c2016c9d32ef180bd0f39441))
* **l1_sender:** backpressure capped replacements ([e944851](https://github.com/syscoin/zksync-os-server/commit/e94485133bc0f18e8aa9fef71737d1aa92b73482))
* **l1_sender:** cap simulation gas during v0.22.0 sync ([8fb97ff](https://github.com/syscoin/zksync-os-server/commit/8fb97ff536860e61aa39a8f4d1799b23b53f0518))
* **l1_sender:** capture confirmed nonce before waiting for commands ([#1543](https://github.com/syscoin/zksync-os-server/issues/1543)) ([ef9ca49](https://github.com/syscoin/zksync-os-server/commit/ef9ca4927ba4c2fc1a5bfc1a78d52ec28288f291))
* **l1_sender:** check exact nonce before reorg resend ([ed05a80](https://github.com/syscoin/zksync-os-server/commit/ed05a802876ba9d57df5605fa570bc82ccbd8c1e))
* **l1_sender:** drain passthroughs before nonce failure ([48a818a](https://github.com/syscoin/zksync-os-server/commit/48a818af167f7f335c3df3dc330880616281b1eb))
* **l1_sender:** group confirmation progress channels ([50ec646](https://github.com/syscoin/zksync-os-server/commit/50ec646f2346cf4f3cd47359594b30f4611b7f74))
* **l1_sender:** pad fallback gas estimate ([b877ec9](https://github.com/syscoin/zksync-os-server/commit/b877ec9be4efbca78f5808937f9925973824a5ff))
* **l1_sender:** preserve stop-and-wait recovery fallback ([9e605b4](https://github.com/syscoin/zksync-os-server/commit/9e605b4f6349e114517087e92335d0027b196b2d))
* **l1_sender:** refresh idle Gateway context and cap replacement fees ([aaa92a3](https://github.com/syscoin/zksync-os-server/commit/aaa92a3b569b1f5aa1e2145bbbc10f5f30210afb))
* **l1_sender:** reject state-dependent estimate fallback ([afbde7f](https://github.com/syscoin/zksync-os-server/commit/afbde7fa72e631b6e9f64362ba86e516b1af4d43))
* **l1_sender:** respect settlement block gas limit ([8ad4ee9](https://github.com/syscoin/zksync-os-server/commit/8ad4ee97e67006c313eb4f23f03a272226b223e7))
* **l1_sender:** restore safe single-tx gas fallback ([24969fb](https://github.com/syscoin/zksync-os-server/commit/24969fbcec63a7d6e583c91be807454a69b9e186))
* **l1_sender:** restore safe single-tx gas fallback ([f3ac3e6](https://github.com/syscoin/zksync-os-server/commit/f3ac3e675ac9f49bc86b7e6aa6f6d9308689e589))
* **l1_sender:** retain original prefix fallback guard ([e6d0506](https://github.com/syscoin/zksync-os-server/commit/e6d0506e0204ddf3e5f7c6915498fbd324aaaca8))
* **l1_sender:** satisfy clippy condition checks ([be9ba58](https://github.com/syscoin/zksync-os-server/commit/be9ba5819c55f4c13979d25a43f7fd539f73d1bc))
* **l1_sender:** validate recovered transaction suffixes ([695afb3](https://github.com/syscoin/zksync-os-server/commit/695afb37b6ffa66dcb3b23c0bad23f71f24b1ac0))
* **l1_sender:** wait for confirmed simulation predecessors ([2810647](https://github.com/syscoin/zksync-os-server/commit/28106470a409c054aeff3c4968b9f4da9ae4989b))
* **l1_watcher:** recognize pre-v31 supplier getter reverts ([#1523](https://github.com/syscoin/zksync-os-server/issues/1523)) ([5a51cb4](https://github.com/syscoin/zksync-os-server/commit/5a51cb45ae44f01406b6ead84d005e0ae4af5447))
* **l1-sender:** fail closed on uncertain recovery ([70a9667](https://github.com/syscoin/zksync-os-server/commit/70a966767c88856b854e641172ac7fc6b8caff9e))
* **l1-sender:** recover orphaned confirmation transactions ([c977383](https://github.com/syscoin/zksync-os-server/commit/c9773838d6c1763126883a61cbe48e582caffbb7))
* **l1-sender:** recover orphaned confirmation transactions ([6fd8bca](https://github.com/syscoin/zksync-os-server/commit/6fd8bca6c7a273815fabd0266faae460d670a543))
* **l1-sender:** retain signed bytes across confirmation ([7e113ee](https://github.com/syscoin/zksync-os-server/commit/7e113eece3eea5dcbd4e3b64976ec02599009921))
* **l1-sender:** tolerate transient receipt headers ([ac138d8](https://github.com/syscoin/zksync-os-server/commit/ac138d89dd2c40ca1dccc651c2c8cba9169e6fd0))
* **launch:** add explicit confirmed core journal recovery ([99d6648](https://github.com/syscoin/zksync-os-server/commit/99d6648c76f64486a86bd0f0f681abba53217a69))
* **launch:** anchor issuance to token deployment receipt ([531978f](https://github.com/syscoin/zksync-os-server/commit/531978fa1d3b03d4bf951443f08163628864c396))
* **launch:** attest gas tank before token prelude sends ([b3832a8](https://github.com/syscoin/zksync-os-server/commit/b3832a89d8ba7b610c651c7485db53e7e3d4f51c))
* **launch:** authenticate address-only administrator accounts ([ab9a05d](https://github.com/syscoin/zksync-os-server/commit/ab9a05dda919744cadd6f4a10bb0f71f43b4439d))
* **launch:** bind every gateway conversion signer ([09d7d68](https://github.com/syscoin/zksync-os-server/commit/09d7d6802d9a94f72a8d1849f9d789cfeaabed1a))
* **launch:** bind external Forge script sender ([f638db9](https://github.com/syscoin/zksync-os-server/commit/f638db92e5c5ea31507e089b61f122fd95cf9083))
* **launch:** clear exact self-pending direct ownership ([f335e98](https://github.com/syscoin/zksync-os-server/commit/f335e98301e773b68083fce79f14c52f54f0dcf7))
* **launch:** publish recovered core config exclusively ([d8d5cc2](https://github.com/syscoin/zksync-os-server/commit/d8d5cc2fbb8db13109d68897431e610b59788491))
* **launch:** support encrypted v32 admins and receipt-anchored issuance ([b022302](https://github.com/syscoin/zksync-os-server/commit/b0223023170c71eb0bc751281ed7b1ecbdeff2e0))
* **loadbase:** prevent nonce gaps and balance overcommit ([4cc4a95](https://github.com/syscoin/zksync-os-server/commit/4cc4a958a05bdc84b7acf1399b3f945777275823))
* **loadbase:** prevent nonce gaps and balance overcommit ([0df56a8](https://github.com/syscoin/zksync-os-server/commit/0df56a81a6139c1836c69ec3f96dae1297f707ff))
* **loadbase:** rebalance queued wallet capacity ([16b0392](https://github.com/syscoin/zksync-os-server/commit/16b0392a5ee195787e69f9ae288048678eb845b2))
* **loadbase:** retry transient gas-price failures ([adccd76](https://github.com/syscoin/zksync-os-server/commit/adccd76441b347fd2d5e04a8ff773570113f4616))
* make zkstack build timestamp reproducible ([62f5f13](https://github.com/syscoin/zksync-os-server/commit/62f5f13d7ca93ffa20d3bfcaa83012c4d65b5291))
* **merkle_tree:** handle empty batch proofs ([#1535](https://github.com/syscoin/zksync-os-server/issues/1535)) ([0b97156](https://github.com/syscoin/zksync-os-server/commit/0b97156fe230853a42bb15ddc1a11e9c2116ff9b))
* **network:** admit trusted incoming zks_2fa peers ([4a14e52](https://github.com/syscoin/zksync-os-server/commit/4a14e520029490d15cadfca8810e94cc2e14046a))
* **network:** bound replay override requests ([58f528b](https://github.com/syscoin/zksync-os-server/commit/58f528bc483894e3451f685629d00c28f5f1355a))
* **network:** bound replay override requests ([caea659](https://github.com/syscoin/zksync-os-server/commit/caea659e4e6ab52b609c9a59610c3e60fd7a229e))
* **network:** disconnect and re-request when the EN replay stream stalls ([#1513](https://github.com/syscoin/zksync-os-server/issues/1513)) ([3319561](https://github.com/syscoin/zksync-os-server/commit/331956187080e35cb3be3f237510d087d9f3e041))
* **network:** preserve trusted zks_2fa connectivity ([b959fda](https://github.com/syscoin/zksync-os-server/commit/b959fda523930944db6bcf896414d6a67248aa1c))
* **network:** reset replay timeout only on progress ([2e3e430](https://github.com/syscoin/zksync-os-server/commit/2e3e430ba71da2b89d937f603daa8ed67c30510e))
* normalize gateway inventory chain id ([adcdeed](https://github.com/syscoin/zksync-os-server/commit/adcdeed36ec328641eeb2f733a476ae3dd02ca2c))
* **ops:** recheck block idleness before heartbeat send ([e69000a](https://github.com/syscoin/zksync-os-server/commit/e69000aa5bfd79afbf789d5269e84ed030550180))
* **pali:** harden smart account validator boundaries ([c58046e](https://github.com/syscoin/zksync-os-server/commit/c58046ee32f47167bbf5c51b1c3c63fe65664d5a))
* **pali:** repin deployment verification scripts ([f6f2fa9](https://github.com/syscoin/zksync-os-server/commit/f6f2fa9bb630891bd59f5c66be8459c2854a9b76))
* pin verifier release to patched prover graph ([a9c1a19](https://github.com/syscoin/zksync-os-server/commit/a9c1a19d92bea99ac77cec8c255505a7a68025eb))
* **pipeline:** make graceful shutdown race-free ([#1521](https://github.com/syscoin/zksync-os-server/issues/1521)) ([4578a8c](https://github.com/syscoin/zksync-os-server/commit/4578a8c898b4dd6772c44eee5815cd0f8e4560d6))
* **pipeline:** preserve backpressure while waiting for downstream capacity ([81d3b14](https://github.com/syscoin/zksync-os-server/commit/81d3b148e9ba8f1530f61a9cebe0004141341af2))
* **pipeline:** preserve bounded backpressure during DA finality waits ([1d3ae5f](https://github.com/syscoin/zksync-os-server/commit/1d3ae5f548d23dc2f44d894a6176571b2163d6f6))
* preserve bounded DA retries and restart receipt recognition ([a31070f](https://github.com/syscoin/zksync-os-server/commit/a31070f6da7e280bf4fd6ef703bae0ec4a519be6))
* preserve bounded DA retries and restart receipt recognition ([4627f50](https://github.com/syscoin/zksync-os-server/commit/4627f50b9431f959d26d34f71b8ee2b5f774e3d1))
* **protocol:** gate v32 and route proofs by settlement ([805ef87](https://github.com/syscoin/zksync-os-server/commit/805ef8703ce6cee78b0f669f4875b1f07f7933fe))
* **prover_api:** measure the payload transfer in pick_job_latency ([#1526](https://github.com/syscoin/zksync-os-server/issues/1526)) ([e9efb14](https://github.com/syscoin/zksync-os-server/commit/e9efb145facf008ccdbe050bfbffa842a915024d))
* **prover_api:** reserve capacity before fake SNARK assignment ([#1522](https://github.com/syscoin/zksync-os-server/issues/1522)) ([8f6f645](https://github.com/syscoin/zksync-os-server/commit/8f6f6457fb77743ba4785e2b77a4175365cc72cd))
* **prover_api:** widen prover histogram buckets so quantiles stop clipping ([#1505](https://github.com/syscoin/zksync-os-server/issues/1505)) ([defc056](https://github.com/syscoin/zksync-os-server/commit/defc056477786c58db89f38b0921a3fa443f20be))
* **prover_service:** continue dispatching after reward quota ([60a65b6](https://github.com/syscoin/zksync-os-server/commit/60a65b68b5b2f35e5545fa5ad437994ac968fa79))
* **prover_service:** keep service dispatch continuous ([036be84](https://github.com/syscoin/zksync-os-server/commit/036be84d52213e5e94918d9be4278832001f3a61))
* **prover_service:** preserve accepted report idempotency ([df29623](https://github.com/syscoin/zksync-os-server/commit/df296233c4b47c91ce8bca8024db381bbbd5a06a))
* **prover_service:** preserve cross-period settlement and recovery ([0fd54c8](https://github.com/syscoin/zksync-os-server/commit/0fd54c897b62b5feef1e437f1fb1e2a6485a1574))
* **prover_service:** preserve live credit across grace expiry ([1b4e9a5](https://github.com/syscoin/zksync-os-server/commit/1b4e9a5026f0bc20961673af5abbdb56730b6fa0))
* **prover_service:** remove lifetime dispatch journal limits ([882ed68](https://github.com/syscoin/zksync-os-server/commit/882ed683fb4fb07b02cd8c8f611093f0683798e5))
* **prover_service:** retain prestart bootstrap control journal ([b4673b8](https://github.com/syscoin/zksync-os-server/commit/b4673b86aa27d9205d6745f80bd681d728a13af1))
* **prover-service:** authenticate canonical account enrollment ([613fef4](https://github.com/syscoin/zksync-os-server/commit/613fef47f3666ee3536291528da2ec9b2f34b244))
* **prover-service:** relay settled Gateway roots ([b4c6b5e](https://github.com/syscoin/zksync-os-server/commit/b4c6b5e997c1f2b7e417cc52ac392e9616fe47a5))
* **prover-service:** require enrollment before live work ([afbbb76](https://github.com/syscoin/zksync-os-server/commit/afbbb76296937b88acc41c8a734f07ab9eec9a0c))
* **prover-service:** retire canonical native proof rejections ([c545873](https://github.com/syscoin/zksync-os-server/commit/c54587349f1662dc729edcdb09a52e7fa518b5d4))
* **prover-service:** retire canonical native proof rejections ([cd1cd82](https://github.com/syscoin/zksync-os-server/commit/cd1cd8286a36a31edc2901c632fcb18d9c294069))
* **prover-service:** reuse enrollment for package calls ([69159e0](https://github.com/syscoin/zksync-os-server/commit/69159e0b3235b4093915efc04fd08890c5268d2f))
* **prover-service:** reuse verified enrollment in audits ([bbeffe9](https://github.com/syscoin/zksync-os-server/commit/bbeffe921463aa8f8822538f541cb493e8cd2dfb))
* **prover:** align V8 verifier graph and fixtures ([2f66eaa](https://github.com/syscoin/zksync-os-server/commit/2f66eaa3d0fe103ffb52bd5d756ca4430334afc8))
* **prover:** bound retained witness memory with backpressure ([65658d1](https://github.com/syscoin/zksync-os-server/commit/65658d11f3e4171522ca9b50df55f42b4b1cb9e0))
* **prover:** cross-check native batches against canonical state ([b6c15b0](https://github.com/syscoin/zksync-os-server/commit/b6c15b03242338d323ad7682a569396a36c8859f))
* **prover:** harden public proxy submission bounds ([99657e9](https://github.com/syscoin/zksync-os-server/commit/99657e97a180858708d956bd40ea7ebfb6d9757e))
* **prover:** negotiate bounded 512 MiB SNARK pick responses ([296a8ab](https://github.com/syscoin/zksync-os-server/commit/296a8abb80a19984748a9ce1ac580b61dbb14609))
* **prover:** negotiate bounded SNARK pick response capacity ([728ef37](https://github.com/syscoin/zksync-os-server/commit/728ef37244a6d2fc43084a56202d049afea42cfd))
* **prover:** pin GPU artifact consistency fix in v32 ([d2158f1](https://github.com/syscoin/zksync-os-server/commit/d2158f1bd7e5bab601e3361f2b92ac688d2d6fc7))
* **prover:** pin sequencer in every service configuration ([75b541b](https://github.com/syscoin/zksync-os-server/commit/75b541b7720b500e5cafe751cab862d3589d1117))
* **prover:** preserve service consent and checkpoint recovery ([8a38522](https://github.com/syscoin/zksync-os-server/commit/8a38522e5f4408ad3e882732c21bfbff306a373d))
* **prover:** recover interrupted coordinator pick initialization ([4833f93](https://github.com/syscoin/zksync-os-server/commit/4833f93c4877bc838aaee40891e89c25daedc9e1))
* **prover:** reject partial fake proving topology ([d4a615f](https://github.com/syscoin/zksync-os-server/commit/d4a615f96b59235b93607a07169a3df5a3687514))
* **prover:** validate full replay output across compact staging ([82aaece](https://github.com/syscoin/zksync-os-server/commit/82aaeced8516caaaf863e642309e598f89a70569))
* **raft:** gate proposals on confirmed replay frontier ([87c25cd](https://github.com/syscoin/zksync-os-server/commit/87c25cd04bfd97ebbabfe7156423ce71c0336c20))
* **recovery:** authenticate replay and Raft rebuild durability ([97ec554](https://github.com/syscoin/zksync-os-server/commit/97ec554fa8897fc050d4322f53e9232c980cbbc5))
* **recovery:** authenticate replay and Raft rebuild durability ([20bced6](https://github.com/syscoin/zksync-os-server/commit/20bced6aa7d744f8d2ae9cbd8f50f3401c0dc141))
* refresh idle Gateway context and cap L1 fees ([c761084](https://github.com/syscoin/zksync-os-server/commit/c761084c3ad9da3802b973dcc4e85080172ff27d))
* regenerate v32 verifier for canonical query bits ([6bd2ca0](https://github.com/syscoin/zksync-os-server/commit/6bd2ca0ad123ff60aa6956566edf981951653948))
* reject noncanonical zkstack chain names ([9e13367](https://github.com/syscoin/zksync-os-server/commit/9e1336753a4c798d736c3546bd7b2e521dd1b940))
* **release:** allow independent release with scoped smoke ([80f3978](https://github.com/syscoin/zksync-os-server/commit/80f39780a24721b80da5a1a572273f869db472b1))
* **release:** export stock launcher inventory bindings ([3bc2c67](https://github.com/syscoin/zksync-os-server/commit/3bc2c673117eefea53e7ad6fbe209580276b80cf))
* **release:** prepare stamped Edge lifecycle artifacts ([74f5307](https://github.com/syscoin/zksync-os-server/commit/74f5307595ecc957df0bff131d40bf8d97e6269f))
* **release:** reject late inventory races and sanitize parsing ([87d5a5c](https://github.com/syscoin/zksync-os-server/commit/87d5a5cec90bfef41d72b5bee66220cfc687930f))
* **release:** repair stale Gateway artifacts before Edge ([fc78374](https://github.com/syscoin/zksync-os-server/commit/fc78374e4434e626fafdc03ba30f21789a529b42))
* **release:** require complete main-node and EN inventory ([aad4592](https://github.com/syscoin/zksync-os-server/commit/aad4592db621ef913e1968e4242db1140496c16b))
* **release:** require complete server and EN inventory ([6c2d4d7](https://github.com/syscoin/zksync-os-server/commit/6c2d4d7e6bab05643feab6dab72c57741633bdb6))
* **replay_archive:** bind gate to published object ([acfd784](https://github.com/syscoin/zksync-os-server/commit/acfd784f8184d33479f660b0950da5153ef2bbdc))
* **replay_archive:** bound ungated receipts ([861a752](https://github.com/syscoin/zksync-os-server/commit/861a75205eeea5263cfa205a51acf9aa1d6daa63))
* **replay_archive:** harden session lifecycle ([b45d091](https://github.com/syscoin/zksync-os-server/commit/b45d0919da1db2f6f1cb4a1eb12e5a5acdbf168b))
* **replay_archive:** isolate recovery temp names ([4562dc0](https://github.com/syscoin/zksync-os-server/commit/4562dc0d678cab255c47e227a88c79999bc2def2))
* **replay_archive:** isolate writer sessions ([83778be](https://github.com/syscoin/zksync-os-server/commit/83778be064589fcc28061c74c758154f23d59c7b))
* **replay_archive:** isolate writer sessions ([76f6f9e](https://github.com/syscoin/zksync-os-server/commit/76f6f9e5ba73e70103fff03bea60fbabf988726b))
* **replay_archive:** preserve recovery session names ([e4283b0](https://github.com/syscoin/zksync-os-server/commit/e4283b0514da600e6a89f2bee2641b3d2527b806))
* **replay_archive:** reject noncanonical archive keys ([f3ad02c](https://github.com/syscoin/zksync-os-server/commit/f3ad02cdb126076834812a26de6e444074637d30))
* **replay_archive:** retain queued retry provenance ([60b1bc1](https://github.com/syscoin/zksync-os-server/commit/60b1bc1549f53820ab6047ce04415ecfa14fef03))
* **replay_archive:** retain startup provenance ([5cfe4d6](https://github.com/syscoin/zksync-os-server/commit/5cfe4d659a3a6bc87057a48dd43c5560a7ce17d0))
* **replay_archive:** support legacy WAL tips ([ff0cc4d](https://github.com/syscoin/zksync-os-server/commit/ff0cc4d2871cf2fabeed95010af240271eac565d))
* **replay_archive:** verify conditional upload retries ([de824b0](https://github.com/syscoin/zksync-os-server/commit/de824b00df94baf4b56c4dc960fbec91581c9417))
* **rpc:** bound capture and enforce public policy provenance ([27a6b85](https://github.com/syscoin/zksync-os-server/commit/27a6b85997381ace29de41c3a6953ad07a171ede))
* **rpc:** bound simulation work through worker completion ([89e9afe](https://github.com/syscoin/zksync-os-server/commit/89e9afe2897c82479aed35462878d3df1e3b5f79))
* **rpc:** bound simulation work through worker completion ([d5c77af](https://github.com/syscoin/zksync-os-server/commit/d5c77af9515b07c0c3939bfec3f10d91e51b8336))
* **rpc:** cap gas estimates by transaction fee cap ([488647c](https://github.com/syscoin/zksync-os-server/commit/488647c88dbb7aa027c73af80f0e0047c60afb1a))
* **rpc:** cap gas estimates by transaction fee cap ([f9548fa](https://github.com/syscoin/zksync-os-server/commit/f9548fa70c58c586cadd321c78e38b398181c827))
* **rpc:** charge simulation admission for tiny-gas calls ([d867fc1](https://github.com/syscoin/zksync-os-server/commit/d867fc1553bcda279d6e1e8e96558d0c1b41a1ef))
* **rpc:** gate transaction filling with the shared VM budget ([e25b9e9](https://github.com/syscoin/zksync-os-server/commit/e25b9e93a58ba81b0aac1e80c595b429cb0f97c6))
* **rpc:** gate transaction filling with the shared VM budget ([30d00cd](https://github.com/syscoin/zksync-os-server/commit/30d00cdccfb231f4d7a14c167ff4ac49c2a45e8e))
* **rpc:** keep bounded filter polls progressing ([9a8cb2d](https://github.com/syscoin/zksync-os-server/commit/9a8cb2df73ec4b93fc48c64e1398674f282d8788))
* **rpc:** preserve filter cursors and idle polling ([3ab2e3c](https://github.com/syscoin/zksync-os-server/commit/3ab2e3c60917feb2443f58c97c866ed2b2bcab0a))
* **rpc:** preserve filter cursors and idle polling ([17a5d63](https://github.com/syscoin/zksync-os-server/commit/17a5d63e0b89b7d4dd312c31858867e28e7f2c6b))
* **rpc:** preserve v31 gateway proof response key ([fcb4f45](https://github.com/syscoin/zksync-os-server/commit/fcb4f454a1e08d64cd11d11958c5d94a1022994b))
* **rpc:** reject detached filter activation ([12ae80e](https://github.com/syscoin/zksync-os-server/commit/12ae80ec8ba3f877e64fe7eb3c33d69e6c6c3e86))
* satisfy clippy in batch verification regression tests ([8886b2a](https://github.com/syscoin/zksync-os-server/commit/8886b2a671fe739824a6055c190712ff3738e0e7))
* **security:** harden verifier and source attestations ([8f8cb38](https://github.com/syscoin/zksync-os-server/commit/8f8cb38130bd15c602efc36fb0929c1d3d43cb32))
* **security:** qualify Linux dependency audit ([#361](https://github.com/syscoin/zksync-os-server/issues/361)) ([8aa4cf5](https://github.com/syscoin/zksync-os-server/commit/8aa4cf5f505ed40cb9368eba9fb23422932b61b7))
* **security:** update vulnerable TLS and mempool dependencies ([9211699](https://github.com/syscoin/zksync-os-server/commit/921169997a9e92ee7be874e354624427456e6a8b))
* **sequencer:** reject unexecutable priority head ([8b15dcc](https://github.com/syscoin/zksync-os-server/commit/8b15dccf143c690275434844d407002468774c47))
* **sequencer:** seal block instead of crashing on pubdata limit ([#1517](https://github.com/syscoin/zksync-os-server/issues/1517)) ([ad3bf26](https://github.com/syscoin/zksync-os-server/commit/ad3bf2616ca76f21c083142c70318e68edb97fe7))
* **sequencer:** update default values for gas rate limiter ([#1514](https://github.com/syscoin/zksync-os-server/issues/1514)) ([258423a](https://github.com/syscoin/zksync-os-server/commit/258423a47508e9f28513afa89ee1aa696f73989f))
* serialize deployment broadcasts ([88b6612](https://github.com/syscoin/zksync-os-server/commit/88b6612395da3c9e583d8c934bf15e68fba0146d))
* **settlement:** close recovery guard gaps ([371e56b](https://github.com/syscoin/zksync-os-server/commit/371e56b41e5d2a80974c5a415b80a6207b6a4e38))
* **settlement:** harden recovery and bound DA republication ([a6dfc7f](https://github.com/syscoin/zksync-os-server/commit/a6dfc7f370e8c9192b5efba9b7ff5bce33a5be79))
* **settlement:** harden recovery and bound DA republication ([7c33964](https://github.com/syscoin/zksync-os-server/commit/7c3396416795881ac819cf887b5f37a881492b41))
* **settlement:** reject split migration snapshots ([a5c0ec7](https://github.com/syscoin/zksync-os-server/commit/a5c0ec75f064f9f913a7dba999fa80573b3e55c3))
* **storage:** reject mismatched replay override keys ([a383a60](https://github.com/syscoin/zksync-os-server/commit/a383a60640bc83023fee8a4f6332763fda390369))
* **storage:** reject mismatched replay override keys ([ef6dedf](https://github.com/syscoin/zksync-os-server/commit/ef6dedff3b539de9e8d283c99740f88359b27c91))
* **storage:** retire sender nonce entries with their owner ([a23dd69](https://github.com/syscoin/zksync-os-server/commit/a23dd69aaefed661037d03b2f4c2bb33ffd73da9))
* **syscoin-da:** persist recovery timing and separate missing-blob grace ([1a76183](https://github.com/syscoin/zksync-os-server/commit/1a76183069e2b306033fa0600fdf4634cedd5aa0))
* **syscoin-da:** persist recovery timing and separate missing-blob grace ([93ab3a4](https://github.com/syscoin/zksync-os-server/commit/93ab3a469cf98630784d5b24714181a1d0c33de1))
* **syscoin-da:** sample recovery clocks after acquiring locks ([8b96589](https://github.com/syscoin/zksync-os-server/commit/8b965895ed498b5d33d14a3cd20e0594c7e90379))
* **syscoin:** close v32 review gaps ([a78614f](https://github.com/syscoin/zksync-os-server/commit/a78614ff107d0112a94ae91067a30fde8b36dd99))
* **test:** allow bounded Anvil fixture startup ([2a354e5](https://github.com/syscoin/zksync-os-server/commit/2a354e56041c1f08f5f06ffbde43061cd9e49137))
* **test:** allow commits while delaying fake SNARK completion ([388a187](https://github.com/syscoin/zksync-os-server/commit/388a187e238ff7a009f264ad2a1158e86482cfcd))
* **test:** deploy trace token before raising pubdata fees ([6ba4153](https://github.com/syscoin/zksync-os-server/commit/6ba415355406730d95eaee940792e42868596296))
* **test:** exercise user asset flows on canonical Gateway edges ([496bffe](https://github.com/syscoin/zksync-os-server/commit/496bffef62f32ba207ddee6a5f879c2cd8ef71fe))
* **test:** keep high-price estimation within V32 gas ceiling ([ff579c3](https://github.com/syscoin/zksync-os-server/commit/ff579c3c571258d21a66bd5b3bb6cb9d3dd7a8de))
* **test:** place timestamp tests after production items ([30bdd44](https://github.com/syscoin/zksync-os-server/commit/30bdd4458c7bce1fe9ca563fe8d7158b5998d2f9))
* **test:** preserve Bitcoin DA mock batch envelopes ([b636ea7](https://github.com/syscoin/zksync-os-server/commit/b636ea7862adc699750095d9fd98820524187931))
* **test:** preserve deployment identity across archive recovery ([4e121fe](https://github.com/syscoin/zksync-os-server/commit/4e121feeae29c56a0c306fce23cb028e8e43cf7e))
* **test:** provision bounded component settlement fee consent ([27438bf](https://github.com/syscoin/zksync-os-server/commit/27438bfa448026a157037667d4c86bba389066c5))
* **test:** respect V32 gas ceiling for interop execution ([81c16c7](https://github.com/syscoin/zksync-os-server/commit/81c16c700d05333bef2c1b32cb4ce1641222680a))
* **test:** restore normal sender window for component fixtures ([203b903](https://github.com/syscoin/zksync-os-server/commit/203b903d5b8c899d6023c27df44127f4519003ff))
* **tests:** box the large Core NEVM fixture inventory ([f6cd764](https://github.com/syscoin/zksync-os-server/commit/f6cd764df02e71f6fa674c99d6f86ca20193c777))
* **tests:** disable partial fake proving ([cf2ce9f](https://github.com/syscoin/zksync-os-server/commit/cf2ce9fffc200ce17996db4e394298776f1cd6e1))
* **tests:** fund policy bootstrap through authenticated L1 deposits ([fe83389](https://github.com/syscoin/zksync-os-server/commit/fe833893588be9ecbb7d5b7d6c6668a892e9820d))
* **tests:** group optional default upgrade post-actions ([bbc70c3](https://github.com/syscoin/zksync-os-server/commit/bbc70c3ebf7a7af3313a2f236c8314b685f3dcfa))
* **tests:** preserve pending restart batches and fail stress CI ([052c904](https://github.com/syscoin/zksync-os-server/commit/052c9046614566a2a4116adf5957773cc8782117))
* **tests:** preserve pending restart batches and fail stress CI ([a2e2028](https://github.com/syscoin/zksync-os-server/commit/a2e202837c9d28db46873961c0f8b0ee1b56b9e3))
* **tests:** synchronize restart and fixture settlement boundaries ([d2b6eb5](https://github.com/syscoin/zksync-os-server/commit/d2b6eb5cc0b5de737971b5f0023bf73bd46947f3))
* **test:** tolerate process exit during Linux liveness probe ([936555d](https://github.com/syscoin/zksync-os-server/commit/936555d4f289c0f0764564614b80c17b163c9868))
* **test:** use canonical DA mode for Gateway-settled chains ([8a5b49b](https://github.com/syscoin/zksync-os-server/commit/8a5b49bf51af048df6c74dc06d1424db855bd333))
* **test:** wait for encrypted replay prefix publication ([6a406a7](https://github.com/syscoin/zksync-os-server/commit/6a406a7580306cf3efbd37b68966afa28bdcec7b))
* **test:** wait for replay before post-restart transactions ([99624c4](https://github.com/syscoin/zksync-os-server/commit/99624c4021f3ff60ef9be03bbb7506dbac3b4c65))
* **tls:** refresh server trust after emSign distrust ([#350](https://github.com/syscoin/zksync-os-server/issues/350)) ([1bf543d](https://github.com/syscoin/zksync-os-server/commit/1bf543dd5cec06605c47ce96b7b039888baf0d9d))
* **tooling:** protect launch files and fail closed on smoke checks ([926d6fb](https://github.com/syscoin/zksync-os-server/commit/926d6fb3c6c8f52e7b457a6675e0791cbeda897b))
* **upgrade:** deploy v32 facets on settlement layer ([6805c0b](https://github.com/syscoin/zksync-os-server/commit/6805c0b42116fd3d6a313faff20d62d42e84c60e))
* **upgrade:** scope legacy calldata fallback ([5a35cb0](https://github.com/syscoin/zksync-os-server/commit/5a35cb094ef945f414e96e75bcf451051c6693df))
* **upgrade:** use canonical fresh-v32 cut calldata ([80c3f48](https://github.com/syscoin/zksync-os-server/commit/80c3f48a7a856de8487f8e62854be287c2614f31))
* **upstream:** preserve v31 compatibility after v32 sync ([9bbc051](https://github.com/syscoin/zksync-os-server/commit/9bbc051ef3e227e3e15a50c03541d055d9f80ef3))
* **v32:** authenticate Gateway commit target runtime ([ace1954](https://github.com/syscoin/zksync-os-server/commit/ace19542478baad9bdd8595fcb5b5d3e353a22af))
* **v32:** bind guardian consent to epochs and detect L1 reorgs ([7a0c675](https://github.com/syscoin/zksync-os-server/commit/7a0c67505e4597c449f20dbb5ab969f3b64f4bda))
* **v32:** close audit recovery and native consistency gaps ([12d35ba](https://github.com/syscoin/zksync-os-server/commit/12d35ba8a6716e817e53157dd9fd21939f9c3b21))
* **v32:** close final prover and workflow review gaps ([288043a](https://github.com/syscoin/zksync-os-server/commit/288043a760d95715c67f2c87251500cf0b739625))
* **v32:** harden live gateway integration ([a068102](https://github.com/syscoin/zksync-os-server/commit/a068102a41000fa74b01857155cf7555cbc8b4b3))
* **v32:** harden live Gateway integration ([45063e2](https://github.com/syscoin/zksync-os-server/commit/45063e2f7244a39432dff538f8973fc222c0d61c))
* **v32:** separate deployment source checks from fixture readiness ([be24f4b](https://github.com/syscoin/zksync-os-server/commit/be24f4b47495d68400ede9f57c0d0c2ee7b537a6))
* **v32:** sync guest source attestation after target repin ([3383ea9](https://github.com/syscoin/zksync-os-server/commit/3383ea993cfa3cc6d8900b56767ffa21123285e8))
* **v7:** enforce native and artifact consistency ([77ae613](https://github.com/syscoin/zksync-os-server/commit/77ae6133a3cb8a03c049b0fbd9607da0437b2fad))
* **v7:** keep upstream OS source ([27c4659](https://github.com/syscoin/zksync-os-server/commit/27c4659fbeb43c85cb09e39795f5424954c9cd23))
* **v7:** require patched OS for native execution ([b690678](https://github.com/syscoin/zksync-os-server/commit/b69067831c7af6c50432bfedb34c37293f6feeb4))
* **validator:** bind signatures to native predecessor ([013b76f](https://github.com/syscoin/zksync-os-server/commit/013b76f4021f75fe96287c6de0d17b422b08f1ae))
* wait for gateway edge registration ([c46e567](https://github.com/syscoin/zksync-os-server/commit/c46e5677379dca925ace28ff1ce1efc1d95fa9df))
* **zkstack:** attest V32 workspace dependencies ([9bf3066](https://github.com/syscoin/zksync-os-server/commit/9bf30667c9b647b9d4b2f3dc49cc50d897aa3f73))
* **zkstack:** bind ephemeral Forge signer sender ([0d6917d](https://github.com/syscoin/zksync-os-server/commit/0d6917d4189e265b0fa7fd81695e3ed490b300d4))
* **zksys:** default keeper batches to 512 ([de29466](https://github.com/syscoin/zksync-os-server/commit/de29466f9b04c6ade026a373df5158c54bce7405))
* **zksys:** harden keeper recovery and CI ([03d4d1b](https://github.com/syscoin/zksync-os-server/commit/03d4d1b0ba6b6d8a81f8fea80bac48f576a9515a))
* **zksys:** remove membership observation expiry ([4ab10f3](https://github.com/syscoin/zksync-os-server/commit/4ab10f3f1791fb64b43a8d9badc11e93e017de0f))
* **zksys:** retry dropped keeper transactions ([859dcf5](https://github.com/syscoin/zksync-os-server/commit/859dcf5261c2e489c45634d9ad0a3bde1fad4b3a))


### Performance Improvements

* **prover:** share retained witness buffers ([1f01d69](https://github.com/syscoin/zksync-os-server/commit/1f01d69dfeb204324930efba9e43382c02c61b53))

## [0.23.0](https://github.com/matter-labs/zksync-os-server/compare/v0.22.0...v0.23.0) (2026-08-24)


### ⚠ BREAKING CHANGES

* remove the Compacted state backend ([#1538](https://github.com/matter-labs/zksync-os-server/issues/1538))
* **prover_api:** move the V8 lane to the zksync-os v0.4.0 release ([#1532](https://github.com/matter-labs/zksync-os-server/issues/1532))

### Features

* bump alloy to v2.4.1 and reth to v2.5.0 ([#1518](https://github.com/matter-labs/zksync-os-server/issues/1518)) ([8f17d02](https://github.com/matter-labs/zksync-os-server/commit/8f17d02a886f051dcff5da5e2a5ac29393748052))
* **prover_api:** move the V8 lane to the zksync-os v0.4.0 release ([#1532](https://github.com/matter-labs/zksync-os-server/issues/1532)) ([cceb02c](https://github.com/matter-labs/zksync-os-server/commit/cceb02c8215036eb96fcf719c343594c2da41bd0))
* **prover_api:** v32.0 / proving V8 - make real proofs settle on L1 ([#1510](https://github.com/matter-labs/zksync-os-server/issues/1510)) ([0bafb13](https://github.com/matter-labs/zksync-os-server/commit/0bafb13d89d7da2f7adb9bad6c12d5301351a80d))
* **prover:** bring back v30.x (V6) proving support ([#1527](https://github.com/matter-labs/zksync-os-server/issues/1527)) ([308f6d5](https://github.com/matter-labs/zksync-os-server/commit/308f6d57626f0ccff1aa70f1a366b40ed0fc7101))
* **prover:** drop pre-0.3 proving support ([#1525](https://github.com/matter-labs/zksync-os-server/issues/1525)) ([d89f354](https://github.com/matter-labs/zksync-os-server/commit/d89f354216da4f1f2887e6a496744c7deefa4d84))
* remove the Compacted state backend ([#1538](https://github.com/matter-labs/zksync-os-server/issues/1538)) ([9656632](https://github.com/matter-labs/zksync-os-server/commit/9656632e6415c7626585abb3c45d515a6ae4e0a1))
* **zks:** add stable getBatchByNumber and getLatestBatchNumber RPC ([#1484](https://github.com/matter-labs/zksync-os-server/issues/1484)) ([0852ab2](https://github.com/matter-labs/zksync-os-server/commit/0852ab276891b7f2dd00af2d2f92b0be74bcc43c))
* zksync-os-revm v0.2.0 ([#1528](https://github.com/matter-labs/zksync-os-server/issues/1528)) ([3f6eff0](https://github.com/matter-labs/zksync-os-server/commit/3f6eff0343bd08b7930a6ddca0443738825f2275))


### Bug Fixes

* **batch_verification:** park instead of crashing when no signatures ([#1509](https://github.com/matter-labs/zksync-os-server/issues/1509)) ([f3117c8](https://github.com/matter-labs/zksync-os-server/commit/f3117c8560c7ccf8654afed18ab146b8231e1cb1))
* **l1_sender:** capture confirmed nonce before waiting for commands ([#1543](https://github.com/matter-labs/zksync-os-server/issues/1543)) ([ef9ca49](https://github.com/matter-labs/zksync-os-server/commit/ef9ca4927ba4c2fc1a5bfc1a78d52ec28288f291))
* **l1_watcher:** recognize pre-v31 supplier getter reverts ([#1523](https://github.com/matter-labs/zksync-os-server/issues/1523)) ([5a51cb4](https://github.com/matter-labs/zksync-os-server/commit/5a51cb45ae44f01406b6ead84d005e0ae4af5447))
* **merkle_tree:** handle empty batch proofs ([#1535](https://github.com/matter-labs/zksync-os-server/issues/1535)) ([0b97156](https://github.com/matter-labs/zksync-os-server/commit/0b97156fe230853a42bb15ddc1a11e9c2116ff9b))
* **network:** disconnect and re-request when the EN replay stream stalls ([#1513](https://github.com/matter-labs/zksync-os-server/issues/1513)) ([3319561](https://github.com/matter-labs/zksync-os-server/commit/331956187080e35cb3be3f237510d087d9f3e041))
* **pipeline:** make graceful shutdown race-free ([#1521](https://github.com/matter-labs/zksync-os-server/issues/1521)) ([4578a8c](https://github.com/matter-labs/zksync-os-server/commit/4578a8c898b4dd6772c44eee5815cd0f8e4560d6))
* **prover_api:** measure the payload transfer in pick_job_latency ([#1526](https://github.com/matter-labs/zksync-os-server/issues/1526)) ([e9efb14](https://github.com/matter-labs/zksync-os-server/commit/e9efb145facf008ccdbe050bfbffa842a915024d))
* **prover_api:** reserve capacity before fake SNARK assignment ([#1522](https://github.com/matter-labs/zksync-os-server/issues/1522)) ([8f6f645](https://github.com/matter-labs/zksync-os-server/commit/8f6f6457fb77743ba4785e2b77a4175365cc72cd))
* **prover_api:** widen prover histogram buckets so quantiles stop clipping ([#1505](https://github.com/matter-labs/zksync-os-server/issues/1505)) ([defc056](https://github.com/matter-labs/zksync-os-server/commit/defc056477786c58db89f38b0921a3fa443f20be))
* **sequencer:** seal block instead of crashing on pubdata limit ([#1517](https://github.com/matter-labs/zksync-os-server/issues/1517)) ([ad3bf26](https://github.com/matter-labs/zksync-os-server/commit/ad3bf2616ca76f21c083142c70318e68edb97fe7))
* **sequencer:** update default values for gas rate limiter ([#1514](https://github.com/matter-labs/zksync-os-server/issues/1514)) ([258423a](https://github.com/matter-labs/zksync-os-server/commit/258423a47508e9f28513afa89ee1aa696f73989f))

## [0.22.0](https://github.com/matter-labs/zksync-os-server/compare/v0.21.0...v0.22.0) (2026-08-07)


### ⚠ BREAKING CHANGES

* **l1_sender:** pipelined sending with a bounded in-flight window ([#1485](https://github.com/matter-labs/zksync-os-server/issues/1485))

### Features

* **l1_sender:** pipelined sending with a bounded in-flight window ([#1485](https://github.com/matter-labs/zksync-os-server/issues/1485)) ([83a0dc8](https://github.com/matter-labs/zksync-os-server/commit/83a0dc8ab363ec2fdfd7babf3a00c3f9c560f7f1))
* **node:** refuse main-node startup when replay WAL is behind L1 ([#1504](https://github.com/matter-labs/zksync-os-server/issues/1504)) ([7b5d906](https://github.com/matter-labs/zksync-os-server/commit/7b5d9060a107ad9adba0fe4419ceb11b45293c39))
* **rpc:** L1 aggregation-hop proof for L1-settled chains (enable interop on L1) ([#1413](https://github.com/matter-labs/zksync-os-server/issues/1413)) ([5974897](https://github.com/matter-labs/zksync-os-server/commit/597489700bc45d61437e829305c8aa6517548958))
* zksync-os 0.4.0 ([#1421](https://github.com/matter-labs/zksync-os-server/issues/1421)) ([59bb218](https://github.com/matter-labs/zksync-os-server/commit/59bb218bc7b0bfe84dc3dca90a3df66a6d49069d))


### Bug Fixes

* **batch_verification:** bound cache eviction to cached blocks ([#1502](https://github.com/matter-labs/zksync-os-server/issues/1502)) ([418db6e](https://github.com/matter-labs/zksync-os-server/commit/418db6e64f1ee9f7dd2abc497b33f74865c937b9))

## [0.21.0](https://github.com/matter-labs/zksync-os-server/compare/v0.20.12...v0.21.0) (2026-08-04)


### ⚠ BREAKING CHANGES

* **network:** drop support for zks v1-v4 ([#1482](https://github.com/matter-labs/zksync-os-server/issues/1482))

### Features

* Expose protocol version filtering for prover ([#1480](https://github.com/matter-labs/zksync-os-server/issues/1480)) ([b660c2e](https://github.com/matter-labs/zksync-os-server/commit/b660c2e3e7f1fcccf2912c0fb00b85cf91b7e7e7))
* **mempool:** validate intrinsic native resources for l2 txs ([#1217](https://github.com/matter-labs/zksync-os-server/issues/1217)) ([e8efe74](https://github.com/matter-labs/zksync-os-server/commit/e8efe74df283cfe58fcb484f2703d1cebb2aeee2))
* **network:** drop support for zks v1-v4 ([#1482](https://github.com/matter-labs/zksync-os-server/issues/1482)) ([f97cd67](https://github.com/matter-labs/zksync-os-server/commit/f97cd67aa370166ba94e21e34a4424f6df0a2cf9))
* **replay_archive:** native gcs archive support ([#1426](https://github.com/matter-labs/zksync-os-server/issues/1426)) ([2338202](https://github.com/matter-labs/zksync-os-server/commit/233820206e23356f5206fb10c195f9b907f1f5cf))
* **rpc:** opt-in parallel execution of JSON-RPC batch entries ([#1475](https://github.com/matter-labs/zksync-os-server/issues/1475)) ([17b45c6](https://github.com/matter-labs/zksync-os-server/commit/17b45c6b1fa8c6d9dda28f1bfdd102bd72bc35f4))
* **sequencer:** rate limiter on gas/s ([#1474](https://github.com/matter-labs/zksync-os-server/issues/1474)) ([dc88a9d](https://github.com/matter-labs/zksync-os-server/commit/dc88a9d246b1f2f1e499cc18e6c524c76f48d4d0))


### Bug Fixes

* **provider:** fail hung L1 RPC requests with a retryable timeout ([#1498](https://github.com/matter-labs/zksync-os-server/issues/1498)) ([8a52df5](https://github.com/matter-labs/zksync-os-server/commit/8a52df58cf60d1881b46a43a737364181a4b9962))
* **provider:** retry header watcher poll when RPC returns no block ([#1492](https://github.com/matter-labs/zksync-os-server/issues/1492)) ([0034170](https://github.com/matter-labs/zksync-os-server/commit/003417016188dcc35ccb9530de41ee23ca4f0843))
* **provider:** survive transient RPC errors in header watchers ([#1490](https://github.com/matter-labs/zksync-os-server/issues/1490)) ([a6849b8](https://github.com/matter-labs/zksync-os-server/commit/a6849b8856a6abd5cec0e01b78ef6edbb31667d2))
* **sequencer:** improve default config values ([#1486](https://github.com/matter-labs/zksync-os-server/issues/1486)) ([57f7ad5](https://github.com/matter-labs/zksync-os-server/commit/57f7ad547d1dbfadb483243c7e661578570dc37a))

## [0.20.12](https://github.com/matter-labs/zksync-os-server/compare/v0.20.11...v0.20.12) (2026-07-20)


### Features

* Add Azure HSM signing support ([#1467](https://github.com/matter-labs/zksync-os-server/issues/1467)) ([316f4e0](https://github.com/matter-labs/zksync-os-server/commit/316f4e0417b49f5287a16376e24b33ba4d810665))
* drop support for SL intervals ([#1457](https://github.com/matter-labs/zksync-os-server/issues/1457)) ([1561279](https://github.com/matter-labs/zksync-os-server/commit/1561279ddc8320cba434884379d9af278e9af199))
* **network:** move 2FA logic to its own subprotocol ([#1409](https://github.com/matter-labs/zksync-os-server/issues/1409)) ([fab1bb3](https://github.com/matter-labs/zksync-os-server/commit/fab1bb39eb88b5466b87eb83f00fdf8ff239c95c))
* new v31 state generated by zk-deployer ([#1462](https://github.com/matter-labs/zksync-os-server/issues/1462)) ([223ca3e](https://github.com/matter-labs/zksync-os-server/commit/223ca3ed2bc434b61aa1a3ff4a8ef81ae6867d35))
* **status:** add /status/ready readiness probe ([#1461](https://github.com/matter-labs/zksync-os-server/issues/1461)) ([1dfcb88](https://github.com/matter-labs/zksync-os-server/commit/1dfcb88e60eaf1ec3a0152927eb21fc7100cbf77))


### Bug Fixes

* **network:** drop external IP probe at startup ([#1465](https://github.com/matter-labs/zksync-os-server/issues/1465)) ([6d673bc](https://github.com/matter-labs/zksync-os-server/commit/6d673bcd5b2e3cea81aa64255eec841987738e2d))
* **network:** honor EN-requested replay record overrides ([#1464](https://github.com/matter-labs/zksync-os-server/issues/1464)) ([d8d7440](https://github.com/matter-labs/zksync-os-server/commit/d8d7440db925d30a8049f083758de01d8008feec))
* **node:** retry listener prebind on transient AddrInUse ([#1453](https://github.com/matter-labs/zksync-os-server/issues/1453)) ([0d076c2](https://github.com/matter-labs/zksync-os-server/commit/0d076c222c2fa12fa090e421c84474ccf0c1a411))
* **revm-checker:** feed REVM the hardcoded prevrandao ZKsync OS actually uses ([#1470](https://github.com/matter-labs/zksync-os-server/issues/1470)) ([abbfb36](https://github.com/matter-labs/zksync-os-server/commit/abbfb36178a989289e6c31dc1de7f61f26e02126))
* **tree_manager:** track tree RocksDB writes as graceful shutdown tasks ([#1444](https://github.com/matter-labs/zksync-os-server/issues/1444)) ([e34880b](https://github.com/matter-labs/zksync-os-server/commit/e34880b52e8a12a0246403e7fdaf4faead72c6b6))


### Performance Improvements

* **l1_watcher:** batch-sweep startup commit discovery ([#1460](https://github.com/matter-labs/zksync-os-server/issues/1460)) ([ab4e583](https://github.com/matter-labs/zksync-os-server/commit/ab4e5831eee876f5415ea219a2389f578d7a7ae7))
* **provider:** skip logs cache historical backfill on cold start ([#1452](https://github.com/matter-labs/zksync-os-server/issues/1452)) ([61c5a3f](https://github.com/matter-labs/zksync-os-server/commit/61c5a3f5a2797009418b96f93086681902000855))
* **storage:** stop depending on persisted block hashes  ([#1458](https://github.com/matter-labs/zksync-os-server/issues/1458)) ([0da3c24](https://github.com/matter-labs/zksync-os-server/commit/0da3c24ce1af4e4dbf2a4a2bef481bbc4579eae2))

## [0.20.11](https://github.com/matter-labs/zksync-os-server/compare/v0.20.10...v0.20.11) (2026-07-13)


### Performance Improvements

* **l1_sender:** improve fees and metrics usage ([#1443](https://github.com/matter-labs/zksync-os-server/issues/1443)) ([e23a088](https://github.com/matter-labs/zksync-os-server/commit/e23a0882265deabc5f5b4a7a5ac922ef480a06c3))

## [0.20.10](https://github.com/matter-labs/zksync-os-server/compare/v0.20.9...v0.20.10) (2026-07-13)


### Features

* **network:** restrict external nodes to trusted peers only ([#1438](https://github.com/matter-labs/zksync-os-server/issues/1438)) ([9b57b36](https://github.com/matter-labs/zksync-os-server/commit/9b57b36161304fa203731e92f367a6870942bf96))
* **network:** treat boot nodes as trusted peers to keep ENs synced ([#1432](https://github.com/matter-labs/zksync-os-server/issues/1432)) ([c37f7b8](https://github.com/matter-labs/zksync-os-server/commit/c37f7b87091d1be82b23fd520a31d7aa240be9f0))
* **status:** add pipeline status endpoint ([#1269](https://github.com/matter-labs/zksync-os-server/issues/1269)) ([07641b7](https://github.com/matter-labs/zksync-os-server/commit/07641b7d16d9a3a6e07bb506225710f94ff23c5b))


### Bug Fixes

* **deps:** update crossbeam-epoch to 0.9.20 ([#1439](https://github.com/matter-labs/zksync-os-server/issues/1439)) ([521a705](https://github.com/matter-labs/zksync-os-server/commit/521a70588587845f5e322fe759d90ea0a54fd580))
* **l1-sender:** retry nonce-class L1 rejections instead of crashing the node ([#1436](https://github.com/matter-labs/zksync-os-server/issues/1436)) ([e1039e4](https://github.com/matter-labs/zksync-os-server/commit/e1039e44c77628eabfb14f3f60da0683d89c53d7))
* **sequencer:** detect EIP-7594 blob support via eth_config ([#1434](https://github.com/matter-labs/zksync-os-server/issues/1434)) ([81f719c](https://github.com/matter-labs/zksync-os-server/commit/81f719c551114bc9ebf05c861f6fc5b523d50973))

## [0.20.9](https://github.com/matter-labs/zksync-os-server/compare/v0.20.8...v0.20.9) (2026-07-03)


### Bug Fixes

* **deps:** bump anyhow to 1.0.103 ([#1424](https://github.com/matter-labs/zksync-os-server/issues/1424)) ([85d7700](https://github.com/matter-labs/zksync-os-server/commit/85d7700da4981bd5dfbfa812ad193af0a8f0103c))
* **deps:** ignore unfixable quick-xml DoS advisories ([#1431](https://github.com/matter-labs/zksync-os-server/issues/1431)) ([477960e](https://github.com/matter-labs/zksync-os-server/commit/477960e8351cd6d18204b8c2ab4fca16cfe6cfce))
* harden ephemeral node port handling ([#1400](https://github.com/matter-labs/zksync-os-server/issues/1400)) ([068b62d](https://github.com/matter-labs/zksync-os-server/commit/068b62da2d71e434909b7fef865002b2644870f6))
* **sequencer:** ensure l1 revert is completed ([#1420](https://github.com/matter-labs/zksync-os-server/issues/1420)) ([7bacb33](https://github.com/matter-labs/zksync-os-server/commit/7bacb33519db1bbdd9f997f07824755e4b5b2bca))

## [0.20.8](https://github.com/matter-labs/zksync-os-server/compare/v0.20.7...v0.20.8) (2026-06-29)


### Features

* **external-node:** retry main-node RPC instead of crash-looping on startup ([#1405](https://github.com/matter-labs/zksync-os-server/issues/1405)) ([df3f9d7](https://github.com/matter-labs/zksync-os-server/commit/df3f9d78848b1ded760271fb5dba45291aa58da7))
* **gateway:** drop support for live gateway migration ([#1401](https://github.com/matter-labs/zksync-os-server/issues/1401)) ([aea7b88](https://github.com/matter-labs/zksync-os-server/commit/aea7b88aa37c9b4619aa8903c8670d79fded5b77))
* **rpc:** add method_filter to disable stateful methods ([#1396](https://github.com/matter-labs/zksync-os-server/issues/1396)) ([1f772e7](https://github.com/matter-labs/zksync-os-server/commit/1f772e77df652af7614af8ff47bf865dbe5d6bd1))
* **rpc:** configurable rate limits via RateLimits enum ([#1397](https://github.com/matter-labs/zksync-os-server/issues/1397)) ([9a60352](https://github.com/matter-labs/zksync-os-server/commit/9a6035273e4b2b1abe182efd51084113e61240d9))
* **sequencer:** L1 batch revert ([#1328](https://github.com/matter-labs/zksync-os-server/issues/1328)) ([e6f0575](https://github.com/matter-labs/zksync-os-server/commit/e6f0575b33c1f0b8eb2069a7e7c2881e7ff9cdad))


### Bug Fixes

* bump memmap2, quinn-proto versions ([#1411](https://github.com/matter-labs/zksync-os-server/issues/1411)) ([f82477c](https://github.com/matter-labs/zksync-os-server/commit/f82477c58d6bc600ab8916b7e1060b4ec97371e4))
* **l1_watcher:** tolerate migrationNumber reverts when resolving gateway migration start block ([#1404](https://github.com/matter-labs/zksync-os-server/issues/1404)) ([b7be022](https://github.com/matter-labs/zksync-os-server/commit/b7be02268adf4afafc9761476ced74be8de8e997))
* **l1-watcher:** handle pre-v31 CAH interface ([#1418](https://github.com/matter-labs/zksync-os-server/issues/1418)) ([47aa445](https://github.com/matter-labs/zksync-os-server/commit/47aa445cbe1cb3f6256fb1da9f9d9e844db5cfa3))
* **rpc:** bound RPC metric label cardinality by method name ([#1419](https://github.com/matter-labs/zksync-os-server/issues/1419)) ([878fc3b](https://github.com/matter-labs/zksync-os-server/commit/878fc3b09bbd0bd09ae247f6cd73788bbe7bfa4a))

## [0.20.7](https://github.com/matter-labs/zksync-os-server/compare/v0.20.6...v0.20.7) (2026-06-16)


### Features

* **backpressure:** introduce PipelineAdmissionGate to pause block ingestion under backpressure ([#1363](https://github.com/matter-labs/zksync-os-server/issues/1363)) ([561c39e](https://github.com/matter-labs/zksync-os-server/commit/561c39e11909344c880da09d4da9442e1fc4d52a))
* **l1-sender:** drop `fusaka_upgrade_timestamp` config option ([#1386](https://github.com/matter-labs/zksync-os-server/issues/1386)) ([239f76e](https://github.com/matter-labs/zksync-os-server/commit/239f76e0afd4b63679262c9e946509d4dc8f66ca))
* **l1-sender:** Make confirmation requirement configurable ([#1374](https://github.com/matter-labs/zksync-os-server/issues/1374)) ([e4442bc](https://github.com/matter-labs/zksync-os-server/commit/e4442bc27d58a2bd21ffad57d3e1a6ac388772c3))
* **provider:** add fallback when `finalized` is not supported ([#1370](https://github.com/matter-labs/zksync-os-server/issues/1370)) ([b579e08](https://github.com/matter-labs/zksync-os-server/commit/b579e08dc106c0eee921abcb385bfe0cdfaa2242))


### Bug Fixes

* **ci:** set finalized block polling interval to 100ms in local_dev.yaml ([#1354](https://github.com/matter-labs/zksync-os-server/issues/1354)) ([93d8d94](https://github.com/matter-labs/zksync-os-server/commit/93d8d94404a98c62683d24272292926b4660f945))
* ignore tx limit for v31 upgrade block ([#1389](https://github.com/matter-labs/zksync-os-server/issues/1389)) ([67d4195](https://github.com/matter-labs/zksync-os-server/commit/67d4195684de4d2de0d6ede34812f41efd0373ec))
* JS tracer performance ([#1381](https://github.com/matter-labs/zksync-os-server/issues/1381)) ([e18afb5](https://github.com/matter-labs/zksync-os-server/commit/e18afb51162abbce2e17669739ea5a6ac39c737a))
* tracing multi-root calls ([#1318](https://github.com/matter-labs/zksync-os-server/issues/1318)) ([04d144c](https://github.com/matter-labs/zksync-os-server/commit/04d144c598fed6ee00e6636cca531a5088427acc))


### Performance Improvements

* fetch committed batch data elements in parallel ([#1361](https://github.com/matter-labs/zksync-os-server/issues/1361)) ([c613906](https://github.com/matter-labs/zksync-os-server/commit/c61390653d3b6ebe9ad4841d5ea72313121a207e))

## [0.20.6](https://github.com/matter-labs/zksync-os-server/compare/v0.20.5...v0.20.6) (2026-06-03)


### Features

* **l1:** use deployment block as lower bound for binsearch ([#1350](https://github.com/matter-labs/zksync-os-server/issues/1350)) ([3ec0af1](https://github.com/matter-labs/zksync-os-server/commit/3ec0af16c65184418866883fb4a846a6ec63f6cb))


### Bug Fixes

* **l1-sender:** Blob tx gas estimation ([#1351](https://github.com/matter-labs/zksync-os-server/issues/1351)) ([4119dc1](https://github.com/matter-labs/zksync-os-server/commit/4119dc13469bf76363bd8dc907364a12baf4b24a))

## [0.20.5](https://github.com/matter-labs/zksync-os-server/compare/v0.20.4...v0.20.5) (2026-06-02)


### Features

* S3 replay archive implementation ([#1319](https://github.com/matter-labs/zksync-os-server/issues/1319)) ([4d522d4](https://github.com/matter-labs/zksync-os-server/commit/4d522d40080cc79857bfa7ef6ca0c93fbcdb8d64))
* use simulateV1 for gas limits ([#1308](https://github.com/matter-labs/zksync-os-server/issues/1308)) ([f1aca77](https://github.com/matter-labs/zksync-os-server/commit/f1aca77356960105fa5766931ee616c741e072ea))
* **watcher:** Separate value for finalized block polling ([#1342](https://github.com/matter-labs/zksync-os-server/issues/1342)) ([de4a84f](https://github.com/matter-labs/zksync-os-server/commit/de4a84f875218d082e9d0c98b4576b2af8d97277))


### Bug Fixes

* **alloy:** fallback to eth_getBlockByNumber with old behaviour ([#1338](https://github.com/matter-labs/zksync-os-server/issues/1338)) ([3653b37](https://github.com/matter-labs/zksync-os-server/commit/3653b37163ac0c33ea50fc5ffd86b8767d1525fe))

## [0.20.4](https://github.com/matter-labs/zksync-os-server/compare/v0.20.3...v0.20.4) (2026-05-26)


### Features

* **multivm:** use v0.1.3 interface ([#1311](https://github.com/matter-labs/zksync-os-server/issues/1311)) ([e908627](https://github.com/matter-labs/zksync-os-server/commit/e9086271239b1f9f7e640847c57a8d1fb310292e))
* **observability:** distinguish between default and non-default values in config metrics ([#1330](https://github.com/matter-labs/zksync-os-server/issues/1330)) ([bd44113](https://github.com/matter-labs/zksync-os-server/commit/bd44113c5d7f5dc187ac0d0f3e39dd0cbe1ffbec))
* replay record archive ([#1279](https://github.com/matter-labs/zksync-os-server/issues/1279)) ([e412fe9](https://github.com/matter-labs/zksync-os-server/commit/e412fe94b0679d8f925a8a55f523e0d92d01ab6e))
* **rpc:** add eth_fillTransaction ([#1288](https://github.com/matter-labs/zksync-os-server/issues/1288)) ([8507c44](https://github.com/matter-labs/zksync-os-server/commit/8507c4448a4b9f882d3a936eccc4b54591860fe3))
* **rpc:** per-method and global JSON-RPC rate limiting ([#1297](https://github.com/matter-labs/zksync-os-server/issues/1297)) ([c9b6fd6](https://github.com/matter-labs/zksync-os-server/commit/c9b6fd6c83295fb60f6f1931c5803f0ddd451999))
* **rpc:** surface VM rejections in eth_sendRawTransactionSync ([#1276](https://github.com/matter-labs/zksync-os-server/issues/1276)) ([bb2b452](https://github.com/matter-labs/zksync-os-server/commit/bb2b4525aa2a1eae64456eb5b7a0a3f4c1adf341))
* **tree_manager:** combine tree writes with a single RocksDB flush ([#1304](https://github.com/matter-labs/zksync-os-server/issues/1304)) ([fd59f82](https://github.com/matter-labs/zksync-os-server/commit/fd59f8298eeb515a3562ba606c2cce9de628f0be))
* **watcher:** shared l1/gw polling ([#1281](https://github.com/matter-labs/zksync-os-server/issues/1281)) ([9f580d1](https://github.com/matter-labs/zksync-os-server/commit/9f580d128cc13cd29893d66f42157a853b37911f))


### Bug Fixes

* **consensus:** forward follower transactions over RPC ([#1321](https://github.com/matter-labs/zksync-os-server/issues/1321)) ([6f8c110](https://github.com/matter-labs/zksync-os-server/commit/6f8c110e5781d27c0c8ea78bcf0ad922a572e9f5))
* **integration-tests:** import NetworkTransactionBuilder ([#1305](https://github.com/matter-labs/zksync-os-server/issues/1305)) ([264d992](https://github.com/matter-labs/zksync-os-server/commit/264d9922452fd93ec5e7a1093ec6f928b67321ad))
* **l1-watcher:** adapt upgrade watcher to ServerNotifier semantics ([#1289](https://github.com/matter-labs/zksync-os-server/issues/1289)) ([3e34339](https://github.com/matter-labs/zksync-os-server/commit/3e343399bafdb7cacfec17dcbca43d7ba883e579))
* Revert change that returned back base fee estimation ([#1306](https://github.com/matter-labs/zksync-os-server/issues/1306)) ([e8030c8](https://github.com/matter-labs/zksync-os-server/commit/e8030c877080c0c3b7e3d54c493e9fd22b9da0d1))
* **rpc:** return -32602 instead of -32603 for block-not-found errors ([#1310](https://github.com/matter-labs/zksync-os-server/issues/1310)) ([2f9f1f2](https://github.com/matter-labs/zksync-os-server/commit/2f9f1f2725a379e032dbfa6c4645d2aa7d3f1047))
* **watcher:** skip old transactions in upgrade watcher ([#1325](https://github.com/matter-labs/zksync-os-server/issues/1325)) ([e9e977a](https://github.com/matter-labs/zksync-os-server/commit/e9e977a06816afe8d23a7669228efd034bb39e57))


### Performance Improvements

* **merkle-tree:** Optimize passing tree data through component pipeline ([#1241](https://github.com/matter-labs/zksync-os-server/issues/1241)) ([3dc0f18](https://github.com/matter-labs/zksync-os-server/commit/3dc0f18ea2c95da875e50bc25855e73e169d926a))

## [0.20.3](https://github.com/matter-labs/zksync-os-server/compare/v0.20.2...v0.20.3) (2026-05-18)


### Features

* **consensus:** Final consensus integration and integration tests ([#1225](https://github.com/matter-labs/zksync-os-server/issues/1225)) ([9068d93](https://github.com/matter-labs/zksync-os-server/commit/9068d9393fc201fc86038d24ff589860855e2587))
* **l1-sender:** Refresh balance & nonce more often, refactor for simpler method signatures ([#1260](https://github.com/matter-labs/zksync-os-server/issues/1260)) ([e8cdf32](https://github.com/matter-labs/zksync-os-server/commit/e8cdf3293cc26c8caffc1310e9fa08870d8fbd2e))
* **l1-watcher:** traverse historical GW intervals ([#1285](https://github.com/matter-labs/zksync-os-server/issues/1285)) ([213b433](https://github.com/matter-labs/zksync-os-server/commit/213b433dcc45a108e6af04e73c4578b17066775d))
* **observability:** jemalloc metrics ([#1284](https://github.com/matter-labs/zksync-os-server/issues/1284)) ([c557017](https://github.com/matter-labs/zksync-os-server/commit/c5570177e13aa34764a8283d05cc4fd498c10176))
* **observability:** semi-automatic export of config values as metrics  ([#1265](https://github.com/matter-labs/zksync-os-server/issues/1265)) ([09996da](https://github.com/matter-labs/zksync-os-server/commit/09996da6febbf60e10662c78e6c177365d6b99da))
* **prividium:** integrate PolicyClient over admit + judge ([#1220](https://github.com/matter-labs/zksync-os-server/issues/1220)) ([b00454e](https://github.com/matter-labs/zksync-os-server/commit/b00454e35f6edde8bbb2332c3a56fcfb3fed5311))
* **revm-checker:** adopt new zksync-os-revm version ([#1267](https://github.com/matter-labs/zksync-os-server/issues/1267)) ([2ddd01a](https://github.com/matter-labs/zksync-os-server/commit/2ddd01a12ba627d07850962c683fc472d7df9d26))
* **rpc:** Implement eth_simulateV1 ([#1240](https://github.com/matter-labs/zksync-os-server/issues/1240)) ([630e1ce](https://github.com/matter-labs/zksync-os-server/commit/630e1cee177f61035745eb1743b8fe9bc5891b87))
* separate gateway sender config ([#1277](https://github.com/matter-labs/zksync-os-server/issues/1277)) ([b6c70a9](https://github.com/matter-labs/zksync-os-server/commit/b6c70a9b571664de7c759dc90df1793c024c9aca))
* update v31 state ([#1293](https://github.com/matter-labs/zksync-os-server/issues/1293)) ([bfd382b](https://github.com/matter-labs/zksync-os-server/commit/bfd382bbdf6f3726cba671c1e65c3075d331e49f))


### Bug Fixes

* **migration:** idle when finalized migration channel is closed ([#1290](https://github.com/matter-labs/zksync-os-server/issues/1290)) ([dac983f](https://github.com/matter-labs/zksync-os-server/commit/dac983f444cbdca71dcc1b809b33f0f97e92017b))
* **raft:** rate-limit ensure_linearizable to silence log spam ([#1283](https://github.com/matter-labs/zksync-os-server/issues/1283)) ([945c19c](https://github.com/matter-labs/zksync-os-server/commit/945c19c8a315189433b96d04e92fa91b51baee7a))

## [0.20.2](https://github.com/matter-labs/zksync-os-server/compare/v0.20.1...v0.20.2) (2026-05-11)


### Features

* **mempool:** stream interop txs only during gateway SL ([#1263](https://github.com/matter-labs/zksync-os-server/issues/1263)) ([b2a8a4a](https://github.com/matter-labs/zksync-os-server/commit/b2a8a4abb9dbe158ce979905ac1b4d718e67a103))


### Bug Fixes

* **l1-watcher:** do not panic on non-existent migration ([#1274](https://github.com/matter-labs/zksync-os-server/issues/1274)) ([a3ba0fe](https://github.com/matter-labs/zksync-os-server/commit/a3ba0fe7b0d4f66b4f9f46c3cad9fb195d06207e))
* **network:** pin reth with bounded RLPx buffer ([#1275](https://github.com/matter-labs/zksync-os-server/issues/1275)) ([d6194aa](https://github.com/matter-labs/zksync-os-server/commit/d6194aa9709dc326ad58055b77c12a7f33be6a1e))

## [0.20.1](https://github.com/matter-labs/zksync-os-server/compare/v0.20.0...v0.20.1) (2026-05-07)


### Features

* add backpressure monitor and pipeline wiring ([#1206](https://github.com/matter-labs/zksync-os-server/issues/1206)) ([66861de](https://github.com/matter-labs/zksync-os-server/commit/66861dea0a703df9700c8ec5d66fc6890f9efb77))

## [0.20.0](https://github.com/matter-labs/zksync-os-server/compare/v0.19.3...v0.20.0) (2026-05-07)


### ⚠ BREAKING CHANGES

* **provider:** Add metrics & retry config ([#1257](https://github.com/matter-labs/zksync-os-server/issues/1257))

### Features

* add possibility to forcefully resubmit transactions ([#1239](https://github.com/matter-labs/zksync-os-server/issues/1239)) ([2ddd717](https://github.com/matter-labs/zksync-os-server/commit/2ddd717bf25f434648d123b0cb9d4c3e46e68e00))
* add zks protocol v4 ([#1247](https://github.com/matter-labs/zksync-os-server/issues/1247)) ([7e20bd2](https://github.com/matter-labs/zksync-os-server/commit/7e20bd2dcff449215478b6526bf97630c1c811ed))
* **interop:** Allow inclusion of multiple interop roots in system tx ([#1258](https://github.com/matter-labs/zksync-os-server/issues/1258)) ([dd58427](https://github.com/matter-labs/zksync-os-server/commit/dd584272b1d37360681b58edcb18181c03e599cd))
* **l1-sender:** Use constant base fee for l1 transactions ([#1255](https://github.com/matter-labs/zksync-os-server/issues/1255)) ([d4eb7ce](https://github.com/matter-labs/zksync-os-server/commit/d4eb7cea9c8f9c163b03644455341484f4f57776))
* **observability:** add push exporter support & a metric to track REVM divergencies that can be used for alerting ([#1221](https://github.com/matter-labs/zksync-os-server/issues/1221)) ([df0275f](https://github.com/matter-labs/zksync-os-server/commit/df0275ff8d1c79d3cc6389fd48f759daa0ecce24))
* **provider:** Add metrics & retry config ([#1257](https://github.com/matter-labs/zksync-os-server/issues/1257)) ([ef0bde5](https://github.com/matter-labs/zksync-os-server/commit/ef0bde59bf01e0fbd34b66638adcd5f5e9173190))
* support live chain L1-&gt;GW migration ([#1129](https://github.com/matter-labs/zksync-os-server/issues/1129)) ([a9019f9](https://github.com/matter-labs/zksync-os-server/commit/a9019f92e2b15005c703cc42eb9e3b486a185517))


### Bug Fixes

* **prover-api:** potential race conditions in SnarkJobManager ([#1254](https://github.com/matter-labs/zksync-os-server/issues/1254)) ([e93a7c1](https://github.com/matter-labs/zksync-os-server/commit/e93a7c1428aa9e6b85d103179007dfd82b474bd6))
* register v4 subprotocol ([#1251](https://github.com/matter-labs/zksync-os-server/issues/1251)) ([3ad8db2](https://github.com/matter-labs/zksync-os-server/commit/3ad8db2618d386a04d712c5488353b63286a0c28))

## [0.19.3](https://github.com/matter-labs/zksync-os-server/compare/v0.19.2...v0.19.3) (2026-05-01)


### Features

* **l1-watcher:** adapt upgrade logic to V31 `BytecodeSupplier` ([#1229](https://github.com/matter-labs/zksync-os-server/issues/1229)) ([0ccdf93](https://github.com/matter-labs/zksync-os-server/commit/0ccdf9344ec5f3ac59e811788303a8e70f59397c))
* **l1-watcher:** add SL-aware watcher ([#1236](https://github.com/matter-labs/zksync-os-server/issues/1236)) ([7f78b5d](https://github.com/matter-labs/zksync-os-server/commit/7f78b5d86480bcf1c648011609871b481ca9a414))
* Make block replay size configurable ([#1243](https://github.com/matter-labs/zksync-os-server/issues/1243)) ([0d353f0](https://github.com/matter-labs/zksync-os-server/commit/0d353f0cc39decd9e1504c19ea352e3c5a8cf0f1))
* **network:** add raft transport layer over devp2p ([#1224](https://github.com/matter-labs/zksync-os-server/issues/1224)) ([c4851fb](https://github.com/matter-labs/zksync-os-server/commit/c4851fbfcad2817b7e61570aa3d921c155d85ce6))
* **observability:** add tokio runtime metrics ([#1226](https://github.com/matter-labs/zksync-os-server/issues/1226)) ([76ec1d6](https://github.com/matter-labs/zksync-os-server/commit/76ec1d6f1e33c4a622860b5dc9ed93dff880895b))
* track finalized executed batches and blocks ([#1235](https://github.com/matter-labs/zksync-os-server/issues/1235)) ([6994a67](https://github.com/matter-labs/zksync-os-server/commit/6994a67a5c92f6919733bf87c56031fb4a1a56d2))


### Bug Fixes

* GatewayMigrationWatcher init ([#1231](https://github.com/matter-labs/zksync-os-server/issues/1231)) ([4575099](https://github.com/matter-labs/zksync-os-server/commit/45750994f9f8290c8b786d70d72a56f28c5cdfd8))
* InteropWatcher init ([#1233](https://github.com/matter-labs/zksync-os-server/issues/1233)) ([b95f566](https://github.com/matter-labs/zksync-os-server/commit/b95f566a9d0d6688055d48b3bd06ca2ad800472f))
* **observability:** align tokio metrics sampling interval with Prometheus scrape interval ([#1237](https://github.com/matter-labs/zksync-os-server/issues/1237)) ([39aad42](https://github.com/matter-labs/zksync-os-server/commit/39aad42a34dce056de04795bb5eb86cce632072d))
* **priority-tree:** release merkle tree lock before sending to internal channel ([#1238](https://github.com/matter-labs/zksync-os-server/issues/1238)) ([37c7131](https://github.com/matter-labs/zksync-os-server/commit/37c7131ed8afd169d8685e7a77c0ec7cff8af105))
* Reduce logging verbosity ([#1242](https://github.com/matter-labs/zksync-os-server/issues/1242)) ([db58eef](https://github.com/matter-labs/zksync-os-server/commit/db58eef1c1a941741086e04d3788d686b4c71a46))


### Performance Improvements

* Default to jemalloc for mem allocation ([#1244](https://github.com/matter-labs/zksync-os-server/issues/1244)) ([6b34c6e](https://github.com/matter-labs/zksync-os-server/commit/6b34c6e3922190ccce699ab8a25fd1cde94b7c44))

## [0.19.2](https://github.com/matter-labs/zksync-os-server/compare/v0.19.1...v0.19.2) (2026-04-27)


### Bug Fixes

* Add --block-time to all the places where anvil is being run ([#1214](https://github.com/matter-labs/zksync-os-server/issues/1214)) ([f7ccfb8](https://github.com/matter-labs/zksync-os-server/commit/f7ccfb8a95b6878f31539acbf32065043c54aba4))
* **network:** treat EN starting_block as replay eligibility proof ([#1209](https://github.com/matter-labs/zksync-os-server/issues/1209)) ([b9e36bf](https://github.com/matter-labs/zksync-os-server/commit/b9e36bf364d0da9c2e395876a04f4e27f4d107eb))

## [0.19.1](https://github.com/matter-labs/zksync-os-server/compare/v0.19.0...v0.19.1) (2026-04-23)


### Features

* allow l1 poll interval to be configured ([#1175](https://github.com/matter-labs/zksync-os-server/issues/1175)) ([14c8bb6](https://github.com/matter-labs/zksync-os-server/commit/14c8bb624f7ab0196e61ded57a23e1ba3afae60a)), closes [#1176](https://github.com/matter-labs/zksync-os-server/issues/1176)
* async batch provider init ([#1160](https://github.com/matter-labs/zksync-os-server/issues/1160)) ([d5867dc](https://github.com/matter-labs/zksync-os-server/commit/d5867dce972429c0c6bd54114632c5b6111d5542))
* bytecodes supplier support ([#1155](https://github.com/matter-labs/zksync-os-server/issues/1155)) ([bc1c98c](https://github.com/matter-labs/zksync-os-server/commit/bc1c98cc307515b6ffc9511aa2e7a0062a20178f))
* detect SL intervals using L1 ([#1185](https://github.com/matter-labs/zksync-os-server/issues/1185)) ([ac97007](https://github.com/matter-labs/zksync-os-server/commit/ac97007392f205f0845f3307acdb12c5492ce797))
* **l1_sender:** configurable tx timeout and inclusion latency metric ([#1162](https://github.com/matter-labs/zksync-os-server/issues/1162)) ([b09f672](https://github.com/matter-labs/zksync-os-server/commit/b09f6726564e2712edd8b380ff68461b1158778b))
* **l1-sender:** In-flight tx detection & minor enhancements ([#1172](https://github.com/matter-labs/zksync-os-server/issues/1172)) ([3defe95](https://github.com/matter-labs/zksync-os-server/commit/3defe9538dcfd0efe4aa8a9e12a0b9bad422b56e))
* **l1-sender:** increase L1 required confirmations ([#1189](https://github.com/matter-labs/zksync-os-server/issues/1189)) ([51b65bf](https://github.com/matter-labs/zksync-os-server/commit/51b65bf03fad370ec1cd6bdfcd5832bee1771390))
* **l1-sender:** Increase required confirmations to 3 ([#1197](https://github.com/matter-labs/zksync-os-server/issues/1197)) ([100f218](https://github.com/matter-labs/zksync-os-server/commit/100f218b6ef1a0381d8610de9245d08c85ed1725))
* **rpc:** add txpool_inspect/content/status RPC methods ([#1200](https://github.com/matter-labs/zksync-os-server/issues/1200)) ([5ee3f21](https://github.com/matter-labs/zksync-os-server/commit/5ee3f21444df61d8ca4a61f756a3a0a683a029bc))
* **rpc:** use log index in eth_getLogs to skip candidate blocks ([#1182](https://github.com/matter-labs/zksync-os-server/issues/1182)) ([8dc21c2](https://github.com/matter-labs/zksync-os-server/commit/8dc21c2d124f71a21c3879f6470288a4c28b25a0))
* **storage:** build RocksDB log index on block write/rollback ([#1158](https://github.com/matter-labs/zksync-os-server/issues/1158)) ([28a0d63](https://github.com/matter-labs/zksync-os-server/commit/28a0d63b57b9ca6aa45baedc5223c839765c9b0d))


### Bug Fixes

* BatchVerificationRunner shutdown ([#1184](https://github.com/matter-labs/zksync-os-server/issues/1184)) ([25048c5](https://github.com/matter-labs/zksync-os-server/commit/25048c5ba889ee1f9a45bb3809292b53dae34471))
* bump rustls-webpki to 0.103.12 ([#1177](https://github.com/matter-labs/zksync-os-server/issues/1177)) ([b427f45](https://github.com/matter-labs/zksync-os-server/commit/b427f45e2f4ae118e087dbaea20a57f25119f603))
* **deps:** patch rustls-webpki advisory ([#1203](https://github.com/matter-labs/zksync-os-server/issues/1203)) ([da7d944](https://github.com/matter-labs/zksync-os-server/commit/da7d944597270974a767d0f14ea02c02313df38b))
* minor issues that lead to panic/error ([#1181](https://github.com/matter-labs/zksync-os-server/issues/1181)) ([fa29459](https://github.com/matter-labs/zksync-os-server/commit/fa29459fbd89914cd8737e7a6e6247a2ee8b440a))
* record sl block used for l1 state fetch ([#1166](https://github.com/matter-labs/zksync-os-server/issues/1166)) ([76f44d2](https://github.com/matter-labs/zksync-os-server/commit/76f44d2ee13251fb804449d7f6d63576ebe885dc))
* resolve misc warnings ([#1180](https://github.com/matter-labs/zksync-os-server/issues/1180)) ([6f2ab9f](https://github.com/matter-labs/zksync-os-server/commit/6f2ab9f1f38a1575a303a0d52bceba35a83abad4))
* return back required confirmations to 1 ([#1205](https://github.com/matter-labs/zksync-os-server/issues/1205)) ([1f4a966](https://github.com/matter-labs/zksync-os-server/commit/1f4a966d7539a6b9a30c60e497eb418658b27724))
* **rpc:** correct error codes and add detailed rejection metrics for eth_sendRawTransaction ([#1199](https://github.com/matter-labs/zksync-os-server/issues/1199)) ([b9960d3](https://github.com/matter-labs/zksync-os-server/commit/b9960d3028c4b4870a2cd99e0e41b422688656e0))

## [0.19.0](https://github.com/matter-labs/zksync-os-server/compare/v0.18.2...v0.19.0) (2026-04-08)


### ⚠ BREAKING CHANGES

* **network:** move batch verification to devp2p ([#1149](https://github.com/matter-labs/zksync-os-server/issues/1149))

### Features

* add config option to `reset_timestamps` for rebuilt blocks ([#1157](https://github.com/matter-labs/zksync-os-server/issues/1157)) ([a59aaf9](https://github.com/matter-labs/zksync-os-server/commit/a59aaf974f867ff2dba3405f53aaade433814f71))
* **batcher:** SLI-based sealing ([#1130](https://github.com/matter-labs/zksync-os-server/issues/1130)) ([9e268e3](https://github.com/matter-labs/zksync-os-server/commit/9e268e3ea3db45bb07de7b6906125e40caf8bf79))
* **integration-tests:** expose fatal node errors from harness ([#1148](https://github.com/matter-labs/zksync-os-server/issues/1148)) ([a196f5b](https://github.com/matter-labs/zksync-os-server/commit/a196f5ba5027be32a880b05e2f12c7d3603d9e21))
* **network:** move batch verification to devp2p ([#1149](https://github.com/matter-labs/zksync-os-server/issues/1149)) ([7a140a7](https://github.com/matter-labs/zksync-os-server/commit/7a140a772034b2d01773ca37d3d5a81e03470ceb))
* **rpc:** track mempool insertion and forwarding latency for eth_sendRawTransaction ([#1153](https://github.com/matter-labs/zksync-os-server/issues/1153)) ([0caa6e2](https://github.com/matter-labs/zksync-os-server/commit/0caa6e2dbd136b0dabe4996355afe5face5df851))
* **storage_api:** add LogIndex supertrait to ReadRepository ([#1156](https://github.com/matter-labs/zksync-os-server/issues/1156)) ([27c1fcf](https://github.com/matter-labs/zksync-os-server/commit/27c1fcfc162d2588f6750592da3f52afd1f63231))
* update v31 state and zksync-os dev ([#1132](https://github.com/matter-labs/zksync-os-server/issues/1132)) ([59d50fc](https://github.com/matter-labs/zksync-os-server/commit/59d50fcb0ca8feb39c4c6edbec41d798596beb3e))
* Use log id for interop root indexing ([#1092](https://github.com/matter-labs/zksync-os-server/issues/1092)) ([8c074ba](https://github.com/matter-labs/zksync-os-server/commit/8c074baa182ce324337830117d728a9bc538e960))


### Bug Fixes

* **loadbase:** Increase max-in-flight from 10 to 1000 in loadbase ([#1136](https://github.com/matter-labs/zksync-os-server/issues/1136)) ([40585b7](https://github.com/matter-labs/zksync-os-server/commit/40585b7dfecaa934c19c1549ee08f009a68e548a))
* **mempool:** track all L2 transaction outflow paths in metrics ([#1151](https://github.com/matter-labs/zksync-os-server/issues/1151)) ([895dd18](https://github.com/matter-labs/zksync-os-server/commit/895dd183ee9feed720aedbb822f12738ed073d35))
* restart node on unexpected l1 commit ([#1147](https://github.com/matter-labs/zksync-os-server/issues/1147)) ([dd558fc](https://github.com/matter-labs/zksync-os-server/commit/dd558fc9c22edceba946e3adddd7b5fdfff70e60))
* **rpc:** map client errors to -32602, keep server errors at -32603 ([#1137](https://github.com/matter-labs/zksync-os-server/issues/1137)) ([fe3275f](https://github.com/matter-labs/zksync-os-server/commit/fe3275f556e0f3b01a8fe1984f5986f7237972cf))
* **rpc:** move wait_for_db_ready_to_process_blocks tobackground ([#1150](https://github.com/matter-labs/zksync-os-server/issues/1150)) ([5f1222f](https://github.com/matter-labs/zksync-os-server/commit/5f1222f08dc13073b18628be2b1deca60c7971eb))

## [0.18.2](https://github.com/matter-labs/zksync-os-server/compare/v0.18.1...v0.18.2) (2026-04-01)


### Features

* **batcher:** add config option to disable batcher subsystem ([#1119](https://github.com/matter-labs/zksync-os-server/issues/1119)) ([0ea9c7f](https://github.com/matter-labs/zksync-os-server/commit/0ea9c7f47bd7a52159d7e513d0e3352b9f2b6600))
* use `getSettlementLayer` to determine SL ([#1116](https://github.com/matter-labs/zksync-os-server/issues/1116)) ([dd1bc0c](https://github.com/matter-labs/zksync-os-server/commit/dd1bc0c0dc1bd3b84c0c6615102777fa314a2520))


### Bug Fixes

* fix batcher shutdown and enable revert test ([#1122](https://github.com/matter-labs/zksync-os-server/issues/1122)) ([a846796](https://github.com/matter-labs/zksync-os-server/commit/a846796043ccc6a44ef9800a7245105a7e49ad10))
* **prover_api:** use drop guards to ensure metrics are always recorded ([#1126](https://github.com/matter-labs/zksync-os-server/issues/1126)) ([e5df063](https://github.com/matter-labs/zksync-os-server/commit/e5df0636b80960a02e6d9476a7a220aa6de7610e))
* **rpc:** record latency for cancelled requests (client disconnect) ([#1125](https://github.com/matter-labs/zksync-os-server/issues/1125)) ([7027315](https://github.com/matter-labs/zksync-os-server/commit/7027315cee8c9943ac5b3ac85989678438da3deb))
* **tracer:** Fix tracer only_top_call handling ([#1127](https://github.com/matter-labs/zksync-os-server/issues/1127)) ([9a88dc1](https://github.com/matter-labs/zksync-os-server/commit/9a88dc13ddb0b761c99dbabf754dbbbf3f8dfc13))

## [0.18.1](https://github.com/matter-labs/zksync-os-server/compare/v0.18.0...v0.18.1) (2026-03-30)


### Features

* **config:** generate declarative config validation ([#1090](https://github.com/matter-labs/zksync-os-server/issues/1090)) ([491b930](https://github.com/matter-labs/zksync-os-server/commit/491b93036e144aa0afc178e07cc07cec87eaaf2e))
* **config:** set production-oriented defaults, extract local dev overrides ([#1062](https://github.com/matter-labs/zksync-os-server/issues/1062)) ([5e850f5](https://github.com/matter-labs/zksync-os-server/commit/5e850f54b0be944b3d1b1ae33f20a2edf9f9fd05))
* **minor:** improve logging of executed transactions ([#1094](https://github.com/matter-labs/zksync-os-server/issues/1094)) ([da4a52a](https://github.com/matter-labs/zksync-os-server/commit/da4a52a31895d308eedc08f672e5b7781825ee94))
* **network:** report metrics from `reth-network` crate ([#1063](https://github.com/matter-labs/zksync-os-server/issues/1063)) ([2e1ec9d](https://github.com/matter-labs/zksync-os-server/commit/2e1ec9d114ca6e325fb05f7a3240c7997d2326cd))
* **network:** support `network_interface` and DNS boot nodes ([#1075](https://github.com/matter-labs/zksync-os-server/issues/1075)) ([f2afb8d](https://github.com/matter-labs/zksync-os-server/commit/f2afb8d544dfad2159ff496ab1c6bec3883e0e7e))
* **tracer:** Meaningful errors for out-of-pubdata reverts ([#1058](https://github.com/matter-labs/zksync-os-server/issues/1058)) ([e62b216](https://github.com/matter-labs/zksync-os-server/commit/e62b216758b795f40061691db497c9f51d1899ba))


### Bug Fixes

* avoid marking rebuild tx source entries invalid ([#1109](https://github.com/matter-labs/zksync-os-server/issues/1109)) ([c5c339f](https://github.com/matter-labs/zksync-os-server/commit/c5c339f345fcdae447fcc02a5a29b13045184758))
* EN preimage persisting before block saved ([#1114](https://github.com/matter-labs/zksync-os-server/issues/1114)) ([fafd039](https://github.com/matter-labs/zksync-os-server/commit/fafd03988cc1ddf4b5a794a3fc24573c7ef9d754))
* fix batch storage in revert case ([#1081](https://github.com/matter-labs/zksync-os-server/issues/1081)) ([4cce328](https://github.com/matter-labs/zksync-os-server/commit/4cce328f3fbe5df9d44ce181e2ccdef2b8e711a4))
* handle already reverted batches in commit watcher ([#1096](https://github.com/matter-labs/zksync-os-server/issues/1096)) ([887ff33](https://github.com/matter-labs/zksync-os-server/commit/887ff3315fa4e2f7b3dbe4457352533c5063af61))
* ignore permission denied for stale port lockfiles ([#1088](https://github.com/matter-labs/zksync-os-server/issues/1088)) ([edafd60](https://github.com/matter-labs/zksync-os-server/commit/edafd604db1d95b8693a73d27d60dd0922800047))
* **l1-watcher:** wait two L1 blocks before processing events ([#1091](https://github.com/matter-labs/zksync-os-server/issues/1091)) ([2afb0b4](https://github.com/matter-labs/zksync-os-server/commit/2afb0b49c07311c719a312d0957e754475babc8e))
* make block executor wait for block applier ([#1108](https://github.com/matter-labs/zksync-os-server/issues/1108)) ([0e23999](https://github.com/matter-labs/zksync-os-server/commit/0e23999e2bb05625152423b50211a095e61c3ca7))
* **network:** retry boot node DNS resolution before startup ([#1100](https://github.com/matter-labs/zksync-os-server/issues/1100)) ([4a635d5](https://github.com/matter-labs/zksync-os-server/commit/4a635d555eb173b110d7395ed884d6abe9e01b8f))
* **priority-tree:** run initialization in background to avoid shutdown bug ([#1067](https://github.com/matter-labs/zksync-os-server/issues/1067)) ([debea8f](https://github.com/matter-labs/zksync-os-server/commit/debea8fab1bac9e83337b684900cd57f71736173))
* **tracer:** map CREATE and CREATE2 correctly ([#1060](https://github.com/matter-labs/zksync-os-server/issues/1060)) ([553e627](https://github.com/matter-labs/zksync-os-server/commit/553e627241a8390eba5fbd7a3affb034df6a9316))

## [0.18.0](https://github.com/matter-labs/zksync-os-server/compare/v0.17.1...v0.18.0) (2026-03-24)


### ⚠ BREAKING CHANGES

* **network:** use chain-aware fork id for filtering discv5 peers ([#1051](https://github.com/matter-labs/zksync-os-server/issues/1051))

### Features

* Add set SL chain Id tx after upgrade ([#1047](https://github.com/matter-labs/zksync-os-server/issues/1047)) ([119e315](https://github.com/matter-labs/zksync-os-server/commit/119e315c02394e9b66638748ff2f082392120709))
* add trace logs to estimate gas with exec results ([#1044](https://github.com/matter-labs/zksync-os-server/issues/1044)) ([0bb4532](https://github.com/matter-labs/zksync-os-server/commit/0bb45329c4e428bc8d57dfc694d6ab1b8bee3ce2))
* consensus integration 2/5: Consensus interface, raft dependency ([#958](https://github.com/matter-labs/zksync-os-server/issues/958)) ([6e88dea](https://github.com/matter-labs/zksync-os-server/commit/6e88dead05f265abf167aceca3e6e84dbf8ecb8f))
* **minor:** small logging and test cleanups ([#1057](https://github.com/matter-labs/zksync-os-server/issues/1057)) ([df40c62](https://github.com/matter-labs/zksync-os-server/commit/df40c62a19a3380cac5aa5d4be4f87bb025e60c3))
* **multivm:** use in-memory app bins for PIG ([#1037](https://github.com/matter-labs/zksync-os-server/issues/1037)) ([49705f6](https://github.com/matter-labs/zksync-os-server/commit/49705f62fcbe7305e8512f0716f7bb2e2e7f7ebe))
* **network:** use chain-aware fork id for filtering discv5 peers ([#1051](https://github.com/matter-labs/zksync-os-server/issues/1051)) ([e9b3586](https://github.com/matter-labs/zksync-os-server/commit/e9b35864d2cbcc1a43f7cfab30aa00410375bbbf))
* **readctor `ReplayRecord`:** extract `BlockStartCursors` struct from flat cursor fields (eg `l1_priority_id`) ([#1034](https://github.com/matter-labs/zksync-os-server/issues/1034)) ([2b6ed46](https://github.com/matter-labs/zksync-os-server/commit/2b6ed46fb040cb44073f97f6e5d9374e936d63d4))
* **rpc:** add gatewayBlockNumber to zks_getL2ToL1LogProof response ([#1064](https://github.com/matter-labs/zksync-os-server/issues/1064)) ([daad643](https://github.com/matter-labs/zksync-os-server/commit/daad6431d1965347ad4966c0b740abd4e08c5dd6))
* **rpc:** Implement `zks_getProof` ([#917](https://github.com/matter-labs/zksync-os-server/issues/917)) ([4c6b676](https://github.com/matter-labs/zksync-os-server/commit/4c6b67642b3213a6e29b27f91aa77293694a2a0e))
* **rpc:** track JSON-RPC error counts by method and error code ([#1040](https://github.com/matter-labs/zksync-os-server/issues/1040)) ([ba5821a](https://github.com/matter-labs/zksync-os-server/commit/ba5821a6bd47abc396c30196f3af475d44fd37f3))
* Sync l1 state with draft-v31 ([#1010](https://github.com/matter-labs/zksync-os-server/issues/1010)) ([2c9fa7a](https://github.com/matter-labs/zksync-os-server/commit/2c9fa7a4c79797712fa85ed668e3165ea64d1eeb))
* **tx_validators:** add deployment filter to restrict contract deployments to an allow-list ([#1013](https://github.com/matter-labs/zksync-os-server/issues/1013)) ([f61b2ec](https://github.com/matter-labs/zksync-os-server/commit/f61b2ecc70ad91c6f666742ef57907949d0fadab))
* Use gateway base token as SL token ([#1042](https://github.com/matter-labs/zksync-os-server/issues/1042)) ([025df77](https://github.com/matter-labs/zksync-os-server/commit/025df77f99402548db4ac204ec5c156f19060be1))
* **zks_getProof:** add L1 verification data to proof response and CLI tool ([#1022](https://github.com/matter-labs/zksync-os-server/issues/1022)) ([fa34042](https://github.com/matter-labs/zksync-os-server/commit/fa34042da3139c5d08fbf5a1a32b8a90ba4c7b27))


### Bug Fixes

* get rid of default debug logs ([#939](https://github.com/matter-labs/zksync-os-server/issues/939)) ([bfb3bd3](https://github.com/matter-labs/zksync-os-server/commit/bfb3bd3de3aeb75deb2d66a3af04becde469cbf3))
* **l1_sender:** fix bug in `parallel_transactions` metric ([#996](https://github.com/matter-labs/zksync-os-server/issues/996)) ([3df0b64](https://github.com/matter-labs/zksync-os-server/commit/3df0b64424678b7cfc97ba97c11cddf1253cd08a))
* **rpc:** Fix `zks_getProof` ([#1032](https://github.com/matter-labs/zksync-os-server/issues/1032)) ([352b7db](https://github.com/matter-labs/zksync-os-server/commit/352b7db30dd7fc4fac717c49f0d43d88f9a80993))
* upgrade lz4_flex to 0.12.1 to address RUSTSEC-2026-0041 ([#1024](https://github.com/matter-labs/zksync-os-server/issues/1024)) ([22e1bee](https://github.com/matter-labs/zksync-os-server/commit/22e1bee73b34de5b99dfcb97986fac18e35ce2c4))

## [0.17.1](https://github.com/matter-labs/zksync-os-server/compare/v0.17.0...v0.17.1) (2026-03-16)


### Bug Fixes

* batch storage persist delay ([#1015](https://github.com/matter-labs/zksync-os-server/issues/1015)) ([ce075bc](https://github.com/matter-labs/zksync-os-server/commit/ce075bcf740ec5842fd4d2d2cdad5194c1333c70))

## [0.17.0](https://github.com/matter-labs/zksync-os-server/compare/v0.16.0...v0.17.0) (2026-03-16)


### ⚠ BREAKING CHANGES

* Remove unnecessary configs for EN ([#986](https://github.com/matter-labs/zksync-os-server/issues/986))
* Store FRI proofs locally, not in S3 ([#891](https://github.com/matter-labs/zksync-os-server/issues/891))
* Commit encoding v4 support ([#899](https://github.com/matter-labs/zksync-os-server/issues/899))

### Features

* add gateway interop fee updater ([#968](https://github.com/matter-labs/zksync-os-server/issues/968)) ([fe50e31](https://github.com/matter-labs/zksync-os-server/commit/fe50e31ba453f0aa24a192a71fabdd3ea6779f01))
* Add proper gateway migration watcher ([#921](https://github.com/matter-labs/zksync-os-server/issues/921)) ([c9e3622](https://github.com/matter-labs/zksync-os-server/commit/c9e36227614d63bf2d36ebd6d58c22436e8ffadf))
* Adding operator signing with HSM ([#956](https://github.com/matter-labs/zksync-os-server/issues/956)) ([5008730](https://github.com/matter-labs/zksync-os-server/commit/5008730299c23f6fb5c0bfcd4965620ba49b9e41))
* Bump zksync-os dev version ([#911](https://github.com/matter-labs/zksync-os-server/issues/911)) ([2bab2b8](https://github.com/matter-labs/zksync-os-server/commit/2bab2b8be287b882541d456a0cab26ab3407b336))
* Commit encoding v4 support ([#899](https://github.com/matter-labs/zksync-os-server/issues/899)) ([f95ddbd](https://github.com/matter-labs/zksync-os-server/commit/f95ddbdc837b035bc944009ae4dfce47c5579e9d))
* consensus integration 1/5: Sequencer split in BlockExecutor and BlockApplier ([#953](https://github.com/matter-labs/zksync-os-server/issues/953)) ([2f588c2](https://github.com/matter-labs/zksync-os-server/commit/2f588c2c43db788e8e2e27bed8ee38cbb75e1001))
* **genesis:** derive execution_version from protocol version, remove from genesis.json ([#940](https://github.com/matter-labs/zksync-os-server/issues/940)) ([38a77fa](https://github.com/matter-labs/zksync-os-server/commit/38a77facc7e3cfbb481ebf5e9710c2bb0338be3b))
* make operator signing keys optional for External Nodes ([#929](https://github.com/matter-labs/zksync-os-server/issues/929)) ([3894215](https://github.com/matter-labs/zksync-os-server/commit/38942150d7c9b8fe106ed5d3e9c136d435cae01c))
* **merkle-tree:** Implement storage proofs for `zks_getProof` ([#904](https://github.com/matter-labs/zksync-os-server/issues/904)) ([eaa38d3](https://github.com/matter-labs/zksync-os-server/commit/eaa38d36818b6d64a11565b6032745c4afa2df12))
* proper gateway settlement and local gateway setup ([#919](https://github.com/matter-labs/zksync-os-server/issues/919)) ([14b202f](https://github.com/matter-labs/zksync-os-server/commit/14b202f5c4a5bed26c308014e83cb00ed4a46bb4))
* **rpc:** Additional format of l2_to_l1_log_proof ([#964](https://github.com/matter-labs/zksync-os-server/issues/964)) ([6397e96](https://github.com/matter-labs/zksync-os-server/commit/6397e968b47260ff922640bbec59bd4c83d9ec33))
* scale eth_gasPrice by configurable factor ([#957](https://github.com/matter-labs/zksync-os-server/issues/957)) ([2240028](https://github.com/matter-labs/zksync-os-server/commit/2240028eee22c26a69082762767ae5595ce61bed))
* some gateway features ([#886](https://github.com/matter-labs/zksync-os-server/issues/886)) ([ba995d7](https://github.com/matter-labs/zksync-os-server/commit/ba995d72d24b41769439ab85966f667d8265294a))
* Store FRI proofs locally, not in S3 ([#891](https://github.com/matter-labs/zksync-os-server/issues/891)) ([2895b90](https://github.com/matter-labs/zksync-os-server/commit/2895b903e4f7c3929ec7f9f4e73944887543f475))
* update rustc version; use prover binary in test ([#901](https://github.com/matter-labs/zksync-os-server/issues/901)) ([2ca6c08](https://github.com/matter-labs/zksync-os-server/commit/2ca6c086a9974f97c1641cf75744af7099690d2c))


### Bug Fixes

* Add more metrics for 2FA ([#1001](https://github.com/matter-labs/zksync-os-server/issues/1001)) ([ccb9ce8](https://github.com/matter-labs/zksync-os-server/commit/ccb9ce80af4001978e9b66631831cfc2aa071b0a))
* Compare block hash during block replay ([#918](https://github.com/matter-labs/zksync-os-server/issues/918)) ([039a9ba](https://github.com/matter-labs/zksync-os-server/commit/039a9ba38e08c11ae4be8c80691988b640b3e3fc))
* Decouple v1 batch verification transport ([#997](https://github.com/matter-labs/zksync-os-server/issues/997)) ([53c09b5](https://github.com/matter-labs/zksync-os-server/commit/53c09b542b83c9d32d46f02a5a18c74f828410f9))
* do not do migration to set `execute_sl_block_number` for old batches ([#976](https://github.com/matter-labs/zksync-os-server/issues/976)) ([c4a00c7](https://github.com/matter-labs/zksync-os-server/commit/c4a00c76c0967d95e14b64ec45c21d220241f6dc))
* fix legacy batch processing in persist batch watcher ([#975](https://github.com/matter-labs/zksync-os-server/issues/975)) ([e1d07c7](https://github.com/matter-labs/zksync-os-server/commit/e1d07c71fd46777f019a258516fa71a6f84922b4))
* keep `StoredBatchInfo::last_block_timestamp` ([#977](https://github.com/matter-labs/zksync-os-server/issues/977)) ([73e5fe5](https://github.com/matter-labs/zksync-os-server/commit/73e5fe5c502f74e8c2932295b680c0611751d19d))
* mempool pending fee refresh ([#955](https://github.com/matter-labs/zksync-os-server/issues/955)) ([07693c8](https://github.com/matter-labs/zksync-os-server/commit/07693c84cdca821efc779dd2180c6cb438e2e850))
* multivm app path caching across tempdirs ([#948](https://github.com/matter-labs/zksync-os-server/issues/948)) ([69a457d](https://github.com/matter-labs/zksync-os-server/commit/69a457d8cc61c70c0e835ebdcbbc256e03e59efd))
* Remove unnecessary configs for EN ([#986](https://github.com/matter-labs/zksync-os-server/issues/986)) ([b68775b](https://github.com/matter-labs/zksync-os-server/commit/b68775b670ad5a67d54ff1f82b34d08465318986))
* rename aggregated root to multichain root ([#924](https://github.com/matter-labs/zksync-os-server/issues/924)) ([6cbc17b](https://github.com/matter-labs/zksync-os-server/commit/6cbc17b5886444284b12abc65640ba2fbe420a2d))
* retry on pending commit tx in L1 watcher instead of panicking ([#952](https://github.com/matter-labs/zksync-os-server/issues/952)) ([8589852](https://github.com/matter-labs/zksync-os-server/commit/8589852c24cd59235cb25e5748589896e942cc22))
* **rpc:** adjust latency histogram bucket range (1µs-32s) ([#990](https://github.com/matter-labs/zksync-os-server/issues/990)) ([10e4200](https://github.com/matter-labs/zksync-os-server/commit/10e4200960b3bd36b30addefc54b011e08a9ec03))
* **rpc:** camelCase `batchNumber` is L2-&gt;L1 log proof ([#923](https://github.com/matter-labs/zksync-os-server/issues/923)) ([9f8bcdd](https://github.com/matter-labs/zksync-os-server/commit/9f8bcdd472af299615b08ea8718f98b3a67a853f))
* **rpc:** lower eth_getLogs default limits to match industry standard ([#992](https://github.com/matter-labs/zksync-os-server/issues/992)) ([4f51503](https://github.com/matter-labs/zksync-os-server/commit/4f51503f353d89f87046cb0be21c005cd6e0a606))
* **sequencer:** handle low-fee L2 transactions without stalling block production ([#927](https://github.com/matter-labs/zksync-os-server/issues/927)) ([c0d7385](https://github.com/matter-labs/zksync-os-server/commit/c0d73850edc30223af6965bc86f98408c3da6f37))
* support 0x-prefixed hex in all config fields ([#931](https://github.com/matter-labs/zksync-os-server/issues/931)) ([a876bec](https://github.com/matter-labs/zksync-os-server/commit/a876becd96231a5a0b109f7ad5377310283482a1))
* **tests:** decompress L1 state in build.rs instead of in-process cache ([#966](https://github.com/matter-labs/zksync-os-server/issues/966)) ([03ae2ed](https://github.com/matter-labs/zksync-os-server/commit/03ae2ed7fb110ecac99bdfa9a893d89768a8992e))
* Use warn for server disconnects ([#998](https://github.com/matter-labs/zksync-os-server/issues/998)) ([266e134](https://github.com/matter-labs/zksync-os-server/commit/266e134cadf575680cabed60ebbfb2877beba244))
* Warn on batch verification threshold mismatch ([#984](https://github.com/matter-labs/zksync-os-server/issues/984)) ([ad0fcab](https://github.com/matter-labs/zksync-os-server/commit/ad0fcab716fc0485c17c842815fb497285127852))

## [0.16.0](https://github.com/matter-labs/zksync-os-server/compare/v0.15.1...v0.16.0) (2026-02-25)


### ⚠ BREAKING CHANGES

* **network:** fully migrate replay transport to p2p network ([#873](https://github.com/matter-labs/zksync-os-server/issues/873))
* change api l2 l1 log format ([#875](https://github.com/matter-labs/zksync-os-server/issues/875))

### Features

* add block hash to revm divergence panic message ([#880](https://github.com/matter-labs/zksync-os-server/issues/880)) ([92a9eaf](https://github.com/matter-labs/zksync-os-server/commit/92a9eafb6c5e89eb27f931a9e9892b99334323ac))
* **batch-verification:** make HTTPS connection a 2-way stream ([#862](https://github.com/matter-labs/zksync-os-server/issues/862)) ([a96e9a0](https://github.com/matter-labs/zksync-os-server/commit/a96e9a0974f7d13d86a3eaa9ab8ef03f9ebe5f29))
* change api l2 l1 log format ([#875](https://github.com/matter-labs/zksync-os-server/issues/875)) ([26ea56f](https://github.com/matter-labs/zksync-os-server/commit/26ea56f6e84febede0278995dbdd5c670c36eb88))
* index reverted blocks by hash ([#867](https://github.com/matter-labs/zksync-os-server/issues/867)) ([8e360fb](https://github.com/matter-labs/zksync-os-server/commit/8e360fb75f774e8acee65ef1c380308a4e7ece61))
* **mempool:** rewrite via in-memory subpools ([#869](https://github.com/matter-labs/zksync-os-server/issues/869)) ([b3bbca8](https://github.com/matter-labs/zksync-os-server/commit/b3bbca84481b624b959a00af98cb06f0af459927))
* **network:** bounded channel + shared starting block state ([#884](https://github.com/matter-labs/zksync-os-server/issues/884)) ([5de34e2](https://github.com/matter-labs/zksync-os-server/commit/5de34e26404e727ee0f2e92258714c12a6f73547))
* **network:** fully migrate replay transport to p2p network ([#873](https://github.com/matter-labs/zksync-os-server/issues/873)) ([a8e963a](https://github.com/matter-labs/zksync-os-server/commit/a8e963a00aa287a625eb57e63a628ec22101de10))


### Bug Fixes

* Apply fixes for cargo deny ([#892](https://github.com/matter-labs/zksync-os-server/issues/892)) ([e4eef3c](https://github.com/matter-labs/zksync-os-server/commit/e4eef3c99011aac6b7da6aeea8017d093292d0d5))
* Commit after each tx in revm consistency checker ([#898](https://github.com/matter-labs/zksync-os-server/issues/898)) ([384ff31](https://github.com/matter-labs/zksync-os-server/commit/384ff3134e2ba9dcaed035818845428a2c338647))
* get rid of broadcast in mempool ([#910](https://github.com/matter-labs/zksync-os-server/issues/910)) ([01b53fd](https://github.com/matter-labs/zksync-os-server/commit/01b53fd8dc2b23e7c38c9f4ec62e48d3643a76b8))
* remove transaction r and s paddings ([#890](https://github.com/matter-labs/zksync-os-server/issues/890)) ([3079e59](https://github.com/matter-labs/zksync-os-server/commit/3079e5968def203a09debd34f119dff79c04f700))
* **rpc:** return hex-encoded subscription ids ([#877](https://github.com/matter-labs/zksync-os-server/issues/877)) ([0dbc703](https://github.com/matter-labs/zksync-os-server/commit/0dbc703741dbd429960d9397e250581047016d5d))

## [0.15.1](https://github.com/matter-labs/zksync-os-server/compare/v0.15.0...v0.15.1) (2026-02-10)


### Bug Fixes

* **eth-watch:** don't save batches with divergent hashes ([#871](https://github.com/matter-labs/zksync-os-server/issues/871)) ([5254754](https://github.com/matter-labs/zksync-os-server/commit/52547541f8ea7f3db819bb5ea90f279ee4db6d5f))

## [0.15.0](https://github.com/matter-labs/zksync-os-server/compare/v0.14.2...v0.15.0) (2026-02-10)


### ⚠ BREAKING CHANGES

* drop proving support for v29.x and v30.0 versions ([#822](https://github.com/matter-labs/zksync-os-server/issues/822))

### Features

* Accumulated interop txs ([#848](https://github.com/matter-labs/zksync-os-server/issues/848)) ([feaeeea](https://github.com/matter-labs/zksync-os-server/commit/feaeeeaaaddb44be5521eaa8d1a4ab829ea43bbd))
* drop proving support for v29.x and v30.0 versions ([#822](https://github.com/matter-labs/zksync-os-server/issues/822)) ([f157dbb](https://github.com/matter-labs/zksync-os-server/commit/f157dbbdf30a49b68ccfc60c555a62732ed6cb9a))
* **multivm:** use v0.2.6-simulate-only for V5 simulation ([#855](https://github.com/matter-labs/zksync-os-server/issues/855)) ([c21a107](https://github.com/matter-labs/zksync-os-server/commit/c21a107f4b344e02d8d799d81c8472769d7d67cc))
* Set SL chain id txs ([#849](https://github.com/matter-labs/zksync-os-server/issues/849)) ([f561a9e](https://github.com/matter-labs/zksync-os-server/commit/f561a9e0feb1cf5b4d8036f05d2d3f574915d6be))
* store gzip-compressed anvil states ([#837](https://github.com/matter-labs/zksync-os-server/issues/837)) ([d231609](https://github.com/matter-labs/zksync-os-server/commit/d231609035533db253bb09b6002197286ff2a8e0))
* support multiple config files ([#866](https://github.com/matter-labs/zksync-os-server/issues/866)) ([319b2f9](https://github.com/matter-labs/zksync-os-server/commit/319b2f9b311c23e1292e7880b3c8e41fabf686e5))
* use max_priority_fee_per_gas config value as cap on the priority fee used ([#857](https://github.com/matter-labs/zksync-os-server/issues/857)) ([2331595](https://github.com/matter-labs/zksync-os-server/commit/233159524f069410e23c9059cac84649a00ace8f))


### Bug Fixes

* better recognition for missing `IMultisigCommitter` ([#852](https://github.com/matter-labs/zksync-os-server/issues/852)) ([9e07c51](https://github.com/matter-labs/zksync-os-server/commit/9e07c518bbc2b0bbc5ba7e6e52703b911879adb2))
* **l1-watcher:** skip persisting legacy batches ([#860](https://github.com/matter-labs/zksync-os-server/issues/860)) ([9d818fd](https://github.com/matter-labs/zksync-os-server/commit/9d818fd223183cddf8b51bf3b4cc08693961bf9d))
* rebuild_from_block assert for EN ([#864](https://github.com/matter-labs/zksync-os-server/issues/864)) ([fa2c6c6](https://github.com/matter-labs/zksync-os-server/commit/fa2c6c64b8c6a122b64ae462f6197d1169f42bda))
* **rpc:** respect 0 gas price during gas estimation ([#865](https://github.com/matter-labs/zksync-os-server/issues/865)) ([ed80197](https://github.com/matter-labs/zksync-os-server/commit/ed80197dc1d5d826064dfb37550127a38d04114e))
* Update time crate to 0.3.47 to address security vulnerability ([#870](https://github.com/matter-labs/zksync-os-server/issues/870)) ([82a0537](https://github.com/matter-labs/zksync-os-server/commit/82a05377bb5929956ccea8d4f5ed76decb31f449))

## [0.14.2](https://github.com/matter-labs/zksync-os-server/compare/v0.14.1...v0.14.2) (2026-01-29)


### Features

* add metric for base fee and native price ([#844](https://github.com/matter-labs/zksync-os-server/issues/844)) ([3aa0b70](https://github.com/matter-labs/zksync-os-server/commit/3aa0b709068d0b01a585e3639346cb152d818da9))
* add pubdata price cap ([#842](https://github.com/matter-labs/zksync-os-server/issues/842)) ([9d9803d](https://github.com/matter-labs/zksync-os-server/commit/9d9803d94a20372e13966d0985e0e637a05b389a))
* do not require S3 for RPC ([#827](https://github.com/matter-labs/zksync-os-server/issues/827)) ([a923d83](https://github.com/matter-labs/zksync-os-server/commit/a923d833f876063f02b7c6cddffe350692b1180f))
* validate genesis batch info against L1 ([#832](https://github.com/matter-labs/zksync-os-server/issues/832)) ([affbc1f](https://github.com/matter-labs/zksync-os-server/commit/affbc1f31deafe35d39955320bd5ab2aef970ae8))


### Bug Fixes

* increase default value for `estimate_gas_pubdata_price_factor` ([#831](https://github.com/matter-labs/zksync-os-server/issues/831)) ([6180db3](https://github.com/matter-labs/zksync-os-server/commit/6180db314fbfca08fbab6e7c9f4f33eeb71b22bc))

## [0.14.1](https://github.com/matter-labs/zksync-os-server/compare/v0.14.0...v0.14.1) (2026-01-27)


### Features

* Add metric for blacklisted addresses count ([#820](https://github.com/matter-labs/zksync-os-server/issues/820)) ([078368a](https://github.com/matter-labs/zksync-os-server/commit/078368a4e80ea3ac3f2d0418a3a99bc901cc5f00))
* do not require batch storage for priority tree ([#825](https://github.com/matter-labs/zksync-os-server/issues/825)) ([6a73d20](https://github.com/matter-labs/zksync-os-server/commit/6a73d2031567d9a2c281d807164bd3637d7184b0))


### Bug Fixes

* **rpc:** revert "make `eth_estimateGas` work when sender has no balance ([#807](https://github.com/matter-labs/zksync-os-server/issues/807))" ([#826](https://github.com/matter-labs/zksync-os-server/issues/826)) ([e1018d6](https://github.com/matter-labs/zksync-os-server/commit/e1018d6da7031bf07aa030f14ff7f0d0d0344b70))


### Performance Improvements

* speed up priority tree init for EN ([#824](https://github.com/matter-labs/zksync-os-server/issues/824)) ([5e1b951](https://github.com/matter-labs/zksync-os-server/commit/5e1b95127f4c272d14989b5763ab8bd28a400ec2))

## [0.14.0](https://github.com/matter-labs/zksync-os-server/compare/v0.13.0...v0.14.0) (2026-01-23)


### ⚠ BREAKING CHANGES

* Execution of service interop transactions ([#803](https://github.com/matter-labs/zksync-os-server/issues/803))
* use token prices in fee model ([#787](https://github.com/matter-labs/zksync-os-server/issues/787))
* token price updater component ([#779](https://github.com/matter-labs/zksync-os-server/issues/779))
* Basic V31 Support ([#759](https://github.com/matter-labs/zksync-os-server/issues/759))

### Features

* 2FA L1 integration ([#726](https://github.com/matter-labs/zksync-os-server/issues/726)) ([43a466f](https://github.com/matter-labs/zksync-os-server/commit/43a466fd341532bdbdc79642e74b86639aad7b6a))
* add bash script to run local chains ([#777](https://github.com/matter-labs/zksync-os-server/issues/777)) ([b786ad8](https://github.com/matter-labs/zksync-os-server/commit/b786ad8ef27a728c6394eb286aa00e48b061f4ea))
* add more eth-sender metrics. Bump fee limit. ([#789](https://github.com/matter-labs/zksync-os-server/issues/789)) ([6b6f13b](https://github.com/matter-labs/zksync-os-server/commit/6b6f13b648739e92bc7f2356b1e2b67dca1da87e))
* add support for YAML config files ([#785](https://github.com/matter-labs/zksync-os-server/issues/785)) ([5f3de80](https://github.com/matter-labs/zksync-os-server/commit/5f3de80747df543202496e737b37ec528bf2b3bb))
* add toHex helper for JS tracer ([#761](https://github.com/matter-labs/zksync-os-server/issues/761)) ([f9e14aa](https://github.com/matter-labs/zksync-os-server/commit/f9e14aa0ccc49c53d052b9425294eeb1d8776453))
* adjust pubdata price based on blob fill ratio ([#700](https://github.com/matter-labs/zksync-os-server/issues/700)) ([a8e6de4](https://github.com/matter-labs/zksync-os-server/commit/a8e6de4f4f260ab33bb2ac57c441c0bec4a8fb2c))
* adjust pubdata price based on blob fill ratio (2nd attempt) ([#756](https://github.com/matter-labs/zksync-os-server/issues/756)) ([167d874](https://github.com/matter-labs/zksync-os-server/commit/167d874bfd4e5e4870ba85405a2a1fbdfd22ac5c))
* Basic V31 Support ([#759](https://github.com/matter-labs/zksync-os-server/issues/759)) ([1103ab8](https://github.com/matter-labs/zksync-os-server/commit/1103ab882b6e7ccc94db08375cb2049cb142e5e5))
* **batcher:** make the limit of transaction count per batch configurable ([#796](https://github.com/matter-labs/zksync-os-server/issues/796)) ([f09de09](https://github.com/matter-labs/zksync-os-server/commit/f09de09e30f586244967e94bb74bd24a0dac76e9))
* **deposit tool:** Make it work with https provider; use ether as unit ([#794](https://github.com/matter-labs/zksync-os-server/issues/794)) ([c6b7839](https://github.com/matter-labs/zksync-os-server/commit/c6b78399be1c4b308d333a85963379b455064169))
* do not require batch storage (S3) for ENs ([#810](https://github.com/matter-labs/zksync-os-server/issues/810)) ([d542f07](https://github.com/matter-labs/zksync-os-server/commit/d542f0777f53c5df5591139448da864a77bc1763))
* Execution of service interop transactions ([#803](https://github.com/matter-labs/zksync-os-server/issues/803)) ([20f5ed2](https://github.com/matter-labs/zksync-os-server/commit/20f5ed296c4913a9cd9964f34ce545eec18fce8d))
* ignore vulnerability to recover cargo-audit ([#754](https://github.com/matter-labs/zksync-os-server/issues/754)) ([309887e](https://github.com/matter-labs/zksync-os-server/commit/309887efe8ed2355d802a60319f72a6d1d5b22cc))
* Implement interop system transaction ([#712](https://github.com/matter-labs/zksync-os-server/issues/712)) ([0310dbc](https://github.com/matter-labs/zksync-os-server/commit/0310dbc504b254596f088a3f66c7206104293981))
* Interop roots watcher ([#819](https://github.com/matter-labs/zksync-os-server/issues/819)) ([66c8fc5](https://github.com/matter-labs/zksync-os-server/commit/66c8fc5f41933ad074c45f3a65a43265a61abf72))
* introduce `CommittedBatchProvider` ([#764](https://github.com/matter-labs/zksync-os-server/issues/764)) ([d3a1cf4](https://github.com/matter-labs/zksync-os-server/commit/d3a1cf4859186a024e54191a061dbeecd16fb864))
* make block-related logging consistent ([#792](https://github.com/matter-labs/zksync-os-server/issues/792)) ([485c13c](https://github.com/matter-labs/zksync-os-server/commit/485c13cd21a9e2dc385e7de9efd01e8c54e2888b))
* more granular buckets for `prove_time_per_million_native` ([#763](https://github.com/matter-labs/zksync-os-server/issues/763)) ([4e0fe7d](https://github.com/matter-labs/zksync-os-server/commit/4e0fe7dbb269981600e63eacfec00147639a0dc9))
* **network:** add runnable `NetworkService` (disabled by default) ([#773](https://github.com/matter-labs/zksync-os-server/issues/773)) ([88fdf39](https://github.com/matter-labs/zksync-os-server/commit/88fdf39a42cf054a92193badf0443b97a33bba6e))
* **network:** implement bare-bones `zks` RLPx subprotocol ([#716](https://github.com/matter-labs/zksync-os-server/issues/716)) ([417c6ad](https://github.com/matter-labs/zksync-os-server/commit/417c6ad00d73f5e4add37f4db08d0bc4e2699eeb))
* record prove time per native ([#757](https://github.com/matter-labs/zksync-os-server/issues/757)) ([63fd801](https://github.com/matter-labs/zksync-os-server/commit/63fd801284a53da3bdff08ecf2f1ddf2053eb6bc))
* remove hardcoded config constants ([#762](https://github.com/matter-labs/zksync-os-server/issues/762)) ([adfc998](https://github.com/matter-labs/zksync-os-server/commit/adfc99875228a0bd3cd8945504355d8fe6dcf478))
* return zeroes in `reward` in `eth_feeHistory` ([#800](https://github.com/matter-labs/zksync-os-server/issues/800)) ([8f09ae7](https://github.com/matter-labs/zksync-os-server/commit/8f09ae7c89a409ceb4fa7fc2eef2da19385441eb))
* Revert "feat: adjust pubdata price based on blob fill ratio" ([#753](https://github.com/matter-labs/zksync-os-server/issues/753)) ([d7a7f54](https://github.com/matter-labs/zksync-os-server/commit/d7a7f54141b9db61773cba6235409f8aa7fdf347))
* set total difficulty in rpc block headers ([#801](https://github.com/matter-labs/zksync-os-server/issues/801)) ([6dac957](https://github.com/matter-labs/zksync-os-server/commit/6dac957fc826d89485a0e5f1eb26b91a1c2121c2))
* support JSON config files ([#752](https://github.com/matter-labs/zksync-os-server/issues/752)) ([f94d846](https://github.com/matter-labs/zksync-os-server/commit/f94d8463ef726f0c5fd8e68ba5ec564147120ae8))
* token price updater component ([#779](https://github.com/matter-labs/zksync-os-server/issues/779)) ([863b909](https://github.com/matter-labs/zksync-os-server/commit/863b909a8d85e11727927618c037df1cfdb6db4c))
* use newer version of zkyns-os-revm ([#798](https://github.com/matter-labs/zksync-os-server/issues/798)) ([aa97f62](https://github.com/matter-labs/zksync-os-server/commit/aa97f627874b5fdd446cfe35aecfb537ee17226b))
* use token prices in fee model ([#787](https://github.com/matter-labs/zksync-os-server/issues/787)) ([1f2375f](https://github.com/matter-labs/zksync-os-server/commit/1f2375f50e370234785a1b792c28f21056ee05db))


### Bug Fixes

* `zksync_os_types` compiles without features ([#815](https://github.com/matter-labs/zksync-os-server/issues/815)) ([b7dbe66](https://github.com/matter-labs/zksync-os-server/commit/b7dbe661e705af617399228c1da6039d7b4671b0))
* construct pending block context in `eth_call`-like methods ([#758](https://github.com/matter-labs/zksync-os-server/issues/758)) ([1e1086a](https://github.com/matter-labs/zksync-os-server/commit/1e1086af9e8a2c4449653958bae0601608e1c693))
* local chain config file is required to start the node ([#771](https://github.com/matter-labs/zksync-os-server/issues/771)) ([4597cae](https://github.com/matter-labs/zksync-os-server/commit/4597cae68267e67159c0340f9c6ff9cf8853dcc8))
* prevent "subtract with overflow" error on EN startup  ([#802](https://github.com/matter-labs/zksync-os-server/issues/802)) ([0678f56](https://github.com/matter-labs/zksync-os-server/commit/0678f56fdbd53daa8d4defc70924c700b78da883))
* refactor local-chains structure and update with anvil 1.5.1 ([#776](https://github.com/matter-labs/zksync-os-server/issues/776)) ([24d3852](https://github.com/matter-labs/zksync-os-server/commit/24d38529dbeb2f0bdd016005b1e5e0bc491b692f))
* rename sandbox to ephemeral ([#778](https://github.com/matter-labs/zksync-os-server/issues/778)) ([16f6bad](https://github.com/matter-labs/zksync-os-server/commit/16f6bad391fc502041750c6e6e3e1d854bf6099a))
* **rpc:** make `eth_estimateGas` work when sender has no balance ([#807](https://github.com/matter-labs/zksync-os-server/issues/807)) ([4ce1018](https://github.com/matter-labs/zksync-os-server/commit/4ce1018e436063ba8d480fd3a5cdb19d6022ac72))
* run RPC/status components later in the flow ([#817](https://github.com/matter-labs/zksync-os-server/issues/817)) ([387999e](https://github.com/matter-labs/zksync-os-server/commit/387999e8d65e19ef9eec2634b5c6e4af2a7b3929))

## [0.13.0](https://github.com/matter-labs/zksync-os-server/compare/v0.12.1...v0.13.0) (2025-12-22)


### ⚠ BREAKING CHANGES

* protocol upgrade v0.30.1 (zksync-os v0.2.5) ([#743](https://github.com/matter-labs/zksync-os-server/issues/743))
* **network:** use real HTTP server/client for batch verification ([#737](https://github.com/matter-labs/zksync-os-server/issues/737))
* **network:** use real HTTP server/client for replay transport ([#729](https://github.com/matter-labs/zksync-os-server/issues/729))

### Features

* add sequencer ephemeral mode ([#730](https://github.com/matter-labs/zksync-os-server/issues/730)) ([b55cdcd](https://github.com/matter-labs/zksync-os-server/commit/b55cdcd652e6ba8a70e82aa451fbddfc597b9aa8))
* config option to disable priority tree ([#738](https://github.com/matter-labs/zksync-os-server/issues/738)) ([36fbd35](https://github.com/matter-labs/zksync-os-server/commit/36fbd3536a28d231fb1fb5899cd46e9268d23d33))
* **config:** make mempool tx_fee_cap configurable ([#717](https://github.com/matter-labs/zksync-os-server/issues/717)) ([4548357](https://github.com/matter-labs/zksync-os-server/commit/4548357ee2d9e4a9da6709d3f301f8ff7dd80499))
* make bytecode supplier address config value optional ([#735](https://github.com/matter-labs/zksync-os-server/issues/735)) ([1e6f363](https://github.com/matter-labs/zksync-os-server/commit/1e6f363db7dae74bbf923a052498ce353018bacf))
* **network:** use real HTTP server/client for batch verification ([#737](https://github.com/matter-labs/zksync-os-server/issues/737)) ([d4aca72](https://github.com/matter-labs/zksync-os-server/commit/d4aca725a7fe7ba86d9a2df3010cc6bc440f7563))
* **network:** use real HTTP server/client for replay transport ([#729](https://github.com/matter-labs/zksync-os-server/issues/729)) ([5537d28](https://github.com/matter-labs/zksync-os-server/commit/5537d2888aa62fc41e772607b203de9af1b572aa))
* protocol upgrade v0.30.1 (zksync-os v0.2.5) ([#743](https://github.com/matter-labs/zksync-os-server/issues/743)) ([2cd6a6e](https://github.com/matter-labs/zksync-os-server/commit/2cd6a6ef8dfe7eb94a1fd54539753b791c7c460b))
* **rpc:** Add zks_getBlockMetadataByNumber ([#724](https://github.com/matter-labs/zksync-os-server/issues/724)) ([184c4bd](https://github.com/matter-labs/zksync-os-server/commit/184c4bd32e49b8717ed51132be5f1c067d115f20))
* **tracer:** Add error message for out-of-native ([#720](https://github.com/matter-labs/zksync-os-server/issues/720)) ([79d035f](https://github.com/matter-labs/zksync-os-server/commit/79d035f9007bf867fe8518d0995cfd939f9e4532))


### Bug Fixes

* don't require genesis_chain_id for ENs ([#734](https://github.com/matter-labs/zksync-os-server/issues/734)) ([95c0512](https://github.com/matter-labs/zksync-os-server/commit/95c051267f74b281c10669277852788053c5cfc2))
* **l1-watcher:** pick the most recent upgrade cut ([#742](https://github.com/matter-labs/zksync-os-server/issues/742)) ([f86e558](https://github.com/matter-labs/zksync-os-server/commit/f86e558e6ed298439e60f7f7ab718d32efc31f55))
* Replace DashMap with RwLock and HashMap ([#722](https://github.com/matter-labs/zksync-os-server/issues/722)) ([a6e658e](https://github.com/matter-labs/zksync-os-server/commit/a6e658e9f4a9748170cc49cd7b186de76d521c70))
* revm-consistency-checker legacy pre-eip155 transactions ([#740](https://github.com/matter-labs/zksync-os-server/issues/740)) ([b2bd059](https://github.com/matter-labs/zksync-os-server/commit/b2bd05917beae97081e4bf0d8e32be508eabf3f1))
* **tracer:** Fix call tracer behavior for 'empty' transactions ([#718](https://github.com/matter-labs/zksync-os-server/issues/718)) ([81b5e82](https://github.com/matter-labs/zksync-os-server/commit/81b5e82b406041823257dc5f3eb94614e6e1f437))
* **tracer:** Fix handling of errors in subcalls ([#719](https://github.com/matter-labs/zksync-os-server/issues/719)) ([1af589d](https://github.com/matter-labs/zksync-os-server/commit/1af589dd8b53cadb481c75e5305b97b971510d3d))
* Update revm to v0.0.2 ([#732](https://github.com/matter-labs/zksync-os-server/issues/732)) ([e502499](https://github.com/matter-labs/zksync-os-server/commit/e502499c9d8b33decf2456ad67ea3961c9df7644))

## [0.12.1](https://github.com/matter-labs/zksync-os-server/compare/v0.12.0...v0.12.1) (2025-12-11)


### Features

* **batcher:** re-create batches using L1 watcher's data ([#672](https://github.com/matter-labs/zksync-os-server/issues/672)) ([11fefc4](https://github.com/matter-labs/zksync-os-server/commit/11fefc41c7c55f88b40ecab5e31464ef1e68e8e4))
* blob computation overhead for pubdata price ([#693](https://github.com/matter-labs/zksync-os-server/issues/693)) ([bf69d65](https://github.com/matter-labs/zksync-os-server/commit/bf69d65f29b0a6bf4a38093a6f26fea1dad97167))
* **config:** Add config command ([#697](https://github.com/matter-labs/zksync-os-server/issues/697)) ([cd8a611](https://github.com/matter-labs/zksync-os-server/commit/cd8a61186406aaf940510ce948aefd21fc1a6c22))
* **config:** use EtherAmount for fee-related configs ([#676](https://github.com/matter-labs/zksync-os-server/issues/676)) ([28c27b1](https://github.com/matter-labs/zksync-os-server/commit/28c27b1a215898bcd7aa27437bcce641eb88636c))
* Don't report Passthrough in batch_number metrics ([#683](https://github.com/matter-labs/zksync-os-server/issues/683)) ([7719fb3](https://github.com/matter-labs/zksync-os-server/commit/7719fb34a6047e98596b26ffcb2abc12917a97e0))
* JS tracer ([#569](https://github.com/matter-labs/zksync-os-server/issues/569)) ([c991043](https://github.com/matter-labs/zksync-os-server/commit/c99104389a790f29237fe7c880d01d67c9319032))
* remove failed transcations from block_output.tx_results ([#714](https://github.com/matter-labs/zksync-os-server/issues/714)) ([23b5323](https://github.com/matter-labs/zksync-os-server/commit/23b5323d0ce6911ded4bb5566b0f93fcc61f696a))
* upgrade reth to 1.9.3/revm to 31.0.2 ([#709](https://github.com/matter-labs/zksync-os-server/issues/709)) ([521d473](https://github.com/matter-labs/zksync-os-server/commit/521d473854423e01dbf011efda04f007e9156e7a))


### Bug Fixes

* **l1-watcher:** handle L1 reverts during state recovery ([#692](https://github.com/matter-labs/zksync-os-server/issues/692)) ([d915174](https://github.com/matter-labs/zksync-os-server/commit/d9151748ada061800611eac8e89a6843c2c57875))
* **rpc:** move executed block check earlier in `zks_getL2ToL1LogProof` ([#704](https://github.com/matter-labs/zksync-os-server/issues/704)) ([117faa8](https://github.com/matter-labs/zksync-os-server/commit/117faa85db69889ff76bffe781fa4ed754d2a6e7))
* state tracking for sequencer ([#715](https://github.com/matter-labs/zksync-os-server/issues/715)) ([01c3a6b](https://github.com/matter-labs/zksync-os-server/commit/01c3a6bb93795a9ce3542e32d59d3a1c53ed55ff))
* upgrade issues in block context provider ([#666](https://github.com/matter-labs/zksync-os-server/issues/666)) ([e80cb85](https://github.com/matter-labs/zksync-os-server/commit/e80cb8539e5a986516a8b01e7a1d0aaa9ec1e9ac))

## [0.12.0](https://github.com/matter-labs/zksync-os-server/compare/v0.11.1...v0.12.0) (2025-11-28)


### ⚠ BREAKING CHANGES

* allow EN to sync with overriden records ([#657](https://github.com/matter-labs/zksync-os-server/issues/657))
* Remove deprecated legacy prover API ([#674](https://github.com/matter-labs/zksync-os-server/issues/674))

### Features

* add internal config; use it in revm checker ([#608](https://github.com/matter-labs/zksync-os-server/issues/608)) ([13e6d18](https://github.com/matter-labs/zksync-os-server/commit/13e6d18ca67561e1c8789b91a0dadc31bd5ab781))
* allow EN to sync with overriden records ([#657](https://github.com/matter-labs/zksync-os-server/issues/657)) ([9422a14](https://github.com/matter-labs/zksync-os-server/commit/9422a1482d82a87f25a9d3f5344299cde9821da0))
* **db:** keep overwritten replay records ([#620](https://github.com/matter-labs/zksync-os-server/issues/620)) ([35bdab6](https://github.com/matter-labs/zksync-os-server/commit/35bdab69403d20b67a87555f81e2593f3bdd14e4))
* **l1-sender:** send EIP-7594 blobs when Fusaka is activated ([#664](https://github.com/matter-labs/zksync-os-server/issues/664)) ([0b41a19](https://github.com/matter-labs/zksync-os-server/commit/0b41a194157a84bb3ee6c2ab1c750e34847c9529))
* **l1-watcher:** monitor `ReportCommittedBatchRangeZKsyncOS` events ([#661](https://github.com/matter-labs/zksync-os-server/issues/661)) ([f21e876](https://github.com/matter-labs/zksync-os-server/commit/f21e876456a04458fbf54f43da4bf87058cb6d20))
* **mempool-config:** make minimal_protocol_basefee configurable ([#671](https://github.com/matter-labs/zksync-os-server/issues/671)) ([9a65250](https://github.com/matter-labs/zksync-os-server/commit/9a65250ffdb8dd22a2cb17362ea4bbaf08ba83b3))
* Remove deprecated legacy prover API ([#674](https://github.com/matter-labs/zksync-os-server/issues/674)) ([728c177](https://github.com/matter-labs/zksync-os-server/commit/728c177dc488198cf886907e2afd279fc5a891be))
* **rpc:** use pubdata price factor during gas estimation ([#669](https://github.com/matter-labs/zksync-os-server/issues/669)) ([8dd8377](https://github.com/matter-labs/zksync-os-server/commit/8dd8377ea88ff41244ed57ae131348475333d16d))
* support multiple SNARKers; enhance proving observability ([#631](https://github.com/matter-labs/zksync-os-server/issues/631)) ([8541de8](https://github.com/matter-labs/zksync-os-server/commit/8541de8ac81bd3f26b595733148221f47570dce9))


### Bug Fixes

* 2FA followup ([#662](https://github.com/matter-labs/zksync-os-server/issues/662)) ([954b322](https://github.com/matter-labs/zksync-os-server/commit/954b322b60a6b919f6b655765f4447a0b324f3fa))
* batch verification config ([#654](https://github.com/matter-labs/zksync-os-server/issues/654)) ([941edbd](https://github.com/matter-labs/zksync-os-server/commit/941edbd64912a02320dcf7132f0357ffa052890c))
* **en:** handle missing blocks on main node ([#677](https://github.com/matter-labs/zksync-os-server/issues/677)) ([d7e2291](https://github.com/matter-labs/zksync-os-server/commit/d7e2291e923214266aa87fd51b4ba616d35d0b6e))
* Sealing empty blocks ([#653](https://github.com/matter-labs/zksync-os-server/issues/653)) ([fcb43d8](https://github.com/matter-labs/zksync-os-server/commit/fcb43d8072d00a576006d324d727d5ea9a1533cf))

## [0.11.1](https://github.com/matter-labs/zksync-os-server/compare/v0.11.0...v0.11.1) (2025-11-24)


### Features

* Add time_since metrics ([#628](https://github.com/matter-labs/zksync-os-server/issues/628)) ([33a7224](https://github.com/matter-labs/zksync-os-server/commit/33a722440f5399f74b8f80b95d9386f285c16c5e))
* config option to disable batcher hash assertion when rebuilding batches ([#647](https://github.com/matter-labs/zksync-os-server/issues/647)) ([34d45e1](https://github.com/matter-labs/zksync-os-server/commit/34d45e1f3b1420664c6a0e1f4367a47e7d10e27c))
* update zksync-os with p256 fix ([#642](https://github.com/matter-labs/zksync-os-server/issues/642)) ([ea04463](https://github.com/matter-labs/zksync-os-server/commit/ea044637adb94336999d0e5031dd61c007defc11))
* upgrade smart-config to 0.4.0; simplify parsing ([#644](https://github.com/matter-labs/zksync-os-server/issues/644)) ([a0c1da9](https://github.com/matter-labs/zksync-os-server/commit/a0c1da9fea1312d46be0f6594d55787ea3ae45dc))


### Bug Fixes

* **batcher:** rebuild batches from S3 even when they are not committed ([#645](https://github.com/matter-labs/zksync-os-server/issues/645)) ([608153d](https://github.com/matter-labs/zksync-os-server/commit/608153d83dee7d37d03c9e53120a496454658df5))
* Update ZKsync REVM deps ([#648](https://github.com/matter-labs/zksync-os-server/issues/648)) ([d66af50](https://github.com/matter-labs/zksync-os-server/commit/d66af5089b5f616da1387d05c7efa480ba5d0b92))

## [0.11.0](https://github.com/matter-labs/zksync-os-server/compare/v0.10.1...v0.11.0) (2025-11-20)


### ⚠ BREAKING CHANGES

* v30 zksync os protocol upgrade support ([#594](https://github.com/matter-labs/zksync-os-server/issues/594))
* upgrade system (part 1 of N) ([#582](https://github.com/matter-labs/zksync-os-server/issues/582))

### Features

* add config for l2 signer blacklist ([#596](https://github.com/matter-labs/zksync-os-server/issues/596)) ([bc30cc9](https://github.com/matter-labs/zksync-os-server/commit/bc30cc967ed79119158ce90f6f0c4b93561f17a2))
* add some prover metrics ([#611](https://github.com/matter-labs/zksync-os-server/issues/611)) ([b2483cf](https://github.com/matter-labs/zksync-os-server/commit/b2483cf3c2d36b49e2c9b078d30f30cd94397cb5))
* **api:** forward EN transactions to main node ([#624](https://github.com/matter-labs/zksync-os-server/issues/624)) ([9a7583c](https://github.com/matter-labs/zksync-os-server/commit/9a7583c87b6e46a13a2cfc69a3796d95cfafa69f))
* **api:** implement EIP-7966 eth_sendRawTransactionSync ([#621](https://github.com/matter-labs/zksync-os-server/issues/621)) ([0fbf615](https://github.com/matter-labs/zksync-os-server/commit/0fbf615a3d4d99ea4c85296ea8ed0e8e1203c52a))
* handle reorgs for EN ([#610](https://github.com/matter-labs/zksync-os-server/issues/610)) ([055136d](https://github.com/matter-labs/zksync-os-server/commit/055136d8f5ce8a41048e0be48437e2bf04c16fac))
* **l1_watcher:** Make l1 watcher processor-agnostic ([#634](https://github.com/matter-labs/zksync-os-server/issues/634)) ([a3fe619](https://github.com/matter-labs/zksync-os-server/commit/a3fe6198be7ec4abd3ef6b2fd8af6337035e0a60))
* Read force deploys from a file ([#612](https://github.com/matter-labs/zksync-os-server/issues/612)) ([b90473a](https://github.com/matter-labs/zksync-os-server/commit/b90473ad45676c307510d84cb64464bf4c728b97))
* upgrade system (part 1 of N) ([#582](https://github.com/matter-labs/zksync-os-server/issues/582)) ([4de5e84](https://github.com/matter-labs/zksync-os-server/commit/4de5e841a3fce8eadcfba2c4cb430de022d20d25))
* upgrade system (part 2 of N) ([#609](https://github.com/matter-labs/zksync-os-server/issues/609)) ([b9a303d](https://github.com/matter-labs/zksync-os-server/commit/b9a303d58adea7a9d8558e374bb28f5944a244f9))
* v30 zksync os protocol upgrade support ([#594](https://github.com/matter-labs/zksync-os-server/issues/594)) ([c8698a6](https://github.com/matter-labs/zksync-os-server/commit/c8698a683546e29a6e9e2fc58cac4371bbb4c80c))


### Bug Fixes

* **config:** add config attributes to fee overrides ([#603](https://github.com/matter-labs/zksync-os-server/issues/603)) ([5539e91](https://github.com/matter-labs/zksync-os-server/commit/5539e918cbfbdb3ad292c442364f04f56d5375bf))
* fix calculation of da fields for validium v4 ([#636](https://github.com/matter-labs/zksync-os-server/issues/636)) ([72282d2](https://github.com/matter-labs/zksync-os-server/commit/72282d25f64b22d18c791f540438bd457c97cb37))
* move BlacklistedSigner error to different enum ([#605](https://github.com/matter-labs/zksync-os-server/issues/605)) ([fd9f1bd](https://github.com/matter-labs/zksync-os-server/commit/fd9f1bdabd1d7247ae381df8da8cc40b38646dd3))
* upgrade issues ([#638](https://github.com/matter-labs/zksync-os-server/issues/638)) ([15697bb](https://github.com/matter-labs/zksync-os-server/commit/15697bb7ec837a06308254e13acae64a2560f224))
* upgrade issues second part ([#639](https://github.com/matter-labs/zksync-os-server/issues/639)) ([a06bb32](https://github.com/matter-labs/zksync-os-server/commit/a06bb32a0ba71978171e16b8a4a5b15b7838f750))

## [0.10.1](https://github.com/matter-labs/zksync-os-server/compare/v0.10.0...v0.10.1) (2025-11-12)


### Features

* Add REVM support of multiple execution versions ([#597](https://github.com/matter-labs/zksync-os-server/issues/597)) ([cccdba0](https://github.com/matter-labs/zksync-os-server/commit/cccdba0d7e88878438191079326463c9760c0aa4))
* set default block time to 250ms ([#598](https://github.com/matter-labs/zksync-os-server/issues/598)) ([3f7c724](https://github.com/matter-labs/zksync-os-server/commit/3f7c724eb671a873064548293f70dff8a6290cb0))
* set sensible global debug levels ([#600](https://github.com/matter-labs/zksync-os-server/issues/600)) ([5e2cdcf](https://github.com/matter-labs/zksync-os-server/commit/5e2cdcfd46ca6fc0f76c6fc36e393dcc003854f5))


### Bug Fixes

* register misc mempool metrics ([#599](https://github.com/matter-labs/zksync-os-server/issues/599)) ([02164b0](https://github.com/matter-labs/zksync-os-server/commit/02164b05fa753e051024bd13bc599a1f2e927336))

## [0.10.0](https://github.com/matter-labs/zksync-os-server/compare/v0.9.2...v0.10.0) (2025-11-06)


### ⚠ BREAKING CHANGES

* support zksync-os v0.1.0 ([#557](https://github.com/matter-labs/zksync-os-server/issues/557))

### Features

* add last_execution_version metric ([#590](https://github.com/matter-labs/zksync-os-server/issues/590)) ([9343794](https://github.com/matter-labs/zksync-os-server/commit/9343794c7a27bd315a7a3096591265abb961247f))
* get rid of batch rescheduling (preparation to get rid of BatchStorage) ([#587](https://github.com/matter-labs/zksync-os-server/issues/587)) ([62dd891](https://github.com/matter-labs/zksync-os-server/commit/62dd89119749fcfe51280676bbc569e189d30626))
* remove app_bin_unpack_path from config ([#588](https://github.com/matter-labs/zksync-os-server/issues/588)) ([e55b0d4](https://github.com/matter-labs/zksync-os-server/commit/e55b0d43f631efbc39f2a24bbb8dcb08e5474727))
* support zksync-os v0.1.0 ([#557](https://github.com/matter-labs/zksync-os-server/issues/557)) ([178a1a9](https://github.com/matter-labs/zksync-os-server/commit/178a1a975dc682a24be5dc6d7e33733c7786f493))

## [0.9.2](https://github.com/matter-labs/zksync-os-server/compare/v0.9.1...v0.9.2) (2025-11-06)


### Features

* 2FA EN batch signing without L1 verification ([#459](https://github.com/matter-labs/zksync-os-server/issues/459)) ([e6d41ab](https://github.com/matter-labs/zksync-os-server/commit/e6d41abf581e5baeeda73b8a772ab7572a8d2b2e))
* get rid of l1_gas_pricing_multiplier ([#576](https://github.com/matter-labs/zksync-os-server/issues/576)) ([3699956](https://github.com/matter-labs/zksync-os-server/commit/36999561aa64f3af7b730e0bae8b461fd903a8b5))
* Protocol upgrade support for provers ([#577](https://github.com/matter-labs/zksync-os-server/issues/577)) ([a60bb89](https://github.com/matter-labs/zksync-os-server/commit/a60bb89c9c7a52c166cc208b98bdf2a3644bec3c))
* **sentry:** Use CLUSTER_NAME as environment tag ([#570](https://github.com/matter-labs/zksync-os-server/issues/570)) ([0befa23](https://github.com/matter-labs/zksync-os-server/commit/0befa239c7b6576cae986eb1e4f0398131dd17b2))


### Bug Fixes

* Consistency checker nonce for failed creates ([#574](https://github.com/matter-labs/zksync-os-server/issues/574)) ([8159d64](https://github.com/matter-labs/zksync-os-server/commit/8159d64d4dff8b1188ce45d6b45dd7e754bed3ad))
* proving empty blocks - fix division by zero error in metrics tracking ([#584](https://github.com/matter-labs/zksync-os-server/issues/584)) ([3c7d3bd](https://github.com/matter-labs/zksync-os-server/commit/3c7d3bd3ea713dd4b71af687f1110504a767ca87))
* set WORKDIR to /app ([#573](https://github.com/matter-labs/zksync-os-server/issues/573)) ([265dc34](https://github.com/matter-labs/zksync-os-server/commit/265dc347daba05e82d669486588a8b6980defd9f))

## [0.9.1](https://github.com/matter-labs/zksync-os-server/compare/v0.9.0...v0.9.1) (2025-10-29)


### Features

* add block rebuild options ([#565](https://github.com/matter-labs/zksync-os-server/issues/565)) ([eab9bdf](https://github.com/matter-labs/zksync-os-server/commit/eab9bdfa7ec205421e55251a2213a406995bc8aa))


### Bug Fixes

* consume l1 txs processed in rebuild commands ([#568](https://github.com/matter-labs/zksync-os-server/issues/568)) ([ff74bec](https://github.com/matter-labs/zksync-os-server/commit/ff74bece2252626782d31fd9358ce41ed5289649))

## [0.9.0](https://github.com/matter-labs/zksync-os-server/compare/v0.8.4...v0.9.0) (2025-10-28)


### ⚠ BREAKING CHANGES

* Opentelemetry support + config schema change ([#559](https://github.com/matter-labs/zksync-os-server/issues/559))

### Features

* eth_estimateGas state overrides ([#560](https://github.com/matter-labs/zksync-os-server/issues/560)) ([44a2281](https://github.com/matter-labs/zksync-os-server/commit/44a228151fb814d122b9afb75e88e980176c9902))
* Opentelemetry support + config schema change ([#559](https://github.com/matter-labs/zksync-os-server/issues/559)) ([592d6bb](https://github.com/matter-labs/zksync-os-server/commit/592d6bb080c561687f6f39a4c18badf27df640cf))
* pubdata price calculation ([#549](https://github.com/matter-labs/zksync-os-server/issues/549)) ([d1700ba](https://github.com/matter-labs/zksync-os-server/commit/d1700babcb7ac5cf4519a2771941050dc217a870))
* revm consistency checker ([#525](https://github.com/matter-labs/zksync-os-server/issues/525)) ([2061a01](https://github.com/matter-labs/zksync-os-server/commit/2061a01f2ae09923b00b33f1705d36ea7b62feb5))

## [0.8.4](https://github.com/matter-labs/zksync-os-server/compare/v0.8.3...v0.8.4) (2025-10-21)


### Features

* config in sequencer to limit block production for operations/debug ([#537](https://github.com/matter-labs/zksync-os-server/issues/537)) ([ebdde51](https://github.com/matter-labs/zksync-os-server/commit/ebdde5129cc15e03600378501744c52eca231263))
* eth_call state overrides ([#539](https://github.com/matter-labs/zksync-os-server/issues/539)) ([bdf32ab](https://github.com/matter-labs/zksync-os-server/commit/bdf32ab4875df087cee8a384456f6ade738c5bb6))
* **l1-sender:** use alloy-based tx inclusion ([#541](https://github.com/matter-labs/zksync-os-server/issues/541)) ([48202cd](https://github.com/matter-labs/zksync-os-server/commit/48202cdbd70381f3689670e7d76bfe53dcdd2801))
* **l1-watcher:** move pagination/polling into shared component ([#548](https://github.com/matter-labs/zksync-os-server/issues/548)) ([d98d0ef](https://github.com/matter-labs/zksync-os-server/commit/d98d0ef66e2c232141a72cf8b8d31fc23be14721))
* make pipelines repository-agnostic ([#536](https://github.com/matter-labs/zksync-os-server/issues/536)) ([e28635b](https://github.com/matter-labs/zksync-os-server/commit/e28635bdc12432857cbeb84056a684bba8e1edf9))
* **storage:** move replay DB to storage crate ([#535](https://github.com/matter-labs/zksync-os-server/issues/535)) ([9c43a90](https://github.com/matter-labs/zksync-os-server/commit/9c43a90011bcb63c69029c2a2505c1ad4576180d))


### Bug Fixes

* Disable warning on connection retries ([#545](https://github.com/matter-labs/zksync-os-server/issues/545)) ([1a56284](https://github.com/matter-labs/zksync-os-server/commit/1a5628418b11cec0e5b99cfcb6df10115a8e05a2))
* Persisting some info about the failed batch ([#532](https://github.com/matter-labs/zksync-os-server/issues/532)) ([ccc9a9f](https://github.com/matter-labs/zksync-os-server/commit/ccc9a9fe48279820731b46094404ccb3a57bdd21))
* **sequencer:** save replay record first ([#556](https://github.com/matter-labs/zksync-os-server/issues/556)) ([1f3fe08](https://github.com/matter-labs/zksync-os-server/commit/1f3fe08bfa9cc45cd499cc84fc789490e1e22497))

## [0.8.3](https://github.com/matter-labs/zksync-os-server/compare/v0.8.2...v0.8.3) (2025-10-15)


### Features

* add execution version enum ([#517](https://github.com/matter-labs/zksync-os-server/issues/517)) ([c5703f9](https://github.com/matter-labs/zksync-os-server/commit/c5703f9736bbe3511a833b75070b593bf854bf03))
* **l1-watcher:** poll events actively when behind ([#523](https://github.com/matter-labs/zksync-os-server/issues/523)) ([93d6b4b](https://github.com/matter-labs/zksync-os-server/commit/93d6b4becbc1bf27ca5331df72fbb3184c4fdc2f))
* **l1:** move `{Commit,Stored}BatchInfo` + introduce `BatchInfo` ([#505](https://github.com/matter-labs/zksync-os-server/issues/505)) ([fe0a6bd](https://github.com/matter-labs/zksync-os-server/commit/fe0a6bdf7df9f3488dff48fe779a383d337ebe23))
* **l1:** move L1 discovery out of `L1Sender` ([#502](https://github.com/matter-labs/zksync-os-server/issues/502)) ([32aff65](https://github.com/matter-labs/zksync-os-server/commit/32aff6570eec9b6e2061e6ac791d08f588da7c96))
* **mempool:** export even more metrics ([#529](https://github.com/matter-labs/zksync-os-server/issues/529)) ([1152166](https://github.com/matter-labs/zksync-os-server/commit/1152166d3516e6e6cd28878e3074fcd5e3ab6378))
* **mempool:** expose metrics ([#522](https://github.com/matter-labs/zksync-os-server/issues/522)) ([6de3a50](https://github.com/matter-labs/zksync-os-server/commit/6de3a50f50536676533ce356ef22989bcd9e688f))
* replace str with module name for app bin unpack path ([#516](https://github.com/matter-labs/zksync-os-server/issues/516)) ([3f90248](https://github.com/matter-labs/zksync-os-server/commit/3f90248b620088449fcdf60b6b608c5d533d2a74))
* Saving failed proofs to bucket and exposing endpoint to get them ([#507](https://github.com/matter-labs/zksync-os-server/issues/507)) ([0dc2093](https://github.com/matter-labs/zksync-os-server/commit/0dc2093b97115266b40589efd4a9bf54e68d1d66))
* **sequencer:** validate last 256 blocks for replayed blocks ([#524](https://github.com/matter-labs/zksync-os-server/issues/524)) ([9b17514](https://github.com/matter-labs/zksync-os-server/commit/9b175143313ff33294507aa790fe2276ff30f3c3))


### Bug Fixes

* **pipeline:** simplify task spawning ([#519](https://github.com/matter-labs/zksync-os-server/issues/519)) ([cdcfec5](https://github.com/matter-labs/zksync-os-server/commit/cdcfec5a0724ac9c06ea0e7c27cc320064980f7d))
* Reduced tracing level for debug functions ([#531](https://github.com/matter-labs/zksync-os-server/issues/531)) ([b960deb](https://github.com/matter-labs/zksync-os-server/commit/b960debd6a41b448a2f10d64551710334d5422b5))
* **storage:** read replay record atomically ([#521](https://github.com/matter-labs/zksync-os-server/issues/521)) ([ff474a7](https://github.com/matter-labs/zksync-os-server/commit/ff474a76c4b864c1041b5a4a32b0ac0450fb5a5d))
* **tree:** report backpressure ([#520](https://github.com/matter-labs/zksync-os-server/issues/520)) ([7efb8a7](https://github.com/matter-labs/zksync-os-server/commit/7efb8a701158234bec88ede6d142e5145b1189b3))

## [0.8.2](https://github.com/matter-labs/zksync-os-server/compare/v0.8.1...v0.8.2) (2025-10-13)


### Bug Fixes

* **l1-sender:** allow non-empty buffer for rescheduling ([#511](https://github.com/matter-labs/zksync-os-server/issues/511)) ([beec7ec](https://github.com/matter-labs/zksync-os-server/commit/beec7ec87ac1547b353c8a4db4b177896e1cb280))
* **l1-watcher:** update batch finality ([#506](https://github.com/matter-labs/zksync-os-server/issues/506)) ([ca11ba7](https://github.com/matter-labs/zksync-os-server/commit/ca11ba7593883ddbdadbe4e1d65dbd7b82a33857))

## [0.8.1](https://github.com/matter-labs/zksync-os-server/compare/v0.8.0...v0.8.1) (2025-10-11)


### Features

* **genesis:** Add genesis root hash to genesis.json ([#494](https://github.com/matter-labs/zksync-os-server/issues/494)) ([4887597](https://github.com/matter-labs/zksync-os-server/commit/4887597e1dbff1bd101af32eea91383c31b6c998))
* **l1:** retry RPC requests on internal error ([#496](https://github.com/matter-labs/zksync-os-server/issues/496)) ([e89d88a](https://github.com/matter-labs/zksync-os-server/commit/e89d88a46fe1319177bd6a24584eb09faca94faf))
* pipeline framework (8/X) - migrate executor l1 and batch sink ([#481](https://github.com/matter-labs/zksync-os-server/issues/481)) ([44d5776](https://github.com/matter-labs/zksync-os-server/commit/44d577669fa8a3c722c4e212563c9d59f1edc510))
* **rpc:** implement `web3` namespace ([#497](https://github.com/matter-labs/zksync-os-server/issues/497)) ([0ff0cc4](https://github.com/matter-labs/zksync-os-server/commit/0ff0cc4bd607ddd22883b5dce61177b609251bfa))
* track `execution_version` in genesis config ([#498](https://github.com/matter-labs/zksync-os-server/issues/498)) ([136a9a9](https://github.com/matter-labs/zksync-os-server/commit/136a9a982dc2ed132d31efe9b5b26b3c22dfe7a5))


### Bug Fixes

* add default v,r,s,yParity fields in L1TxType during serialization ([#500](https://github.com/matter-labs/zksync-os-server/issues/500)) ([a1f28ab](https://github.com/matter-labs/zksync-os-server/commit/a1f28ab7bfabe659bffc9902bee036fadd7ed406))

## [0.8.0](https://github.com/matter-labs/zksync-os-server/compare/v0.7.5...v0.8.0) (2025-10-09)


### ⚠ BREAKING CHANGES

* Protocol upgrade v1.1 ([#487](https://github.com/matter-labs/zksync-os-server/issues/487))

### Features

* add config for fee params override ([#489](https://github.com/matter-labs/zksync-os-server/issues/489)) ([13587e5](https://github.com/matter-labs/zksync-os-server/commit/13587e529f24f5b1ea6158626403b751e3504b56))
* add more general metrics ([#468](https://github.com/matter-labs/zksync-os-server/issues/468)) ([079a285](https://github.com/matter-labs/zksync-os-server/commit/079a28539dad438d5c483f9103661ef3f52d7e6e))
* Adding more documentation ([#455](https://github.com/matter-labs/zksync-os-server/issues/455)) ([2ed7bc7](https://github.com/matter-labs/zksync-os-server/commit/2ed7bc766d55d3bd682b7c4dcbce04a6a35a6bd3))
* ensure L1 tx is deserializable from RPC response ([#484](https://github.com/matter-labs/zksync-os-server/issues/484)) ([80abbcb](https://github.com/matter-labs/zksync-os-server/commit/80abbcb56c5f876d468bac34b5380ce08a6b4027))
* get rid of `Source`/`Sink` ([#461](https://github.com/matter-labs/zksync-os-server/issues/461)) ([762c9b7](https://github.com/matter-labs/zksync-os-server/commit/762c9b788743813a4b55d138701f7c620e3cc901))
* **l1-watcher:** track last committed/executed batch in finality ([#485](https://github.com/matter-labs/zksync-os-server/issues/485)) ([11c715c](https://github.com/matter-labs/zksync-os-server/commit/11c715c51522e2f2d90421aab2d483274bd81d40))
* make mempool configurable ([#464](https://github.com/matter-labs/zksync-os-server/issues/464)) ([63f9f69](https://github.com/matter-labs/zksync-os-server/commit/63f9f69fcb6486f9e57dc983c7efcf14c0623a69))
* Peek batch data from State ([#458](https://github.com/matter-labs/zksync-os-server/issues/458)) ([05ed98b](https://github.com/matter-labs/zksync-os-server/commit/05ed98b3977bebea91f489e7f33c95612a55d4c8))
* Peek FRI Proofs from ProofStorage ([#470](https://github.com/matter-labs/zksync-os-server/issues/470)) ([0b5bbec](https://github.com/matter-labs/zksync-os-server/commit/0b5bbeca5285e26c68e5bb91d1050d33b1bfdf31))
* pipeline framework (3/X) - migrate FriJobManager ([#465](https://github.com/matter-labs/zksync-os-server/issues/465)) ([2e012d9](https://github.com/matter-labs/zksync-os-server/commit/2e012d9ce4ffa9217dbb471293735a12c30f1e46))
* pipeline framework (4/X): migrate gapless committer ([#467](https://github.com/matter-labs/zksync-os-server/issues/467)) ([07cccce](https://github.com/matter-labs/zksync-os-server/commit/07cccce96472794d0e4dfb322b73ae832d7980de))
* pipeline framework (5/X) - migrate l1 committer ([#472](https://github.com/matter-labs/zksync-os-server/issues/472)) ([2ead9a0](https://github.com/matter-labs/zksync-os-server/commit/2ead9a0f857fb4af828332be9ce66a3544234efa))
* pipeline framework (PR 2/X) - `pipe()` syntax; consume `self`; migrate batcher ([#448](https://github.com/matter-labs/zksync-os-server/issues/448)) ([7366acc](https://github.com/matter-labs/zksync-os-server/commit/7366accd4f587da5b789fa0a730f49ba0e9c294c))
* pipeline framework PR 6/X - migrate l1 sender proves and SnarkJobsManager ([#477](https://github.com/matter-labs/zksync-os-server/issues/477)) ([84d87d6](https://github.com/matter-labs/zksync-os-server/commit/84d87d6b4f777467072b1b5398690fb8daa2e4d7))
* pipeline framework PR 7/X - priority tree migrated ([#479](https://github.com/matter-labs/zksync-os-server/issues/479)) ([2bc7250](https://github.com/matter-labs/zksync-os-server/commit/2bc72500e65301600cd25c4768e65ad9d46e6871))
* Protocol upgrade v1.1 ([#487](https://github.com/matter-labs/zksync-os-server/issues/487)) ([3f49fbc](https://github.com/matter-labs/zksync-os-server/commit/3f49fbc6640223fe02b90b38d5ef34f4731002a9))
* refactor priority tree ([#483](https://github.com/matter-labs/zksync-os-server/issues/483)) ([d12b99f](https://github.com/matter-labs/zksync-os-server/commit/d12b99f1780d3fbc2b3518a88db570d065d60083))
* set pubdata price to `1` ([#476](https://github.com/matter-labs/zksync-os-server/issues/476)) ([dcd060c](https://github.com/matter-labs/zksync-os-server/commit/dcd060ce6ffc1f5ab5b00a706d1d61dc2697fb09))
* update zksync-os to v0.0.26 and interface to v0.0.7 ([#429](https://github.com/matter-labs/zksync-os-server/issues/429)) ([f22e478](https://github.com/matter-labs/zksync-os-server/commit/f22e478bd14f9342bbf88ec3c0516434e6cab265))
* wait for tx in block context provider ([#478](https://github.com/matter-labs/zksync-os-server/issues/478)) ([d6e87b7](https://github.com/matter-labs/zksync-os-server/commit/d6e87b7522289f83c6b5f90ad41ae63b80e8abf3))


### Bug Fixes

* Add TxValidatorConfig to schema ([#475](https://github.com/matter-labs/zksync-os-server/issues/475)) ([797a0b5](https://github.com/matter-labs/zksync-os-server/commit/797a0b5744f6281b3fb2ac2f567c1a12f2638478))
* **multivm:** use correct directories and default version ([#490](https://github.com/matter-labs/zksync-os-server/issues/490)) ([35e5440](https://github.com/matter-labs/zksync-os-server/commit/35e54407a7574aaedb7c0d61292d1436cc8404fe))

## [0.7.5](https://github.com/matter-labs/zksync-os-server/compare/v0.7.4...v0.7.5) (2025-10-06)


### Features

* add net namespace and net_version RPC call support ([#436](https://github.com/matter-labs/zksync-os-server/issues/436)) ([e7b6ff5](https://github.com/matter-labs/zksync-os-server/commit/e7b6ff52d73506670ff5f2ffb03cdc8784fe2f96))
* add Sentry support ([#430](https://github.com/matter-labs/zksync-os-server/issues/430)) ([afed980](https://github.com/matter-labs/zksync-os-server/commit/afed98050b513d36efee32ea85cee2424203e225))
* drop GCP support and reduce dependencies ([#375](https://github.com/matter-labs/zksync-os-server/issues/375)) ([a4bd9e1](https://github.com/matter-labs/zksync-os-server/commit/a4bd9e1dd22b595a74584041152e653e155404ef))
* pipeline framework (1/X) - tree, sequencer and prover_input_gen ([#447](https://github.com/matter-labs/zksync-os-server/issues/447)) ([ba2186e](https://github.com/matter-labs/zksync-os-server/commit/ba2186edb5131e4138917ee4972f2d61c1a5945c))
* re-implement alloy tx types ([#438](https://github.com/matter-labs/zksync-os-server/issues/438)) ([9f993fc](https://github.com/matter-labs/zksync-os-server/commit/9f993fc2264a5c8c9c3820c9d00b29a6dad5616b))


### Bug Fixes

* report error on reverting `eth_call` ([#449](https://github.com/matter-labs/zksync-os-server/issues/449)) ([39ff0ae](https://github.com/matter-labs/zksync-os-server/commit/39ff0aef8ff5012437ea7638fd795e1fc978deed))

## [0.7.4](https://github.com/matter-labs/zksync-os-server/compare/v0.7.3...v0.7.4) (2025-09-30)


### Features

* add logging configuration (json/terminal/logfmt) ([#407](https://github.com/matter-labs/zksync-os-server/issues/407)) ([06ef2f5](https://github.com/matter-labs/zksync-os-server/commit/06ef2f51f92264f6a80d94d841d1921a60d41809))
* **en:** remote en config ([#387](https://github.com/matter-labs/zksync-os-server/issues/387)) ([550f3c4](https://github.com/matter-labs/zksync-os-server/commit/550f3c468977ae64aa28b44af62840cc2db37e39))
* set gas per pubdata to `1` ([#406](https://github.com/matter-labs/zksync-os-server/issues/406)) ([528ea85](https://github.com/matter-labs/zksync-os-server/commit/528ea85cde0d4494d32cb4db99336511a6f173e7))


### Bug Fixes

* hack to allow forcing null bridgehub in config ([#435](https://github.com/matter-labs/zksync-os-server/issues/435)) ([60c007b](https://github.com/matter-labs/zksync-os-server/commit/60c007b8da71ce6fceb2a15b74157642fd15afae))


### Reverts

* feat: set gas per pubdata to `1` ([#431](https://github.com/matter-labs/zksync-os-server/issues/431)) ([1ca638b](https://github.com/matter-labs/zksync-os-server/commit/1ca638b4bed6cfd9630d524fa80f627743a1e306))

## [0.7.3](https://github.com/matter-labs/zksync-os-server/compare/v0.7.2...v0.7.3) (2025-09-26)


### Features

* configurable fee collector ([#383](https://github.com/matter-labs/zksync-os-server/issues/383)) ([2d89f45](https://github.com/matter-labs/zksync-os-server/commit/2d89f45ce0105ae31bf3c19a9ce8e74aa8077d53))

## [0.7.2](https://github.com/matter-labs/zksync-os-server/compare/v0.7.1...v0.7.2) (2025-09-25)


### Bug Fixes

* missing unwrap_or in submit_proof ([#418](https://github.com/matter-labs/zksync-os-server/issues/418)) ([32f8ade](https://github.com/matter-labs/zksync-os-server/commit/32f8ade4748c4867dbdce69383071e5f34d158ad))

## [0.7.1](https://github.com/matter-labs/zksync-os-server/compare/v0.7.0...v0.7.1) (2025-09-25)


### Features

* more metrics and logs - gas per second, transaction status ([#415](https://github.com/matter-labs/zksync-os-server/issues/415)) ([6f7711a](https://github.com/matter-labs/zksync-os-server/commit/6f7711aa5a3df28070f718cf31f6371bbf7656dd))


### Bug Fixes

* unwrap_or in pick_real_job  ([#416](https://github.com/matter-labs/zksync-os-server/issues/416)) ([9097d00](https://github.com/matter-labs/zksync-os-server/commit/9097d0014785557b6d922b0442d73d31b83ad043))

## [0.7.0](https://github.com/matter-labs/zksync-os-server/compare/v0.6.4...v0.7.0) (2025-09-25)


### ⚠ BREAKING CHANGES

* add `execution_version` 2 ([#409](https://github.com/matter-labs/zksync-os-server/issues/409))

### Features

* add `execution_version` 2 ([#409](https://github.com/matter-labs/zksync-os-server/issues/409)) ([a661115](https://github.com/matter-labs/zksync-os-server/commit/a6611152b7eeab51d2bd3ea4fcfef5d15ccd5a40))


### Bug Fixes

* backward compatible deserialization for proofs ([#414](https://github.com/matter-labs/zksync-os-server/issues/414)) ([84e5182](https://github.com/matter-labs/zksync-os-server/commit/84e51827a4cbb4fb6cb060d4a7663622636b3fe7))

## [0.6.4](https://github.com/matter-labs/zksync-os-server/compare/v0.6.3...v0.6.4) (2025-09-22)


### Features

* config option to force starting block number ([#402](https://github.com/matter-labs/zksync-os-server/issues/402)) ([b6024ab](https://github.com/matter-labs/zksync-os-server/commit/b6024abb9a1461aacc2973b7dd823cd930971cc7))
* improve debug logging ([#401](https://github.com/matter-labs/zksync-os-server/issues/401)) ([d996338](https://github.com/matter-labs/zksync-os-server/commit/d996338b9b0264ede512f85370a58c0607d97c36))
* make batcher skip blocks that are already processed ([#404](https://github.com/matter-labs/zksync-os-server/issues/404)) ([edb2c27](https://github.com/matter-labs/zksync-os-server/commit/edb2c27cf0ca445d86688e2b5f4befcef11fc8b8))

## [0.6.3](https://github.com/matter-labs/zksync-os-server/compare/v0.6.2...v0.6.3) (2025-09-22)


### Bug Fixes

* priority tree caching ([#399](https://github.com/matter-labs/zksync-os-server/issues/399)) ([b8c4e8d](https://github.com/matter-labs/zksync-os-server/commit/b8c4e8dca86ddbeb054c594ec437d923c0c62824))

## [0.6.2](https://github.com/matter-labs/zksync-os-server/compare/v0.6.1...v0.6.2) (2025-09-22)


### Bug Fixes

* priority tree trim ([#397](https://github.com/matter-labs/zksync-os-server/issues/397)) ([e908c4e](https://github.com/matter-labs/zksync-os-server/commit/e908c4e0cbf5dfd90063cb4273f5551b55685795))

## [0.6.1](https://github.com/matter-labs/zksync-os-server/compare/v0.6.0...v0.6.1) (2025-09-22)


### Features

* **l1:** optimistic RPC retry policy ([#385](https://github.com/matter-labs/zksync-os-server/issues/385)) ([16f816b](https://github.com/matter-labs/zksync-os-server/commit/16f816bea3d50b2c98f0f836c60adec16fd5dde1))


### Bug Fixes

* **state:** do not overwrite full diffs ([#386](https://github.com/matter-labs/zksync-os-server/issues/386)) ([c715709](https://github.com/matter-labs/zksync-os-server/commit/c715709afa36edf4831c1c1ef3aacd85fd158d19))
* use correct previous_block_timestamp on server restart ([#384](https://github.com/matter-labs/zksync-os-server/issues/384)) ([941b1d5](https://github.com/matter-labs/zksync-os-server/commit/941b1d52e51321f524956b08d1568eeea6c2f247))

## [0.6.0](https://github.com/matter-labs/zksync-os-server/compare/v0.5.0...v0.6.0) (2025-09-17)


### ⚠ BREAKING CHANGES

* folder with risc-v binaries + handle protocol version in batch components ([#369](https://github.com/matter-labs/zksync-os-server/issues/369))

### Features

* add retry layer for l1 provider ([#377](https://github.com/matter-labs/zksync-os-server/issues/377)) ([8f2bfda](https://github.com/matter-labs/zksync-os-server/commit/8f2bfda76c8d0c8cbfec953aa14d7fa6d09c6d42))
* config option to disable l1 senders ([#372](https://github.com/matter-labs/zksync-os-server/issues/372)) ([51253ca](https://github.com/matter-labs/zksync-os-server/commit/51253cae83485ab8b23e370dabfc5bd1d2283a0b))
* folder with risc-v binaries + handle protocol version in batch components ([#369](https://github.com/matter-labs/zksync-os-server/issues/369)) ([39ff2cf](https://github.com/matter-labs/zksync-os-server/commit/39ff2cf7d657ecbea83ac640b02b485c9490c488))
* support L1-&gt;L2 tx gas estimation ([#370](https://github.com/matter-labs/zksync-os-server/issues/370)) ([11febe4](https://github.com/matter-labs/zksync-os-server/commit/11febe428708aaa69d96bef725654ef20bf60562))

## [0.5.0](https://github.com/matter-labs/zksync-os-server/compare/v0.4.0...v0.5.0) (2025-09-15)


### ⚠ BREAKING CHANGES

* Update state - contracts: zkos-v0.29.6, zkstack tool: origin/main ([#364](https://github.com/matter-labs/zksync-os-server/issues/364))
* zksync os inteface/multivm ([#345](https://github.com/matter-labs/zksync-os-server/issues/345))
* Update state - contracts from zkos-0.29.5 + scripts changes ([#356](https://github.com/matter-labs/zksync-os-server/issues/356))
* make EN replay streams HTTP 1.0 ([#341](https://github.com/matter-labs/zksync-os-server/issues/341))

### Features

* add persistence for priority tree ([#321](https://github.com/matter-labs/zksync-os-server/issues/321)) ([2107932](https://github.com/matter-labs/zksync-os-server/commit/210793218f104c6249ca061215959d389f7d89c6))
* additional metrics to various components ([#352](https://github.com/matter-labs/zksync-os-server/issues/352)) ([821f319](https://github.com/matter-labs/zksync-os-server/commit/821f319373ecab6bd0a9041000eb195a205a8526))
* delay the termination, expose health endpoint ([#348](https://github.com/matter-labs/zksync-os-server/issues/348)) ([ab4c709](https://github.com/matter-labs/zksync-os-server/commit/ab4c70956af9d118390b1db0f99f30fb59a5a622))
* Enhance documentation for zkos and era contracts updates ([#337](https://github.com/matter-labs/zksync-os-server/issues/337)) ([cfc42e2](https://github.com/matter-labs/zksync-os-server/commit/cfc42e20767410163f54de7c199853075a2e5ca7))
* have all user-facing config values in one file ([#349](https://github.com/matter-labs/zksync-os-server/issues/349)) ([14cf17c](https://github.com/matter-labs/zksync-os-server/commit/14cf17c4219222ef0d30154a93dd4f2ab6fc5648))
* implement `debug_traceCall` ([#359](https://github.com/matter-labs/zksync-os-server/issues/359)) ([1d11649](https://github.com/matter-labs/zksync-os-server/commit/1d1164938da483175ded72ac38ec24789657623b))
* **l1-sender:** wait for pending state to finalize ([#311](https://github.com/matter-labs/zksync-os-server/issues/311)) ([2aebbb5](https://github.com/matter-labs/zksync-os-server/commit/2aebbb5fee094b3a63843e30c27feb6861ce0109))
* make EN replay streams HTTP 1.0 ([#341](https://github.com/matter-labs/zksync-os-server/issues/341)) ([f78e184](https://github.com/matter-labs/zksync-os-server/commit/f78e184c76a8ecca081b5255e3eb49638f3d7d06))
* split l1_state metrics; fix typo in l1_sender metrics ([#357](https://github.com/matter-labs/zksync-os-server/issues/357)) ([b100eda](https://github.com/matter-labs/zksync-os-server/commit/b100eda5554081c8b8f08a99c832984f4dd6ff0b))
* Update state - contracts from zkos-0.29.5 + scripts changes ([#356](https://github.com/matter-labs/zksync-os-server/issues/356)) ([246618e](https://github.com/matter-labs/zksync-os-server/commit/246618e4fac6e95a060681ee7724ad5c303bf88b))
* Update state - contracts: zkos-v0.29.6, zkstack tool: origin/main ([#364](https://github.com/matter-labs/zksync-os-server/issues/364)) ([282919c](https://github.com/matter-labs/zksync-os-server/commit/282919cfaf8542d1cea15b06c80cf8c3e0aabd36))
* zksync os inteface/multivm ([#345](https://github.com/matter-labs/zksync-os-server/issues/345)) ([0498f2b](https://github.com/matter-labs/zksync-os-server/commit/0498f2b7e760b7ab16c7cc157d6b917eff08da8e))


### Bug Fixes

* `eth_getTransactionCount` takes mempool into account ([#360](https://github.com/matter-labs/zksync-os-server/issues/360)) ([2141089](https://github.com/matter-labs/zksync-os-server/commit/2141089dead809862114bc7e962bb95842cae2ee))
* gas field calculation in tx receipt ([#361](https://github.com/matter-labs/zksync-os-server/issues/361)) ([9bb51f4](https://github.com/matter-labs/zksync-os-server/commit/9bb51f4d20a4cc1135fef37047fee0c6c5c742a7))

## [0.4.0](https://github.com/matter-labs/zksync-os-server/compare/v0.3.0...v0.4.0) (2025-09-09)


### ⚠ BREAKING CHANGES

* external node can read previous replay version ([#224](https://github.com/matter-labs/zksync-os-server/issues/224))

### Features

* external node can read previous replay version ([#224](https://github.com/matter-labs/zksync-os-server/issues/224)) ([a4bd5f5](https://github.com/matter-labs/zksync-os-server/commit/a4bd5f5e7b1576e6af7dced62434488a2ab6c292))
* RPC monitoring middleware ([#306](https://github.com/matter-labs/zksync-os-server/issues/306)) ([8837e43](https://github.com/matter-labs/zksync-os-server/commit/8837e433cb76ef3b481e51c84018f3cf4af105cb))

## [0.3.0](https://github.com/matter-labs/zksync-os-server/compare/v0.2.0...v0.3.0) (2025-09-05)


### ⚠ BREAKING CHANGES

* update l1 contracts interface ([#339](https://github.com/matter-labs/zksync-os-server/issues/339))
* change L1->L2/upgrade tx type id ([#333](https://github.com/matter-labs/zksync-os-server/issues/333))

### Features

* **api:** implement `debug_traceBlockBy{Hash,Number}` ([#310](https://github.com/matter-labs/zksync-os-server/issues/310)) ([3fa831a](https://github.com/matter-labs/zksync-os-server/commit/3fa831aca46b6a0449fde705c19fc891b1a405a5)), closes [#309](https://github.com/matter-labs/zksync-os-server/issues/309)
* change L1-&gt;L2/upgrade tx type id ([#333](https://github.com/matter-labs/zksync-os-server/issues/333)) ([d62892c](https://github.com/matter-labs/zksync-os-server/commit/d62892cc4bab249106684c42332d3b10ae78bb92))
* metric for tx execution ([#323](https://github.com/matter-labs/zksync-os-server/issues/323)) ([ea889bf](https://github.com/matter-labs/zksync-os-server/commit/ea889bf165aaa20f6965c7812f1c49073de21499))
* update l1 contracts interface ([#339](https://github.com/matter-labs/zksync-os-server/issues/339)) ([c7b149e](https://github.com/matter-labs/zksync-os-server/commit/c7b149ee6618fb544d4d2edbf1ee8a3f4c3b161f))
* update tracing-subscriber version ([#325](https://github.com/matter-labs/zksync-os-server/issues/325)) ([b2e7442](https://github.com/matter-labs/zksync-os-server/commit/b2e74424a8bd9f8e8127981946499760534ff70a))


### Bug Fixes

* add forgotten state.compact_peridoically() ([#324](https://github.com/matter-labs/zksync-os-server/issues/324)) ([e38846a](https://github.com/matter-labs/zksync-os-server/commit/e38846aff6061b23d5aeea833a3b3805303e43d7))

## [0.2.0](https://github.com/matter-labs/zksync-os-server/compare/v0.1.2...v0.2.0) (2025-09-02)


### ⚠ BREAKING CHANGES

* adapt server for v29 ([#284](https://github.com/matter-labs/zksync-os-server/issues/284))

### Features

* adapt server for v29 ([#284](https://github.com/matter-labs/zksync-os-server/issues/284)) ([df2d66e](https://github.com/matter-labs/zksync-os-server/commit/df2d66e46668db6812be628b7c1e49658e12b3a2))
* add observability on node init ([#290](https://github.com/matter-labs/zksync-os-server/issues/290)) ([895fd6b](https://github.com/matter-labs/zksync-os-server/commit/895fd6b2bfc720a1c0462d161f3068e1aaf2441d))
* **api:** implement `debug_traceTransaction` ([#231](https://github.com/matter-labs/zksync-os-server/issues/231)) ([15cf104](https://github.com/matter-labs/zksync-os-server/commit/15cf1044a174b539548cde2bc7abf22e4b12bfb6))
* **docker:** use new crate ([#294](https://github.com/matter-labs/zksync-os-server/issues/294)) ([3a92eae](https://github.com/matter-labs/zksync-os-server/commit/3a92eae6430389104e8881d6cd33e0fbfcd45840))
* ERC20 integration tests ([#285](https://github.com/matter-labs/zksync-os-server/issues/285)) ([3d7dac5](https://github.com/matter-labs/zksync-os-server/commit/3d7dac5bece2431ea428040c72b3802aab9e4fe0))
* move sequencer implementation to its own crate ([#291](https://github.com/matter-labs/zksync-os-server/issues/291)) ([183ee2a](https://github.com/matter-labs/zksync-os-server/commit/183ee2ae1423c3f17921d87eac301def4e2150b0))
* refactor lib.rs in sequencer ([#280](https://github.com/matter-labs/zksync-os-server/issues/280)) ([454b104](https://github.com/matter-labs/zksync-os-server/commit/454b104bb335e3183f6a46662a06b09b79172801))
* Update state - contracts: zkos-v0.29.2, zkstack tool: 0267d99b366c97 ([#305](https://github.com/matter-labs/zksync-os-server/issues/305)) ([62d234d](https://github.com/matter-labs/zksync-os-server/commit/62d234ddecfa81bbb3a8cc5534dd3c96747315cf))
* update to zkos v0.0.20 and airbender 0.4.3 ([#301](https://github.com/matter-labs/zksync-os-server/issues/301)) ([be23bef](https://github.com/matter-labs/zksync-os-server/commit/be23bef943d4ff44c6af79020d0b3ac15430958c))
* use open source prover ([#300](https://github.com/matter-labs/zksync-os-server/issues/300)) ([82370e9](https://github.com/matter-labs/zksync-os-server/commit/82370e9decad8c5625b51a9e461938d1df3a374f))


### Bug Fixes

* block count limit ([#297](https://github.com/matter-labs/zksync-os-server/issues/297)) ([080dcc5](https://github.com/matter-labs/zksync-os-server/commit/080dcc5beea9fcf34fa805c6cd7e75ea5ba024ac))
* state recovery edge case ([#299](https://github.com/matter-labs/zksync-os-server/issues/299)) ([ccee05b](https://github.com/matter-labs/zksync-os-server/commit/ccee05b01095c2c92e86abd3682b7ba3a8651892))

## [0.1.2](https://github.com/matter-labs/zksync-os-server/compare/v0.1.1...v0.1.2) (2025-08-27)


### Features

* Allow loading configs from old yaml files ([#230](https://github.com/matter-labs/zksync-os-server/issues/230)) ([272b6e7](https://github.com/matter-labs/zksync-os-server/commit/272b6e7790dc5bef6f0d6688a815f67e1ce1ef7f))
* **api:** populate RPC block size ([#217](https://github.com/matter-labs/zksync-os-server/issues/217)) ([ce24acf](https://github.com/matter-labs/zksync-os-server/commit/ce24acf026ace7a49f0271ed03e8e3da6816a863))
* **api:** safeguard `zks_getL2ToL1LogProof` to work on executed batches ([#242](https://github.com/matter-labs/zksync-os-server/issues/242)) ([1450bf1](https://github.com/matter-labs/zksync-os-server/commit/1450bf14ec853824205d9c45bbfe04274bcb1230))
* basic validium support ([73fc1d1](https://github.com/matter-labs/zksync-os-server/commit/73fc1d112aff0b4096782a727cd12bdb1d163301))
* batcher seal criteria ([#213](https://github.com/matter-labs/zksync-os-server/issues/213)) ([fe8250a](https://github.com/matter-labs/zksync-os-server/commit/fe8250a04f2c7153a3ea36ebee66ed27e03c0395))
* **docker:** use clang/LLVM 19 on Trixie ([#229](https://github.com/matter-labs/zksync-os-server/issues/229)) ([0ff5c5b](https://github.com/matter-labs/zksync-os-server/commit/0ff5c5b8d2c540b8c75aa5686e430ea8892762d1))
* external node ([#163](https://github.com/matter-labs/zksync-os-server/issues/163)) ([d595e64](https://github.com/matter-labs/zksync-os-server/commit/d595e64f29112fa221a3ecdbf1499f5f3d14f15e))
* more metrics ([686cc12](https://github.com/matter-labs/zksync-os-server/commit/686cc12c7b328458240f594965bf92deaf25c9df))
* new state impl ([#278](https://github.com/matter-labs/zksync-os-server/issues/278)) ([6410653](https://github.com/matter-labs/zksync-os-server/commit/6410653e1f2c1ee8305f7013b503c56a094dd788))
* periodic collections of component states ([3b20513](https://github.com/matter-labs/zksync-os-server/commit/3b20513515f2f4bd116189bc4104296606ed8f1f))
* process genesis upgrade tx ([#201](https://github.com/matter-labs/zksync-os-server/issues/201)) ([9cc9a9c](https://github.com/matter-labs/zksync-os-server/commit/9cc9a9c79b3c44a242c1a8c66eaa7fb0014bfb09))
* **proof-storage:** use object store ([#225](https://github.com/matter-labs/zksync-os-server/issues/225)) ([0342daa](https://github.com/matter-labs/zksync-os-server/commit/0342daae9ba404df55cb2fbd6fca76dcf80773c7))
* refactor config ([#246](https://github.com/matter-labs/zksync-os-server/issues/246)) ([6ef1f06](https://github.com/matter-labs/zksync-os-server/commit/6ef1f061150fc639c42d24acf1e3f3847108d795))
* refine component state tracking ([#256](https://github.com/matter-labs/zksync-os-server/issues/256)) ([8b64257](https://github.com/matter-labs/zksync-os-server/commit/8b64257866d052e1d121735d3faf7c195082bfaf))
* speed-up batch storage lookup ([#273](https://github.com/matter-labs/zksync-os-server/issues/273)) ([1d24514](https://github.com/matter-labs/zksync-os-server/commit/1d24514cd8f33f41cdc9aaa45623df5b8aa03bf9))
* **storage:** add `ReadStateHistory` trait ([#244](https://github.com/matter-labs/zksync-os-server/issues/244)) ([1e7a4bb](https://github.com/matter-labs/zksync-os-server/commit/1e7a4bb22dd686c0dfe4ad99e4ff4dc1fb128dc7))
* Update codebase to use v0.3.3 verifiers ([#223](https://github.com/matter-labs/zksync-os-server/issues/223)) ([f457bcf](https://github.com/matter-labs/zksync-os-server/commit/f457bcf68f7cf4e8e4ec39e1cbf1d2b40ce74363))
* upgrade bincode to v2 ([#274](https://github.com/matter-labs/zksync-os-server/issues/274)) ([b5066b1](https://github.com/matter-labs/zksync-os-server/commit/b5066b12f80482df9026f70d29aad96ac7901768))
* zksync os bump to 0.0.13 ([#283](https://github.com/matter-labs/zksync-os-server/issues/283)) ([177364a](https://github.com/matter-labs/zksync-os-server/commit/177364a33b064897d77b47d41ae4a98460d3f6f2))


### Bug Fixes

* always replay at least one block ([#281](https://github.com/matter-labs/zksync-os-server/issues/281)) ([b298988](https://github.com/matter-labs/zksync-os-server/commit/b2989887dbf773cd82dce26701229d96154036f3))
* **api:** flatten L1 tx envelopes ([#234](https://github.com/matter-labs/zksync-os-server/issues/234)) ([f4e4296](https://github.com/matter-labs/zksync-os-server/commit/f4e429601644de63564bc17138db841d80ed2a79))
* **api:** proper type id for txs in api ([#269](https://github.com/matter-labs/zksync-os-server/issues/269)) ([c6993b7](https://github.com/matter-labs/zksync-os-server/commit/c6993b761ba5713411e697485e20b0842ecddf41))
* commit- and execute- watchers - fix one-off error in batch numbers ([53976e0](https://github.com/matter-labs/zksync-os-server/commit/53976e09522bdaf256f96ed529cc1b1435b43f51))
* **docker:** add genesis.json to docker image ([#220](https://github.com/matter-labs/zksync-os-server/issues/220)) ([2b2c3d0](https://github.com/matter-labs/zksync-os-server/commit/2b2c3d0eed11e8c4a2f36a80f935433109b8f63b))
* EN and handle errors more gracefully ([#247](https://github.com/matter-labs/zksync-os-server/issues/247)) ([0af3d9c](https://github.com/matter-labs/zksync-os-server/commit/0af3d9ca9991f65100f0f0c594292cbef7fa9d9f))
* **l1:** various `alloy::Provider` improvements ([#272](https://github.com/matter-labs/zksync-os-server/issues/272)) ([1f4fca4](https://github.com/matter-labs/zksync-os-server/commit/1f4fca47d991c63f161d2227312e0d8d5131d191))
* main after EN, serde/bincode accident ([#221](https://github.com/matter-labs/zksync-os-server/issues/221)) ([a7b4a2f](https://github.com/matter-labs/zksync-os-server/commit/a7b4a2f357d7427a116ff165181744da5a139a85))
* make get_transaction_receipt fallible ([#279](https://github.com/matter-labs/zksync-os-server/issues/279)) ([16cce7b](https://github.com/matter-labs/zksync-os-server/commit/16cce7be82ac39d68abb0facdfdd68bf1c833c70))
* set correct default for pubdata limit ([#241](https://github.com/matter-labs/zksync-os-server/issues/241)) ([2beb101](https://github.com/matter-labs/zksync-os-server/commit/2beb10194040cbc32220f56b4d3bb2dbe42b650d))
* skip already committed blocks before main batcher loop ([#286](https://github.com/matter-labs/zksync-os-server/issues/286)) ([7e9ea74](https://github.com/matter-labs/zksync-os-server/commit/7e9ea74c09d48b6fea677335d2d847e452fb17a1))
* start from batch number instead of block number ([#228](https://github.com/matter-labs/zksync-os-server/issues/228)) ([241a00e](https://github.com/matter-labs/zksync-os-server/commit/241a00e73a4d32bb317843205f7d5e9a3d67bf3e))
* temporary disable l1 commit and execute watchers ([99bdfbc](https://github.com/matter-labs/zksync-os-server/commit/99bdfbc627276e8c80f08e9c8320d5b0e5d4ab44))
* track timeout seal criteria in batcher ([b136822](https://github.com/matter-labs/zksync-os-server/commit/b1368224e51d5458921e817d952e1e495a12994b))
* use validium-rollup setting from L1 - not config; fix integration tests ([#255](https://github.com/matter-labs/zksync-os-server/issues/255)) ([19a1a82](https://github.com/matter-labs/zksync-os-server/commit/19a1a8283c6162fc0d822e241d5a5c5aa7f0ed27))

## [0.1.1](https://github.com/matter-labs/zksync-os-server/compare/v0.1.0...v0.1.1) (2025-08-19)


### Features

* add mini merkle tree crate ([#169](https://github.com/matter-labs/zksync-os-server/issues/169)) ([3c068ea](https://github.com/matter-labs/zksync-os-server/commit/3c068ead7d98dc7fd8441f7e5ad41b9619c3e44a))
* allow replaying blocks from zero ([#197](https://github.com/matter-labs/zksync-os-server/issues/197)) ([b0da499](https://github.com/matter-labs/zksync-os-server/commit/b0da499e09a978b55aa3c5bf0e278ac2dd20ad54))
* **api:** implement `ots_` namespace; add support for local Otterscan ([#168](https://github.com/matter-labs/zksync-os-server/issues/168)) ([dae4794](https://github.com/matter-labs/zksync-os-server/commit/dae47942cfadc885910b5ab0f158a2ef16612dd3))
* **api:** implement `zks_getL2ToL1LogProof` ([#203](https://github.com/matter-labs/zksync-os-server/issues/203)) ([c83e1c8](https://github.com/matter-labs/zksync-os-server/commit/c83e1c8e078f7346f4f3ded10d90d35c6f9b108c))
* **api:** limit req/resp body size ([#204](https://github.com/matter-labs/zksync-os-server/issues/204)) ([db19257](https://github.com/matter-labs/zksync-os-server/commit/db19257919f8cacb37cafa079d42f8fa0b4af548))
* component state observability ([#187](https://github.com/matter-labs/zksync-os-server/issues/187)) ([d961485](https://github.com/matter-labs/zksync-os-server/commit/d961485a5d3204a92eb2a2e6ab0bfb4d60c31190))
* dump block input on `run_block` error ([#165](https://github.com/matter-labs/zksync-os-server/issues/165)) ([75f76ac](https://github.com/matter-labs/zksync-os-server/commit/75f76acda4bd167c22b88c3b9567a71a54fac7bc))
* Instructions on how to run 2 chains, and prometheus config ([#195](https://github.com/matter-labs/zksync-os-server/issues/195)) ([3b890fb](https://github.com/matter-labs/zksync-os-server/commit/3b890fb8c2c8ead4c1dbf6e343d8f735ed5230d5))
* **l1-sender:** basic http support ([#175](https://github.com/matter-labs/zksync-os-server/issues/175)) ([92a90fa](https://github.com/matter-labs/zksync-os-server/commit/92a90fa8d65b04d8368e074afc40f1992d684b72))
* **l1-sender:** implement L1 batch execution ([#157](https://github.com/matter-labs/zksync-os-server/issues/157)) ([5d27812](https://github.com/matter-labs/zksync-os-server/commit/5d278121f4c0abe37e416b82c663ae8b9b4f04f7))
* **l1-watcher:** implement basic `L1CommitWatcher` ([#189](https://github.com/matter-labs/zksync-os-server/issues/189)) ([326ac6b](https://github.com/matter-labs/zksync-os-server/commit/326ac6b33c069a46e6648388e396f86d2a1b49bf))
* **l1-watcher:** track last committed block ([#194](https://github.com/matter-labs/zksync-os-server/issues/194)) ([dda3a18](https://github.com/matter-labs/zksync-os-server/commit/dda3a1884b33501eb287c14f01a67406e0981dbc))
* **l1-watcher:** track last executed block ([#199](https://github.com/matter-labs/zksync-os-server/issues/199)) ([c34194d](https://github.com/matter-labs/zksync-os-server/commit/c34194d81d90cab4e654ffc7b0638c8420f6ff20))
* limit number of blocks per batch ([#192](https://github.com/matter-labs/zksync-os-server/issues/192)) ([195ce8f](https://github.com/matter-labs/zksync-os-server/commit/195ce8ffb737b96ee11ad79e83c72a7fd809c472))
* proper batching ([#167](https://github.com/matter-labs/zksync-os-server/issues/167)) ([e3b5ebc](https://github.com/matter-labs/zksync-os-server/commit/e3b5ebc9fc46d74594a9cc897f0d7efc5f367a41))
* report earliest block number ([#216](https://github.com/matter-labs/zksync-os-server/issues/216)) ([af9263f](https://github.com/matter-labs/zksync-os-server/commit/af9263f078146c9370460ebd748cd07f33780f9b))
* save `node_version` and `block_output_hash` in `ReplayRecord` ([#162](https://github.com/matter-labs/zksync-os-server/issues/162)) ([50eb1af](https://github.com/matter-labs/zksync-os-server/commit/50eb1afb70649b2ec23d82191e946cc3beec03a6))
* save proper block 0 ([#198](https://github.com/matter-labs/zksync-os-server/issues/198)) ([ca8d46b](https://github.com/matter-labs/zksync-os-server/commit/ca8d46b585b88652a064a53cc709ab05d20a554d))
* setup release please ([#156](https://github.com/matter-labs/zksync-os-server/issues/156)) ([0a0f170](https://github.com/matter-labs/zksync-os-server/commit/0a0f170d2f22ffc3580a30d0f16db21eb01766d9))
* **storage:** implement batch storage ([#200](https://github.com/matter-labs/zksync-os-server/issues/200)) ([0c06f14](https://github.com/matter-labs/zksync-os-server/commit/0c06f14fa3cda7f4768464da1d3e8130b39a9c5a))
* support real SNARK provers ([#164](https://github.com/matter-labs/zksync-os-server/issues/164)) ([5ced71c](https://github.com/matter-labs/zksync-os-server/commit/5ced71c9bb147bf2cc8ec1eaabcc29dad0ef8c61))
* unify batcher subsystem latency tracking ([#170](https://github.com/matter-labs/zksync-os-server/issues/170)) ([25e0301](https://github.com/matter-labs/zksync-os-server/commit/25e030194c58665b35f5af6c4e38662473302d1f))
* upgrade zksync-os to 0.0.10 ([#215](https://github.com/matter-labs/zksync-os-server/issues/215)) ([53a4e82](https://github.com/matter-labs/zksync-os-server/commit/53a4e824990da42e37b55e406f1308d5d92ead25))


### Bug Fixes

* adopt some channel capacity to accomodate all rescheduled jobs ([2bd5878](https://github.com/matter-labs/zksync-os-server/commit/2bd5878eb7fac663b00782b3d8394d89195f1f5c))
* **api:** disable Prague in mempool ([9c00b42](https://github.com/matter-labs/zksync-os-server/commit/9c00b427ee78266327406cf2fd60b37d3ab968c3))
* **l1-watch:** support new deployments ([#166](https://github.com/matter-labs/zksync-os-server/issues/166)) ([8215db9](https://github.com/matter-labs/zksync-os-server/commit/8215db9de3bf614e2e527a1aad9467bcc9d101a5))
* skip already processed l1 transactions in watcher on restart ([#172](https://github.com/matter-labs/zksync-os-server/issues/172)) ([b290405](https://github.com/matter-labs/zksync-os-server/commit/b290405529160de4840ac12f1b90cc8161026a15))
* state recovery - read persisted repository block - not memory ([#191](https://github.com/matter-labs/zksync-os-server/issues/191)) ([146cb19](https://github.com/matter-labs/zksync-os-server/commit/146cb19f3798fc064fcb9b771b200dcefa266f43))
* **storage:** report proper lazy latest block ([#193](https://github.com/matter-labs/zksync-os-server/issues/193)) ([a570006](https://github.com/matter-labs/zksync-os-server/commit/a570006b3a215f48fa117b4ab47870b707b770da))
* update release version suffix for crates in CI ([#159](https://github.com/matter-labs/zksync-os-server/issues/159)) ([8c661fe](https://github.com/matter-labs/zksync-os-server/commit/8c661fea8e30d2d3396161b3f9013085c4de467a))
* use spawn instead of select! to start everything ([#185](https://github.com/matter-labs/zksync-os-server/issues/185)) ([09a71af](https://github.com/matter-labs/zksync-os-server/commit/09a71afef222835282c3a1952ef4f04793603c26))
