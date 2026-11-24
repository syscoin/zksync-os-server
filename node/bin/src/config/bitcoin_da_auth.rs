use super::BatcherConfig;
use anyhow::{Context, ensure};
use secrecy::ExposeSecret;
use smart_config::value::SecretString;
use zksync_os_gas_adjuster::validate_bitcoin_da_rpc_url;

impl BatcherConfig {
    pub(crate) fn resolve_bitcoin_da_auth(&mut self) -> anyhow::Result<()> {
        let cookie_file = self.bitcoin_da_cookie_file.as_deref();
        let cookie_rpc_url = self.bitcoin_da_cookie_rpc_url.as_deref();
        let Some(rpc_url) = &self.bitcoin_da_rpc_url else {
            ensure!(
                cookie_file.is_none()
                    && cookie_rpc_url.is_none()
                    && self.bitcoin_da_rpc_user.is_none()
                    && self.bitcoin_da_rpc_password.is_none(),
                "Bitcoin DA credentials require batcher.bitcoin_da_rpc_url"
            );
            return Ok(());
        };

        validate_bitcoin_da_rpc_url(rpc_url, cookie_file.is_some())?;
        let Some(path) = cookie_file else {
            ensure!(
                cookie_rpc_url.is_none(),
                "batcher.bitcoin_da_cookie_rpc_url requires batcher.bitcoin_da_cookie_file"
            );
            ensure!(
                [&self.bitcoin_da_rpc_user, &self.bitcoin_da_rpc_password]
                    .into_iter()
                    .all(|credential| credential
                        .as_ref()
                        .is_some_and(|value| !value.expose_secret().trim().is_empty())),
                "Bitcoin DA RPC requires both explicit credentials or a configured cookie"
            );
            return Ok(());
        };
        ensure!(
            self.bitcoin_da_rpc_user.is_none() && self.bitcoin_da_rpc_password.is_none(),
            "choose either batcher.bitcoin_da_cookie_file or explicit Bitcoin DA RPC credentials"
        );
        ensure!(
            cookie_rpc_url == Some(rpc_url.as_str()),
            "effective batcher.bitcoin_da_rpc_url must match batcher.bitcoin_da_cookie_rpc_url before using the cookie"
        );
        ensure!(
            path.is_absolute() && path.is_file(),
            "batcher.bitcoin_da_cookie_file must be an absolute path to a regular file"
        );

        let cookie = std::fs::read_to_string(path)
            .context("could not read configured Bitcoin DA RPC cookie")?;
        let (user, password) =
            cookie_credentials(&cookie).context("invalid configured Bitcoin DA RPC cookie")?;
        self.bitcoin_da_rpc_user = Some(SecretString::new(user.to_owned().into()));
        self.bitcoin_da_rpc_password = Some(SecretString::new(password.to_owned().into()));
        Ok(())
    }
}

fn cookie_credentials(cookie: &str) -> Option<(&str, &str)> {
    let cookie = cookie.trim_end_matches(['\r', '\n']);
    let (user, password) = cookie.split_once(':')?;
    if user.trim().is_empty() || password.trim().is_empty() || cookie.contains(['\r', '\n', '\0']) {
        return None;
    }
    Some((user, password))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;

    #[test]
    fn accepts_literal_loopback_and_remote_https_with_explicit_credentials() {
        assert!(validate_bitcoin_da_rpc_url("http://127.0.0.1:8370", true).is_ok());
        assert!(validate_bitcoin_da_rpc_url("http://[::1]:8370", true).is_ok());
        assert!(validate_bitcoin_da_rpc_url("https://core.example.com:8370", false).is_ok());
    }

    #[test]
    fn retains_explicit_https_credentials() {
        let mut config = BatcherConfig {
            bitcoin_da_rpc_url: Some("https://core.example.com:8370".into()),
            bitcoin_da_rpc_user: Some(SecretString::new("provider-user".to_owned().into())),
            bitcoin_da_rpc_password: Some(SecretString::new("provider-pass".to_owned().into())),
            ..BatcherConfig::default()
        };
        config.resolve_bitcoin_da_auth().unwrap();
        assert_eq!(
            config.bitcoin_da_rpc_user.as_ref().unwrap().expose_secret(),
            "provider-user"
        );
        assert_eq!(
            config
                .bitcoin_da_rpc_password
                .as_ref()
                .unwrap()
                .expose_secret(),
            "provider-pass"
        );
        config.bitcoin_da_rpc_password = None;
        assert!(config.resolve_bitcoin_da_auth().is_err());
    }

    #[test]
    fn resolves_cookie_in_memory_at_startup() {
        let mut cookie = tempfile::NamedTempFile::new().unwrap();
        writeln!(cookie, "__cookie__:first:password").unwrap();
        let original = BatcherConfig {
            bitcoin_da_rpc_url: Some("http://127.0.0.1:8370".into()),
            bitcoin_da_cookie_file: Some(cookie.path().to_path_buf()),
            bitcoin_da_cookie_rpc_url: Some("http://127.0.0.1:8370".into()),
            ..BatcherConfig::default()
        };
        let mut redirected = original.clone();
        redirected.bitcoin_da_rpc_url = Some("http://127.0.0.1:18370".into());
        assert!(
            redirected
                .resolve_bitcoin_da_auth()
                .unwrap_err()
                .to_string()
                .contains("must match")
        );

        let mut config = original.clone();
        config.resolve_bitcoin_da_auth().unwrap();
        assert_eq!(
            config.bitcoin_da_rpc_user.unwrap().expose_secret(),
            "__cookie__"
        );
        assert_eq!(
            config.bitcoin_da_rpc_password.unwrap().expose_secret(),
            "first:password"
        );

        std::fs::write(cookie.path(), "__cookie__:rotated\n").unwrap();
        let mut config = original;
        config.resolve_bitcoin_da_auth().unwrap();
        assert_eq!(
            config.bitcoin_da_rpc_password.unwrap().expose_secret(),
            "rotated"
        );
    }

    #[test]
    fn rejects_disallowed_endpoint_or_auth_mode() {
        assert!(validate_bitcoin_da_rpc_url("http://core.example.com:8370", false).is_err());
        assert!(validate_bitcoin_da_rpc_url("https://core.example.com:8370", true).is_err());
        assert!(
            validate_bitcoin_da_rpc_url("https://user:password@core.example.com:8370", false)
                .is_err()
        );
        let mut config = BatcherConfig {
            bitcoin_da_rpc_url: Some("http://127.0.0.1:8370".into()),
            bitcoin_da_cookie_file: Some("/path/to/.cookie".into()),
            bitcoin_da_rpc_user: Some(SecretString::new("user".to_owned().into())),
            ..BatcherConfig::default()
        };
        assert!(config.resolve_bitcoin_da_auth().is_err());
    }
}
