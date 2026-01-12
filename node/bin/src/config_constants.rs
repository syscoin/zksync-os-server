//! This file contains constants that are dependent on local state.
//! Please keep it in the `const VAR: type = "val"` format only
//! as it is used to be automatically updated.
//! Please, use #[rustfmt::skip] if a constant is formatted to occupy two lines.

/// Default path to RocksDB storage.
pub const DEFAULT_ROCKS_DB_PATH: &str = "./db/node1";

/// L1 address of `Bridgehub` contract. This address and chain ID is an entrypoint into L1 discoverability so most
/// other contracts should be discoverable through it.
pub const BRIDGEHUB_ADDRESS: &str = "0xf7478d9a67818964cebfe8a4b76e2b72b0bf790d";

/// L1 address of the `BytecodeSupplier` contract. This address right now cannot be discovered through `Bridgehub`,
/// so it has to be provided explicitly.
pub const BYTECODE_SUPPLIER_ADDRESS: &str = "0xaa22a7bcb533db51540e39fcf9d311464faea0a5";

/// Chain ID of the chain node operates on.
pub const CHAIN_ID: u64 = 6565;

/// Private key to commit batches to L1
/// Must be consistent with the operator key set on the contract (permissioned!)
#[rustfmt::skip]
pub const OPERATOR_COMMIT_PK: &str = "0xb83bd9e5c1952dd3b28c36ab82d977d126915012ffd364f9b4dc45a8745aa387";

/// Private key to use to submit proofs to L1
/// Can be arbitrary funded address - proof submission is permissionless.
#[rustfmt::skip]
pub const OPERATOR_PROVE_PK: &str = "0x5d52d0da0bd90e235744df340dc1391f9a1d8d2068e7a832c1ff5c93945fbaea";

/// Private key to use to execute batches on L1
/// Can be arbitrary funded address - execute submission is permissionless.
#[rustfmt::skip]
pub const OPERATOR_EXECUTE_PK: &str = "0x89c0d4553e4cdf9142d06906faf5a9828b61b782dddbb4896d1161e408e3767c";
