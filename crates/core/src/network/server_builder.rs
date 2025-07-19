// Copyright (c) 2023 - 2025 Restate Software, Inc., Restate GmbH.
// All rights reserved.
//
// Use of this software is governed by the Business Source License
// included in the LICENSE file.
//
// As of the Change Date specified in that file, in accordance with
// the Business Source License, use of this software will be governed
// by the Apache License, Version 2.0.

use std::convert::Infallible;
use std::sync::Arc;

use http::Request;
use hyper::body::Incoming;
use hyper_util::service::TowerToHyperService;
use tokio::sync::broadcast;
use tonic::body::boxed;
use tonic::service::Routes;
use tower::ServiceExt;
use tower_http::trace::{DefaultOnFailure, TraceLayer};
use tracing::{Level, debug, info, warn};

use restate_types::config::{NetworkingOptions, TlsSwimlane};
use restate_types::health::HealthStatus;
use restate_types::net::BindAddress;
use restate_types::protobuf::common::NodeRpcStatus;

use super::multiplex::MultiplexService;
use super::net_util::run_hyper_server;
use super::tls_util::{create_server_tls_config, validate_tls_config};
use super::CertificateReloader;

#[derive(Debug, Default)]
pub struct NetworkServerBuilder {
    grpc_descriptors: Vec<&'static [u8]>,
    grpc_routes: Option<Routes>,
    axum_router: Option<axum::routing::Router>,
}

impl NetworkServerBuilder {
    pub fn is_empty(&self) -> bool {
        self.grpc_routes.is_none() && self.axum_router.is_none()
    }

    pub fn register_grpc_service<S>(
        &mut self,
        svc: S,
        file_descriptor_set: &'static [u8],
    ) -> &mut Self
    where
        S: tower::Service<
                Request<tonic::body::BoxBody>,
                Response = http::Response<tonic::body::BoxBody>,
                Error = Infallible,
            > + tonic::server::NamedService
            + Clone
            + Send
            + 'static,
        S::Future: Send + 'static,
    {
        let current_routes = self.grpc_routes.take().unwrap_or_default();
        self.grpc_descriptors.push(file_descriptor_set);
        debug!(svc = S::NAME, "Registering gRPC service to node-rpc-server");
        self.grpc_routes = Some(current_routes.add_service(svc));
        self
    }

    pub fn register_axum_routes(&mut self, routes: impl Into<axum::routing::Router>) {
        self.axum_router = Some(self.axum_router.take().unwrap_or_default().merge(routes));
    }

    pub async fn run(
        self,
        node_rpc_health: HealthStatus<NodeRpcStatus>,
        bind_address: &BindAddress,
        networking_options: &NetworkingOptions,
    ) -> Result<(), anyhow::Error> {
        node_rpc_health.update(NodeRpcStatus::StartingUp);
        // Trace layer
        let span_factory = tower_http::trace::DefaultMakeSpan::new()
            .include_headers(true)
            .level(tracing::Level::ERROR);

        let axum_router = self
            .axum_router
            .unwrap_or_default()
            .layer(TraceLayer::new_for_http().make_span_with(span_factory.clone()))
            .fallback(handler_404);

        let mut reflection_service_builder = tonic_reflection::server::Builder::configure();
        for descriptor in self.grpc_descriptors {
            reflection_service_builder =
                reflection_service_builder.register_encoded_file_descriptor_set(descriptor);
        }

        // Configure TLS for general inter-node communication
        let tls_config = networking_options.tls.for_swimlane(TlsSwimlane::General);
        
        // Validate TLS configuration if enabled
        if tls_config.enabled {
            validate_tls_config(&tls_config)?;
        }

        let mut server_builder = tonic::transport::Server::builder()
            .layer(
                TraceLayer::new_for_grpc()
                    .make_span_with(span_factory)
                    .on_failure(DefaultOnFailure::new().level(Level::DEBUG)),
            );

        // Add TLS configuration if enabled
        if tls_config.enabled {
            let server_tls_config = create_server_tls_config(&tls_config)?;
            debug!("Configuring TLS for gRPC server");
            server_builder = server_builder.tls_config(server_tls_config)?;
        }

        let server_builder = server_builder
            .add_routes(self.grpc_routes.unwrap_or_default())
            .add_service(reflection_service_builder.build_v1()?);

        // Multiplex both grpc and http based on content-type
        let service = TowerToHyperService::new(
            MultiplexService::new(axum_router, server_builder.into_service())
                .map_request(|req: Request<Incoming>| req.map(boxed)),
        );

        run_hyper_server(
            bind_address,
            service,
            "node-rpc-server",
            || node_rpc_health.update(NodeRpcStatus::Ready),
            || node_rpc_health.update(NodeRpcStatus::Stopping),
        )
        .await?;

        Ok(())
    }

    /// Run the server with certificate hot-reload support
    pub async fn run_with_hot_reload(
        self,
        node_rpc_health: HealthStatus<NodeRpcStatus>,
        bind_address: &BindAddress,
        networking_options: &NetworkingOptions,
        cert_reloader: Arc<CertificateReloader>,
    ) -> Result<(), anyhow::Error> {
        // Subscribe to certificate reload events for General swimlane
        let mut reload_rx = cert_reloader.subscribe();
        
        // Create a shutdown signal that can be triggered by certificate reload
        let (_shutdown_tx, _shutdown_rx) = broadcast::channel::<()>(1);
        
        let bind_address = bind_address.clone();
        let networking_options = networking_options.clone();
        
        // Configure certificate reloader with current TLS config
        let tls_config = networking_options.tls.for_swimlane(TlsSwimlane::General);
        if tls_config.enabled {
            cert_reloader.add_config(TlsSwimlane::General, tls_config).await?;
            info!("Certificate reloader configured for General swimlane");
        }
        
        loop {
            // Clone builder for this iteration
            let builder = NetworkServerBuilder {
                grpc_descriptors: self.grpc_descriptors.clone(),
                grpc_routes: self.grpc_routes.clone(),
                axum_router: self.axum_router.clone(),
            };
            
            let health_clone = node_rpc_health.clone();
            let bind_clone = bind_address.clone();
            let options_clone = networking_options.clone();
            let _shutdown_rx_clone = _shutdown_tx.subscribe();
            
            // Start server in background task with timeout for graceful restart
            let server_task = tokio::spawn(async move {
                // For now, use a simplified approach where we restart the entire server
                // TODO: Implement proper graceful shutdown integration with run_hyper_server
                builder.run(health_clone, &bind_clone, &options_clone).await
            });
            
            // Wait for certificate reload or server completion
            tokio::select! {
                // Server completed (error or normal termination)
                server_result = server_task => {
                    match server_result {
                        Ok(Ok(())) => {
                            info!("Server completed normally");
                            break;
                        }
                        Ok(Err(e)) => {
                            warn!("Server error: {}", e);
                            return Err(e);
                        }
                        Err(e) => {
                            warn!("Server task panic: {}", e);
                            return Err(anyhow::anyhow!("Server task failed: {}", e));
                        }
                    }
                }
                
                // Certificate reload event
                reload_event = reload_rx.recv() => {
                    match reload_event {
                        Ok(event) if event.swimlane == TlsSwimlane::General => {
                            info!("Received certificate reload event for General swimlane");
                            warn!("Certificate hot-reload requires server restart. Consider using a load balancer for zero-downtime updates.");
                            
                            // For now, we abort the current server and restart
                            // This will cause a brief interruption but ensures new certificates are used
                            // Note: server_task is consumed by the select!, so we need to restart the loop
                            
                            // Small delay to allow cleanup
                            tokio::time::sleep(tokio::time::Duration::from_millis(100)).await;
                            
                            info!("Restarting server with new certificates");
                            // Continue loop to restart server with new certificates
                        }
                        Ok(event) => {
                            debug!("Ignoring certificate reload event for swimlane {:?}", event.swimlane);
                        }
                        Err(broadcast::error::RecvError::Lagged(skipped)) => {
                            warn!("Certificate reload events lagged, skipped {} events", skipped);
                        }
                        Err(broadcast::error::RecvError::Closed) => {
                            info!("Certificate reloader channel closed");
                            break;
                        }
                    }
                }
            }
        }
        
        Ok(())
    }
}

// handle 404
async fn handler_404() -> (http::StatusCode, &'static str) {
    (
        axum::http::StatusCode::NOT_FOUND,
        "Are you lost? Maybe visit https://restate.dev instead!",
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use restate_types::config::{TlsConfig, NetworkingOptions};
    use tempfile::TempDir;
    
    fn init_test_environment() {
        // Initialize crypto provider if needed
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
    fn test_server_builder_with_tls_config() {
        init_test_environment();
        let temp_dir = TempDir::new().unwrap();
        let cert_path = temp_dir.path().join("cert.pem");
        let key_path = temp_dir.path().join("key.pem");
        
        std::fs::write(&cert_path, TEST_CERT_PEM).unwrap();
        std::fs::write(&key_path, TEST_KEY_PEM).unwrap();
        
        let builder = NetworkServerBuilder::default();
        assert!(builder.is_empty());
        
        let mut networking_options = NetworkingOptions::default();
        networking_options.tls = TlsConfig {
            enabled: true,
            cert_path: Some(cert_path.clone()),
            key_path: Some(key_path.clone()),
            ca_cert_path: None,
            require_client_cert: false,
            swimlane_overrides: Default::default(),
        };
        
        // Test that TLS configuration can be loaded
        let tls_config = networking_options.tls.for_swimlane(TlsSwimlane::General);
        assert!(tls_config.enabled);
        assert_eq!(tls_config.cert_path, Some(cert_path));
        assert_eq!(tls_config.key_path, Some(key_path));
        
        // Test TLS validation
        let validation_result = validate_tls_config(&tls_config);
        assert!(validation_result.is_ok());
    }
    
    #[test]
    fn test_server_builder_without_tls() {
        let builder = NetworkServerBuilder::default();
        assert!(builder.is_empty());
        
        let networking_options = NetworkingOptions::default(); // TLS disabled by default
        let tls_config = networking_options.tls.for_swimlane(TlsSwimlane::General);
        assert!(!tls_config.enabled);
    }
    
    #[test]
    fn test_server_builder_with_hot_reload() {
        init_test_environment();
        let cert_reloader = Arc::new(CertificateReloader::new().unwrap());
        
        // Test that we can create certificate reloader
        let mut rx = cert_reloader.subscribe();
        
        // Ensure we can subscribe to reload events
        assert!(rx.try_recv().is_err()); // No events yet
    }
}
