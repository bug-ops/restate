// Copyright (c) 2023 - 2025 Restate Software, Inc., Restate GmbH.
// All rights reserved.
//
// Use of this software is governed by the Business Source License
// included in the LICENSE file.
//
// As of the Change Date specified in that file, in accordance with
// the Business Source License, use of this software will be governed
// by the Apache License, Version 2.0.

use axum::Json;
use axum::routing::{MethodFilter, get, on};
use futures::future::try_join;

use restate_core::TaskCenter;
use restate_core::network::grpc::CoreNodeSvcHandler;
use restate_core::network::{ConnectionManager, NetworkServerBuilder};
use restate_core::{Identification, MetadataWriter};
use restate_tracing_instrumentation::prometheus_metrics::Prometheus;
use restate_types::config::Configuration;

use super::grpc_svc_handler::{MetadataProxySvcHandler, NodeCtlSvcHandler};
use super::internal_grpc_svc_handler::InternalNodeCtlSvcHandler;
use super::pprof;
use crate::network_server::metrics::render_metrics;
use crate::network_server::state::NodeCtrlHandlerStateBuilder;

pub struct NetworkServer {}

impl NetworkServer {
    pub async fn run(
        connection_manager: ConnectionManager,
        server_builder: NetworkServerBuilder,
        metadata_writer: MetadataWriter,
        prometheus: Prometheus,
    ) -> Result<(), anyhow::Error> {
        let config = Configuration::pinned();
        
        // Check if we should run in split mode
        if let Some(internal_bind_address) = &config.common.internal_bind_address {
            // Split mode: run external and internal servers
            Self::run_split_mode(
                connection_manager,
                server_builder,
                metadata_writer,
                prometheus,
                internal_bind_address.clone(),
            )
            .await
        } else {
            // Unified mode: run single server (backward compatibility)
            Self::run_unified_mode(
                connection_manager,
                server_builder,
                metadata_writer,
                prometheus,
            )
            .await
        }
    }

    /// Run in unified mode with a single server (backward compatibility)
    async fn run_unified_mode(
        connection_manager: ConnectionManager,
        mut server_builder: NetworkServerBuilder,
        metadata_writer: MetadataWriter,
        prometheus: Prometheus,
    ) -> Result<(), anyhow::Error> {
        let config = Configuration::pinned();
        
        // Configure Metric Exporter
        let mut state_builder = NodeCtrlHandlerStateBuilder::default();
        state_builder.task_center(TaskCenter::current());
        state_builder.prometheus_handle(prometheus.into());
        let shared_state = state_builder.build().expect("should be infallible");

        let post_or_put = MethodFilter::POST.or(MethodFilter::PUT);

        // -- HTTP service (for prometheus et al.)
        let axum_router = axum::Router::new()
            .route("/health", get(report_health))
            .route("/metrics", get(render_metrics))
            .route("/debug/pprof/heap", get(pprof::heap))
            .route(
                "/debug/pprof/heap/activate",
                on(post_or_put, pprof::activate_heap),
            )
            .route(
                "/debug/pprof/heap/deactivate",
                on(post_or_put, pprof::deactivate_heap),
            )
            .with_state(shared_state);

        server_builder.register_axum_routes(axum_router);

        let node_rpc_health = TaskCenter::with_current(|tc| tc.health().node_rpc_status());

        // Register all services on the single server
        server_builder.register_grpc_service(
            MetadataProxySvcHandler::new(metadata_writer.raw_metadata_store_client().clone())
                .into_server(&config.networking),
            restate_metadata_store::protobuf::metadata_proxy_svc::FILE_DESCRIPTOR_SET,
        );

        server_builder.register_grpc_service(
            NodeCtlSvcHandler::new()
                .into_server(&config.networking),
            restate_core::protobuf::node_ctl_svc::FILE_DESCRIPTOR_SET,
        );

        // In unified mode, we also register the internal service with ProvisionCluster
        server_builder.register_grpc_service(
            InternalNodeCtlSvcHandler::new(metadata_writer)
                .into_server(&config.networking),
            restate_core::protobuf::internal_node_ctl_svc::FILE_DESCRIPTOR_SET,
        );

        server_builder.register_grpc_service(
            CoreNodeSvcHandler::new(connection_manager)
                .into_server(&config.networking),
            restate_core::network::protobuf::core_node_svc::FILE_DESCRIPTOR_SET,
        );

        server_builder
            .run(
                node_rpc_health,
                config
                    .common
                    .bind_address
                    .as_ref()
                    .unwrap(),
            )
            .await?;

        Ok(())
    }

    /// Run in split mode with separate external and internal servers
    async fn run_split_mode(
        connection_manager: ConnectionManager,
        external_server_builder: NetworkServerBuilder,
        metadata_writer: MetadataWriter,
        prometheus: Prometheus,
        internal_bind_address: restate_types::net::BindAddress,
    ) -> Result<(), anyhow::Error> {
        let _config = Configuration::pinned();
        
        // Create a new builder for the internal server
        let internal_server_builder = NetworkServerBuilder::default();
        
        // Clone necessary components
        let metadata_writer_internal = metadata_writer.clone();
        let connection_manager_internal = connection_manager.clone();
        
        // Start external server
        let external_future = Self::start_external_server(
            external_server_builder,
            metadata_writer,
            prometheus,
        );

        // Start internal server
        let internal_future = Self::start_internal_server(
            internal_server_builder,
            connection_manager_internal,
            metadata_writer_internal,
            internal_bind_address,
        );

        // Run both servers concurrently
        try_join(external_future, internal_future).await?;

        Ok(())
    }

    /// Start the external server (port 5122) with public services
    async fn start_external_server(
        mut server_builder: NetworkServerBuilder,
        _metadata_writer: MetadataWriter,
        prometheus: Prometheus,
    ) -> Result<(), anyhow::Error> {
        let config = Configuration::pinned();
        
        // Configure Metric Exporter
        let mut state_builder = NodeCtrlHandlerStateBuilder::default();
        state_builder.task_center(TaskCenter::current());
        state_builder.prometheus_handle(prometheus.into());
        let shared_state = state_builder.build().expect("should be infallible");

        let post_or_put = MethodFilter::POST.or(MethodFilter::PUT);

        // -- HTTP service (for prometheus et al.)
        let axum_router = axum::Router::new()
            .route("/health", get(report_health))
            .route("/metrics", get(render_metrics))
            .route("/debug/pprof/heap", get(pprof::heap))
            .route(
                "/debug/pprof/heap/activate",
                on(post_or_put, pprof::activate_heap),
            )
            .route(
                "/debug/pprof/heap/deactivate",
                on(post_or_put, pprof::deactivate_heap),
            )
            .with_state(shared_state);

        server_builder.register_axum_routes(axum_router);

        let node_rpc_health = TaskCenter::with_current(|tc| tc.health().node_rpc_status());

        // Register only external services
        server_builder.register_grpc_service(
            NodeCtlSvcHandler::new()
                .into_server(&config.networking),
            restate_core::protobuf::node_ctl_svc::FILE_DESCRIPTOR_SET,
        );

        // Note: ClusterCtrlSvc would be registered here when available

        server_builder
            .run(
                node_rpc_health,
                config
                    .common
                    .bind_address
                    .as_ref()
                    .unwrap(),
            )
            .await?;

        Ok(())
    }

    /// Start the internal server (port 5123) with internal services
    async fn start_internal_server(
        mut server_builder: NetworkServerBuilder,
        connection_manager: ConnectionManager,
        metadata_writer: MetadataWriter,
        bind_address: restate_types::net::BindAddress,
    ) -> Result<(), anyhow::Error> {
        let config = Configuration::pinned();
        
        let node_rpc_health = TaskCenter::with_current(|tc| tc.health().node_rpc_status());

        // Register internal services
        server_builder.register_grpc_service(
            CoreNodeSvcHandler::new(connection_manager)
                .into_server(&config.networking),
            restate_core::network::protobuf::core_node_svc::FILE_DESCRIPTOR_SET,
        );

        server_builder.register_grpc_service(
            MetadataProxySvcHandler::new(metadata_writer.raw_metadata_store_client().clone())
                .into_server(&config.networking),
            restate_metadata_store::protobuf::metadata_proxy_svc::FILE_DESCRIPTOR_SET,
        );

        server_builder.register_grpc_service(
            InternalNodeCtlSvcHandler::new(metadata_writer)
                .into_server(&config.networking),
            restate_core::protobuf::internal_node_ctl_svc::FILE_DESCRIPTOR_SET,
        );

        server_builder
            .run(node_rpc_health, &bind_address)
            .await?;

        Ok(())
    }
}

pub async fn report_health() -> Json<Identification> {
    Json(Identification::get())
}