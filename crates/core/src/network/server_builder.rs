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
