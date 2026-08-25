//! Please, use #[rustfmt::skip] if a constant is formatted to occupy two lines.

// TODO: to be moved to config instead of constants
/// Default path to RocksDB storage.
pub const DEFAULT_ROCKS_DB_PATH: &str = "./db/node1";

/// SYSCOIN: The only supported protocol version in the fresh deployment lane.
pub const PROTOCOL_VERSION_V32_0: &str = "v32.0";

/// Current default protocol version for local chain configuration.
pub const PROTOCOL_VERSION: &str = PROTOCOL_VERSION_V32_0;
