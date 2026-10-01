//! Prometheus-related functionality, such as [`PrometheusExporterConfig`].

use std::{
    env,
    net::{IpAddr, Ipv4Addr, SocketAddr},
    time::Duration,
};

use anyhow::Context as _;
use reth_tasks::shutdown::GracefulShutdown;
use vise::{MetricsCollection, Registry};
use vise_exporter::MetricsExporter;

#[cfg(all(feature = "jemalloc", target_family = "unix"))]
use crate::jemalloc;
use crate::tokio_runtime;

#[derive(Debug, Clone)]
enum PrometheusTransport {
    Pull {
        address: SocketAddr,
    },
    Push {
        gateway_uri: String,
        interval: Duration,
    },
}

/// Configuration of a Prometheus exporter.
#[derive(Debug, Clone)]
pub struct PrometheusExporterConfig {
    transport: PrometheusTransport,
}

impl PrometheusExporterConfig {
    /// Creates an exporter that will run an HTTP server on the specified `port`.
    pub const fn pull(port: u16) -> Self {
        Self::pull_at(IpAddr::V4(Ipv4Addr::UNSPECIFIED), port)
    }

    /// SYSCOIN: Private validation nodes can expose metrics only on loopback
    /// without changing the existing deployment default or firewall policy.
    pub const fn pull_at(bind_address: IpAddr, port: u16) -> Self {
        Self {
            transport: PrometheusTransport::Pull {
                address: SocketAddr::new(bind_address, port),
            },
        }
    }

    /// Creates an exporter that will push metrics to the specified Prometheus gateway endpoint.
    pub const fn push(gateway_uri: String, interval: Duration) -> Self {
        Self {
            transport: PrometheusTransport::Push {
                gateway_uri,
                interval,
            },
        }
    }

    /// Creates a full push gateway endpoint.
    pub fn gateway_endpoint(base_url: &str) -> String {
        let job_id = "zksync-pushgateway";
        let namespace =
            env::var("POD_NAMESPACE").unwrap_or_else(|_| "UNKNOWN_NAMESPACE".to_owned());
        let pod = env::var("HOSTNAME").unwrap_or_else(|_| "UNKNOWN_POD".to_owned());
        format!("{base_url}/metrics/job/{job_id}/namespace/{namespace}/pod/{pod}")
    }

    /// Get the list of metrics that use this type of exporter (Push vs Pull)
    /// Only groups with the `PushMetrics` suffix are exported using the push exporter.
    fn registry(&self) -> Registry {
        let is_push_exporter = matches!(self.transport, PrometheusTransport::Push { .. });
        MetricsCollection::lazy()
            .filter(|group| group.name.ends_with("PushMetrics") == is_push_exporter)
            .collect()
    }

    /// Runs the exporter. This future should be spawned in a separate Tokio task.
    pub async fn run(self, shutdown: GracefulShutdown) -> anyhow::Result<()> {
        tokio_runtime::register_monitor();
        #[cfg(all(feature = "jemalloc", target_family = "unix"))]
        jemalloc::register_monitor();

        let registry = self.registry();
        // ignore_guard will drop the guard too early, so clone is used.
        let metrics_exporter = MetricsExporter::new(registry.into())
            .with_graceful_shutdown(shutdown.clone().ignore_guard());

        match self.transport {
            PrometheusTransport::Pull { address } => {
                metrics_exporter
                    .start(address)
                    .await
                    .context("Failed starting metrics server")?;
            }
            PrometheusTransport::Push {
                gateway_uri,
                interval,
            } => {
                let endpoint = gateway_uri
                    .parse()
                    .context("Failed parsing Prometheus push gateway endpoint")?;
                metrics_exporter.push_to_gateway(endpoint, interval).await;
            }
        }
        // We can drop it now because shutdown is complete.
        drop(shutdown);
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn pull_preserves_default_and_supports_explicit_loopback() {
        for (config, expected) in [
            (PrometheusExporterConfig::pull(3312), "0.0.0.0:3312"),
            (
                PrometheusExporterConfig::pull_at("127.0.0.1".parse().unwrap(), 3312),
                "127.0.0.1:3312",
            ),
            (
                PrometheusExporterConfig::pull_at("::1".parse().unwrap(), 3312),
                "[::1]:3312",
            ),
        ] {
            let PrometheusTransport::Pull { address } = config.transport else {
                panic!("expected pull transport");
            };
            assert_eq!(address.to_string(), expected);
        }
    }
}
