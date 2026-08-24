use crate::config::Config;
use anyhow::Context;
use std::io::ErrorKind;
use std::net::SocketAddr;
use std::time::Duration;
use tokio::net::{TcpListener, TcpSocket};
use tokio::time::Instant;
use zksync_os_network::NetworkPorts;

/// How long to keep retrying a bind that fails with `AddrInUse`; a port pinned across restart
/// (e.g. in integration tests) may be transiently occupied by another process's socket.
const BIND_RETRY_TIMEOUT: Duration = Duration::from_secs(5);
const BIND_RETRY_INTERVAL: Duration = Duration::from_millis(250);
// SYSCOIN: Match Tokio / Mio's normal listener backlog when promoting the reserved prover socket.
const LISTEN_BACKLOG: u32 = 1024;

/// Actual ports bound by each service after `run()` starts.
/// Fields are `None` when the corresponding service is disabled in config.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ServerPorts {
    pub rpc: u16,
    pub status: Option<u16>,
    pub prover_api: Option<u16>,
    pub network: Option<NetworkPorts>,
}

/// Sockets bound before node startup and then handed to their servers.
#[derive(Debug)]
pub(crate) struct BoundListeners {
    pub(crate) rpc: TcpListener,
    pub(crate) status: Option<TcpListener>,
    // SYSCOIN: Reserve the real prover port without listening until durable SNARK recovery has
    // completed. This retains early `AddrInUse` detection and the actual port selected for `:0`.
    pub(crate) prover_api: Option<ReservedTcpSocket>,
}

/// SYSCOIN: A bound TCP socket that owns its address but cannot accept connections until the
/// readiness-gated prover task explicitly promotes it to a listener.
#[derive(Debug)]
pub(crate) struct ReservedTcpSocket {
    socket: TcpSocket,
}

impl ReservedTcpSocket {
    pub(crate) fn local_addr(&self) -> std::io::Result<SocketAddr> {
        self.socket.local_addr()
    }

    pub(crate) fn listen(self) -> std::io::Result<TcpListener> {
        let listener = self.socket.listen(LISTEN_BACKLOG)?;
        // SYSCOIN: Listen while the bound reservation is still exclusive, then safely restore
        // Tokio's normal Unix listener option so clean restarts can reuse an address with
        // connections in `TIME_WAIT`. No reusable bound-only address-steal window is introduced.
        #[cfg(unix)]
        socket2::SockRef::from(&listener).set_reuse_address(true)?;
        Ok(listener)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Service {
    Rpc,
    Status,
    ProverApi,
}

impl std::fmt::Display for Service {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let label = match self {
            Service::Rpc => "RPC",
            Service::Status => "status",
            Service::ProverApi => "prover API",
        };
        write!(f, "{label}")
    }
}

impl BoundListeners {
    pub(crate) async fn bind_from_config(config: &Config) -> anyhow::Result<Self> {
        let status_address = config
            .status_server_config
            .enabled
            .then_some(config.status_server_config.address.as_str());
        let prover_api_address = (config.general_config.node_role.is_main()
            && config.batcher_config.enabled
            && config.prover_api_config.enabled)
            .then_some(config.prover_api_config.address.as_str());

        Self::bind(
            &config.rpc_config.address,
            status_address,
            prover_api_address,
        )
        .await
    }

    async fn bind(
        rpc_address: &str,
        status_address: Option<&str>,
        prover_api_address: Option<&str>,
    ) -> anyhow::Result<Self> {
        let rpc = bind_tcp_listener(rpc_address, Service::Rpc).await?;
        let status = match status_address {
            Some(address) => Some(bind_tcp_listener(address, Service::Status).await?),
            None => None,
        };
        let prover_api = match prover_api_address {
            // SYSCOIN: Binding without `listen(2)` keeps the prover API network-closed throughout
            // durable journal recovery while still reserving its configured or ephemeral port.
            Some(address) => Some(bind_reserved_tcp_socket(address, Service::ProverApi).await?),
            None => None,
        };

        Ok(Self {
            rpc,
            status,
            prover_api,
        })
    }
}

// SYSCOIN: Mirror `TcpListener::bind` socket options but deliberately defer `listen(2)` until the
// SNARK readiness barrier. A fresh socket is required for every retry after `AddrInUse`.
async fn bind_reserved_tcp_socket(
    address: &str,
    service: Service,
) -> anyhow::Result<ReservedTcpSocket> {
    let addr: SocketAddr = address
        .parse()
        .with_context(|| format!("malformed {service} bind address {address:?}"))?;
    let deadline = Instant::now() + BIND_RETRY_TIMEOUT;
    loop {
        let socket = if addr.is_ipv4() {
            TcpSocket::new_v4()
        } else {
            TcpSocket::new_v6()
        }
        .with_context(|| format!("failed to create prebound {service} socket at {address}"))?;
        // Tokio's normal `TcpListener::bind` enables this on Berkeley-derived sockets so a clean
        // restart does not wait for stale TCP state. Disable it immediately after binding: unlike
        // a listener, a reusable bound-only socket could otherwise lose the address to another
        // reusable bind before readiness promotes it. Windows intentionally keeps its safer default.
        #[cfg(not(windows))]
        socket.set_reuseaddr(true).with_context(|| {
            format!("failed to configure prebound {service} socket at {address}")
        })?;

        match socket.bind(addr) {
            Ok(()) => {
                #[cfg(not(windows))]
                socket.set_reuseaddr(false).with_context(|| {
                    format!("failed to make reserved {service} socket exclusive at {address}")
                })?;
                return Ok(ReservedTcpSocket { socket });
            }
            Err(err) if err.kind() == ErrorKind::AddrInUse && Instant::now() < deadline => {
                tracing::warn!(%addr, %service, "bind address in use; retrying");
                tokio::time::sleep(BIND_RETRY_INTERVAL).await;
            }
            Err(err) => {
                return Err(err)
                    .with_context(|| format!("failed to prebind {service} socket at {address}"));
            }
        }
    }
}

async fn bind_tcp_listener(address: &str, service: Service) -> anyhow::Result<TcpListener> {
    let addr: SocketAddr = address
        .parse()
        .with_context(|| format!("malformed {service} bind address {address:?}"))?;
    let deadline = Instant::now() + BIND_RETRY_TIMEOUT;
    loop {
        match TcpListener::bind(addr).await {
            Ok(listener) => return Ok(listener),
            Err(err) if err.kind() == ErrorKind::AddrInUse && Instant::now() < deadline => {
                tracing::warn!(%addr, %service, "bind address in use; retrying");
                tokio::time::sleep(BIND_RETRY_INTERVAL).await;
            }
            Err(err) => {
                return Err(err)
                    .with_context(|| format!("failed to prebind {service} listener at {address}"));
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tokio::net::TcpStream;

    // SYSCOIN: A reserved prover socket owns and reports its real `:0` port but does not expose a
    // TCP accept backlog until the durable-readiness task explicitly promotes it.
    #[tokio::test]
    async fn reserved_prover_socket_is_not_connectable_until_listen() -> anyhow::Result<()> {
        let socket = bind_reserved_tcp_socket("127.0.0.1:0", Service::ProverApi).await?;
        let address = socket.local_addr()?;
        assert_ne!(
            address.port(),
            0,
            "the OS must resolve an ephemeral port at bind"
        );
        assert!(
            TcpListener::bind(address).await.is_err(),
            "a second listener stole the reserved prover address"
        );

        let early_connect =
            tokio::time::timeout(Duration::from_millis(250), TcpStream::connect(address)).await;
        assert!(
            !matches!(early_connect, Ok(Ok(_))),
            "the reserved prover socket accepted a connection before readiness"
        );

        let listener = socket.listen()?;
        assert_eq!(listener.local_addr()?, address);
        #[cfg(unix)]
        assert!(
            socket2::SockRef::from(&listener).reuse_address()?,
            "the promoted listener did not restore clean-restart address reuse"
        );
        assert!(
            TcpListener::bind(address).await.is_err(),
            "a second listener stole the promoted prover address"
        );
        let (connected, accepted) = tokio::join!(TcpStream::connect(address), listener.accept());
        connected?;
        accepted?;
        Ok(())
    }
}
