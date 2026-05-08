use std::time::Duration;

use secrecy::SecretString;

// SYSCOIN: Settings used by batch-verifier clients to independently validate
// committed Syscoin DA blob hashes before signing a batch.
#[derive(Clone, Debug)]
pub struct SyscoinDaVerificationConfig {
    pub rpc_url: String,
    pub rpc_user: SecretString,
    pub rpc_password: SecretString,
    pub poda_url: String,
    pub wallet_name: String,
    pub request_timeout: Duration,
}

/// Struct matches zksync_os_server::config::BatchVerificationConfig.
/// See there for documentation
#[derive(Clone, Debug)]
pub struct BatchVerificationConfig {
    pub server_enabled: bool,
    pub client_enabled: bool,
    pub threshold: u64,
    pub accepted_signers: Vec<String>,
    pub request_timeout: Duration,
    pub retry_delay: Duration,
    pub total_timeout: Duration,
    pub signing_key: SecretString,
    pub syscoin_da_verification: Option<SyscoinDaVerificationConfig>,
}
