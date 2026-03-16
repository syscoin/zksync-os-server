mod releases;
mod scenario;
mod server;
mod settlement;

pub use scenario::{
    restart_from_previous_minor_is_not_operational,
    restart_from_previous_patch_settles_three_batches,
};

const REPO: &str = "matter-labs/zksync-os-server";
const EXPECTED_BATCHES_PER_PHASE: u64 = 3;
const MAX_PHASE_TXS: usize = 24;
const RELEASE_DOWNLOAD_DIR: &str = "server-binaries";
const CURRENT_BIN_ENV: &str = "ZKSYNC_OS_SERVER_BIN";
const DEFAULT_RICH_PRIVATE_KEY: &str =
    "0x7726827caac94a7f9e1b160f7ea819f172f7b6f9d2a97f992c38edeab82d4110";
