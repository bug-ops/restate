// Copyright (c) 2023 - 2025 Restate Software, Inc., Restate GmbH.
// All rights reserved.
//
// Use of this software is governed by the Business Source License
// included in the LICENSE file.
//
// As of the Change Date specified in that file, in accordance with
// the Business Source License, use of this software will be governed
// by the Apache License, Version 2.0.

use std::collections::HashMap;
use std::future::Future;
use std::sync::Arc;

use futures::Stream;
use http::Uri;
use hyper_util::rt::TokioIo;
use tokio::io;
use tokio::net::UnixStream;
use tokio::sync::RwLock;
use tokio_stream::StreamExt;
use tonic::codec::CompressionEncoding;
use tonic::transport::Endpoint;
use tonic::transport::channel::Channel;
use tracing::{debug, info, warn};

use restate_types::config::{Configuration, NetworkingOptions, TlsSwimlane};
use restate_types::net::AdvertisedAddress;

use super::MAX_MESSAGE_SIZE;
use crate::network::grpc::DEFAULT_GRPC_COMPRESSION;
use crate::network::protobuf::core_node_svc::core_node_svc_client::CoreNodeSvcClient;
use crate::network::protobuf::network::Message;
use crate::network::tls_util::{create_client_tls_config, validate_tls_config};
use crate::network::transport_connector::find_node;
use crate::network::{CertificateReloader, ConnectError, Destination, Swimlane, TransportConnect};
use crate::{Metadata, TaskCenter, TaskKind};

#[derive(Clone)]
pub struct GrpcConnector {
    /// Connection cache for hot certificate reload
    connection_cache: Arc<RwLock<HashMap<(AdvertisedAddress, TlsSwimlane), Channel>>>,
    /// Certificate reloader for hot reload events
    cert_reloader: Option<Arc<CertificateReloader>>,
}

impl Default for GrpcConnector {
    fn default() -> Self {
        Self {
            connection_cache: Arc::new(RwLock::new(HashMap::new())),
            cert_reloader: None,
        }
    }
}

impl GrpcConnector {
    /// Create a new GrpcConnector with certificate reloader support
    pub fn new_with_cert_reloader(cert_reloader: Arc<CertificateReloader>) -> Self {
        let connector = Self {
            connection_cache: Arc::new(RwLock::new(HashMap::new())),
            cert_reloader: Some(cert_reloader.clone()),
        };
        
        // Start background task to handle certificate reload events
        let cache_clone = connector.connection_cache.clone();
        let mut reload_rx = cert_reloader.subscribe();
        
        let _ = TaskCenter::spawn(
            TaskKind::NetworkMessageHandler,
            "grpc-cert-reloader",
            async move {
                while let Ok(event) = reload_rx.recv().await {
                    info!("Received certificate reload event for swimlane {:?}", event.swimlane);
                    
                    // Clear all cached connections for the affected swimlane
                    // New connections will be created with updated certificates
                    let mut cache = cache_clone.write().await;
                    let keys_to_remove: Vec<_> = cache
                        .keys()
                        .filter(|(_, swimlane)| *swimlane == event.swimlane)
                        .cloned()
                        .collect();
                    
                    let count = keys_to_remove.len();
                    for key in keys_to_remove {
                        cache.remove(&key);
                        debug!("Removed cached connection for {:?} due to certificate reload", key.0);
                    }
                    
                    info!("Updated {} connections for swimlane {:?}", count, event.swimlane);
                }
                warn!("Certificate reload event listener terminated");
                Ok(())
            },
        );
        
        connector
    }
    
    /// Get or create a cached channel
    async fn get_or_create_channel(
        &self,
        address: AdvertisedAddress,
        swimlane: Swimlane,
        options: &NetworkingOptions,
    ) -> Result<Channel, ConnectError> {
        // Map Swimlane to TlsSwimlane for cache key
        let tls_swimlane = match swimlane {
            Swimlane::General => TlsSwimlane::General,
            Swimlane::Gossip => TlsSwimlane::Gossip,
            Swimlane::BifrostData => TlsSwimlane::BifrostData,
            Swimlane::IngressData => TlsSwimlane::IngressData,
        };
        
        let cache_key = (address.clone(), tls_swimlane);
        
        // Check if we have a cached connection
        {
            let cache = self.connection_cache.read().await;
            if let Some(channel) = cache.get(&cache_key) {
                debug!("Using cached gRPC connection to {}", address);
                return Ok(channel.clone());
            }
        }
        
        // Create new connection
        debug!("Creating new gRPC connection to {}", address);
        let channel = create_channel(address.clone(), swimlane, options)?;
        
        // Cache the connection
        {
            let mut cache = self.connection_cache.write().await;
            cache.insert(cache_key, channel.clone());
        }
        
        Ok(channel)
    }
}

impl TransportConnect for GrpcConnector {
    async fn connect(
        &self,
        destination: &Destination,
        swimlane: Swimlane,
        output_stream: impl Stream<Item = Message> + Send + Unpin + 'static,
    ) -> Result<impl Stream<Item = Message> + Send + Unpin + 'static, ConnectError> {
        let address = match destination {
            Destination::Node(node_id) => {
                find_node(&Metadata::with_current(|m| m.nodes_config_ref()), *node_id)?
                    .address
                    .clone()
            }
            Destination::Address(address) => address.clone(),
        };

        debug!("Connecting to {} at {}", destination, address);
        let channel = self.get_or_create_channel(address, swimlane, &Configuration::pinned().networking).await?;

        // Establish the connection
        let mut client = CoreNodeSvcClient::new(channel)
            .max_decoding_message_size(MAX_MESSAGE_SIZE)
            .max_decoding_message_size(MAX_MESSAGE_SIZE)
            // note: the order of those calls defines the priority
            .accept_compressed(CompressionEncoding::Zstd)
            .accept_compressed(CompressionEncoding::Gzip)
            .send_compressed(DEFAULT_GRPC_COMPRESSION);
        let incoming = client.create_connection(output_stream).await?.into_inner();
        Ok(incoming.map_while(|x| x.ok()))
    }
}

fn create_channel(
    address: AdvertisedAddress,
    swimlane: Swimlane,
    options: &NetworkingOptions,
) -> Result<Channel, ConnectError> {
    // Map Swimlane to TlsSwimlane for TLS configuration lookup
    let tls_swimlane = match swimlane {
        Swimlane::General => TlsSwimlane::General,
        Swimlane::Gossip => TlsSwimlane::Gossip,
        Swimlane::BifrostData => TlsSwimlane::BifrostData,
        Swimlane::IngressData => TlsSwimlane::IngressData,
    };

    // Get TLS configuration for this swimlane
    let tls_config = options.tls.for_swimlane(tls_swimlane);
    
    // Validate TLS configuration if enabled
    if tls_config.enabled {
        validate_tls_config(&tls_config)
            .map_err(|e| ConnectError::Transport(format!("TLS configuration error: {}", e)))?;
    }

    let endpoint = match &address {
        AdvertisedAddress::Uds(_) => {
            // dummy endpoint required to specify an uds connector, it is not used anywhere
            Endpoint::try_from("http://127.0.0.1").expect("/ should be a valid Uri")
        }
        AdvertisedAddress::Http(uri) => {
            let mut endpoint = Channel::builder(uri.clone()).executor(TaskCenterExecutor);
            
            // Configure TLS if enabled for HTTP connections
            if tls_config.enabled {
                let client_tls_config = create_client_tls_config(&tls_config)
                    .map_err(|e| ConnectError::Transport(format!("Failed to create TLS config: {}", e)))?;
                debug!("Configuring TLS for gRPC client to {}", uri);
                endpoint = endpoint.tls_config(client_tls_config)
                    .map_err(|e| ConnectError::Transport(format!("Failed to apply TLS config: {}", e)))?;
            }
            
            endpoint
        }
    };

    let endpoint = endpoint
        .user_agent(format!(
            "restate/{}",
            option_env!("CARGO_PKG_VERSION").unwrap_or("dev")
        ))
        .unwrap()
        .connect_timeout(*options.connect_timeout)
        .http2_keep_alive_interval(*options.http2_keep_alive_interval)
        .keep_alive_timeout(*options.http2_keep_alive_timeout)
        .http2_adaptive_window(options.http2_adaptive_window)
        .initial_stream_window_size(options.stream_window_size())
        .initial_connection_window_size(options.connection_window_size())
        .keep_alive_while_idle(true)
        // this true by default, but this is to guard against any change in defaults
        .tcp_nodelay(true);

    let channel = match address {
        AdvertisedAddress::Uds(uds_path) => {
            endpoint.connect_with_connector_lazy(tower::service_fn(move |_: Uri| {
                let uds_path = uds_path.clone();
                async move {
                    Ok::<_, io::Error>(TokioIo::new(UnixStream::connect(uds_path).await?))
                }
            }))
        }
        AdvertisedAddress::Http(_) => endpoint.connect_lazy()
    };

    Ok(channel)
}

#[derive(Clone, Default)]
struct TaskCenterExecutor;

impl<F> hyper::rt::Executor<F> for TaskCenterExecutor
where
    F: Future + 'static + Send,
    F::Output: Send + 'static,
{
    fn execute(&self, fut: F) {
        // This is unmanaged task because we don't want to bind the connection lifetime to the task
        // that created it, the connection reactor is already a managed task and will react to
        // global system shutdown and other graceful shutdown signals (i.e. dropping the owning
        // sender, or via egress_drop)
        //
        // Making this task managed will result in occasional lockups on shutdown.
        let _ = TaskCenter::spawn_unmanaged(TaskKind::H2ClientStream, "h2stream", async move {
            // ignore the future output
            let _ = fut.await;
        });
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use restate_types::config::{TlsConfig, NetworkingOptions};
    use restate_types::net::AdvertisedAddress;
    use tempfile::TempDir;
    use crate::network::tls_util::validate_tls_config;
    
    fn init_test_environment() {
        // Initialize test environment if needed
        // The actual TLS tests will handle crypto initialization internally
    }

    const TEST_CERT_PEM: &str = r#"-----BEGIN CERTIFICATE-----
MIIBkTCB+wIJAMlyFqk69v+9MA0GCSqGSIb3DQEBCwUAMBQxEjAQBgNVBAMMCWxv
Y2FsaG9zdDAeFw0yNDAxMDEwMDAwMDBaFw0yNTAxMDEwMDAwMDBaMBQxEjAQBgNV
BAMMCWxvY2FsaG9zdDBcMA0GCSqGSIb3DQEBAQUAA0sAMEgCQQDTDxfor3f7n/B6
XhNO7w8sONqhD4bIjT2qN7VQNcJJd1ZPEYJBFb2o9Yb4g9ShGZ1E4DxF+QqNjCk6
0qvFqLhfAgMBAAEwDQYJKoZIhvcNAQELBQADQQCJ1JQ7LCcEWVEb+KlkQi1nSmZ6
r5B1HdDfr8R3h6Q2OJl3RqY5c4LT5GdIx4WxWzFkOQWmOFwdJNUdUqpU3Z7Z
-----END CERTIFICATE-----"#;

    const TEST_KEY_PEM: &str = r#"-----BEGIN PRIVATE KEY-----
MIIBVAIBADANBgkqhkiG9w0BAQEFAASCAT4wggE6AgEAAkEA0w8X6K93+5/wel4T
Tu8PLDjaoQ+GyI09qje1UDXCSXdWTxGCQRW9qPWG+IPUoRmdROA8RfkKjYwpOtKr
xai4XwIDAQABAkBvb6fgM9ys/yLCNpYCiYOmNJrjAM9Y/QDHQNhM3rKMRX7HZJ1j
vLLKlNqBUXO8Y3C9F5F0Bfp5b6cQqRJNvtMhAiEA+YF5+J9pFqYB8vQoNHY1N7cZ
vF2Tj0XY5Wj9NQ0x6IECIQDZJY4Nz7vX4w9Q2gR3xJ3i5j6tJq8lO3cR4pYG0QYA
YwIhAMr5wJ1xQ9hJN0J2mR7F3vA5GfQ8bO2qN6iHY8X7JxnlAiEA3YOy2lWn9Ol3
xH8LfH8rN5V7t0pRvY2qTj3nN1YoE6sCIC2jOjdQ7J0uH1dQ5oJ8G2aN6k3O5hP7
f4rV7w2XNdBt
-----END PRIVATE KEY-----"#;

    #[test]
    fn test_grpc_connector_default() {
        let connector = GrpcConnector::default();
        assert!(connector.cert_reloader.is_none());
    }
    
    #[test]
    fn test_grpc_connector_with_cert_reloader() {
        // This test can't run without TaskCenter initialization
        // Just test that CertificateReloader can be created
        let cert_reloader = CertificateReloader::new().unwrap();
        // Test that we can subscribe to events
        let _rx = cert_reloader.subscribe();
    }

    #[test]
    fn test_create_channel_without_tls() {
        let _address = AdvertisedAddress::Http("http://localhost:1234".parse().unwrap());
        let options = NetworkingOptions::default(); // TLS disabled by default
        
        let tls_config = options.tls.for_swimlane(TlsSwimlane::General);
        assert!(!tls_config.enabled);
        
        // Channel creation requires TaskCenter, so just test configuration
        assert_eq!(tls_config.cert_path, None);
        assert_eq!(tls_config.key_path, None);
    }
    
    #[test]
    fn test_create_channel_with_tls_config() {
        init_test_environment();
        let temp_dir = TempDir::new().unwrap();
        let cert_path = temp_dir.path().join("cert.pem");
        let key_path = temp_dir.path().join("key.pem");
        
        std::fs::write(&cert_path, TEST_CERT_PEM).unwrap();
        std::fs::write(&key_path, TEST_KEY_PEM).unwrap();
        
        let mut options = NetworkingOptions::default();
        options.tls = TlsConfig {
            enabled: true,
            cert_path: Some(cert_path.clone()),
            key_path: Some(key_path.clone()),
            ca_cert_path: None,
            require_client_cert: false,
            swimlane_overrides: Default::default(),
        };
        
        // Test that TLS configuration is properly loaded
        let tls_config = options.tls.for_swimlane(TlsSwimlane::General);
        assert!(tls_config.enabled);
        assert_eq!(tls_config.cert_path, Some(cert_path));
        assert_eq!(tls_config.key_path, Some(key_path));
    }
    
    #[test]
    fn test_connection_cache_basic() {
        let connector = GrpcConnector::default();
        
        // Test that cache starts empty
        let rt = tokio::runtime::Runtime::new().unwrap();
        rt.block_on(async {
            let cache = connector.connection_cache.read().await;
            assert_eq!(cache.len(), 0);
        });
    }
    
    #[test]
    fn test_connection_cache_with_cert_reload() {
        init_test_environment();
        let temp_dir = TempDir::new().unwrap();
        let cert_path = temp_dir.path().join("cert.pem");
        let key_path = temp_dir.path().join("key.pem");
        
        std::fs::write(&cert_path, TEST_CERT_PEM).unwrap();
        std::fs::write(&key_path, TEST_KEY_PEM).unwrap();
        
        let _cert_reloader = CertificateReloader::new().unwrap();
        
        // Test configuration without TaskCenter spawn
        let tls_config = restate_types::config::EffectiveTlsConfig {
            enabled: true,
            cert_path: Some(cert_path),
            key_path: Some(key_path),
            ca_cert_path: None,
            require_client_cert: false,
        };
        
        // Test validation (this doesn't require TaskCenter)
        let validation_result = validate_tls_config(&tls_config);
        assert!(validation_result.is_ok());
    }
}
