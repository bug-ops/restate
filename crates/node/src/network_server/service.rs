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

use restate_core::TaskCenter;
use restate_core::network::grpc::CoreNodeSvcHandler;
use restate_core::network::{ConnectionManager, NetworkServerBuilder};
use restate_core::{Identification, MetadataWriter};
use restate_tracing_instrumentation::prometheus_metrics::Prometheus;
use restate_types::config::{CommonOptions, Configuration};

use super::grpc_svc_handler::{MetadataProxySvcHandler, NodeCtlSvcHandler};
use super::pprof;
use crate::network_server::metrics::render_metrics;
use crate::network_server::state::NodeCtrlHandlerStateBuilder;

pub struct NetworkServer {}

impl NetworkServer {
    pub async fn run(
        connection_manager: ConnectionManager,
        server_builder: NetworkServerBuilder,
        options: CommonOptions,
        metadata_writer: MetadataWriter,
        prometheus: Prometheus,
    ) -> Result<(), anyhow::Error> {
        let networking_config = &Configuration::pinned().networking;
        
        if networking_config.enable_separate_internal_server {
            Self::run_separate_servers(
                connection_manager,
                server_builder,
                options,
                metadata_writer,
                prometheus,
            ).await
        } else {
            Self::run_single_server(
                connection_manager,
                server_builder,
                options,
                metadata_writer,
                prometheus,
            ).await
        }
    }

    async fn run_single_server(
        connection_manager: ConnectionManager,
        mut server_builder: NetworkServerBuilder,
        options: CommonOptions,
        metadata_writer: MetadataWriter,
        prometheus: Prometheus,
    ) -> Result<(), anyhow::Error> {
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

        server_builder.register_grpc_service(
            MetadataProxySvcHandler::new(metadata_writer.raw_metadata_store_client().clone())
                .into_server(),
            restate_metadata_store::protobuf::metadata_proxy_svc::FILE_DESCRIPTOR_SET,
        );

        server_builder.register_grpc_service(
            NodeCtlSvcHandler::new(metadata_writer).into_server(),
            restate_core::protobuf::node_ctl_svc::FILE_DESCRIPTOR_SET,
        );

        server_builder.register_grpc_service(
            CoreNodeSvcHandler::new(connection_manager).into_server(),
            restate_core::network::protobuf::core_node_svc::FILE_DESCRIPTOR_SET,
        );

        server_builder
            .run(node_rpc_health, &options.bind_address.unwrap())
            .await?;

        Ok(())
    }

    async fn run_separate_servers(
        connection_manager: ConnectionManager,
        _server_builder: NetworkServerBuilder,
        options: CommonOptions,
        metadata_writer: MetadataWriter,
        prometheus: Prometheus,
    ) -> Result<(), anyhow::Error> {
        // Create admin server builder (external services)
        let mut admin_builder = NetworkServerBuilder::default();
        
        // Create internal server builder (internal services)
        let mut internal_builder = NetworkServerBuilder::default();

        // Configure shared state
        let mut state_builder = NodeCtrlHandlerStateBuilder::default();
        state_builder.task_center(TaskCenter::current());
        state_builder.prometheus_handle(prometheus.into());
        let shared_state = state_builder.build().expect("should be infallible");

        let post_or_put = MethodFilter::POST.or(MethodFilter::PUT);

        // Admin server gets HTTP endpoints
        let axum_router = axum::Router::new()
            .route("/health", get(report_health))
            .route("/metrics", get(render_metrics))
            .route("/debug/pprof/heap", get(pprof::heap))
            .route(
                "/debug/pprof/heap/activate",
                on(post_or_put.clone(), pprof::activate_heap),
            )
            .route(
                "/debug/pprof/heap/deactivate",
                on(post_or_put, pprof::deactivate_heap),
            )
            .with_state(shared_state);

        admin_builder.register_axum_routes(axum_router);

        // Admin server gets NodeCtlSvc (for restatectl)
        admin_builder.register_grpc_service(
            NodeCtlSvcHandler::new(metadata_writer.clone()).into_server(),
            restate_core::protobuf::node_ctl_svc::FILE_DESCRIPTOR_SET,
        );

        // Internal server gets internal communication services
        internal_builder.register_grpc_service(
            MetadataProxySvcHandler::new(metadata_writer.raw_metadata_store_client().clone())
                .into_server(),
            restate_metadata_store::protobuf::metadata_proxy_svc::FILE_DESCRIPTOR_SET,
        );

        internal_builder.register_grpc_service(
            CoreNodeSvcHandler::new(connection_manager).into_server(),
            restate_core::network::protobuf::core_node_svc::FILE_DESCRIPTOR_SET,
        );

        let node_rpc_health = TaskCenter::with_current(|tc| tc.health().node_rpc_status());
        let admin_bind_address = options.bind_address.as_ref().unwrap();
        let internal_bind_address = Self::get_internal_bind_address(&options)?;

        NetworkServerBuilder::run_separate_servers(
            admin_builder,
            internal_builder,
            node_rpc_health.clone(),
            node_rpc_health,
            admin_bind_address,
            &internal_bind_address,
        ).await
    }

    fn get_internal_bind_address(options: &CommonOptions) -> Result<restate_types::net::BindAddress, anyhow::Error> {
        if let Some(internal_addr) = &options.internal_bind_address {
            return Ok(internal_addr.clone());
        }

        // Generate internal address from main bind address
        let main_bind = options.bind_address.as_ref().unwrap();
        let internal_port = match main_bind {
            restate_types::net::BindAddress::Socket(addr) => addr.port() + 1,
            restate_types::net::BindAddress::Uds(_) => {
                return Err(anyhow::anyhow!("Internal bind address must be specified when using UDS"));
            }
        };

        let internal_addr = match main_bind {
            restate_types::net::BindAddress::Socket(addr) => {
                restate_types::net::BindAddress::Socket(
                    std::net::SocketAddr::new(addr.ip(), internal_port)
                )
            }
            restate_types::net::BindAddress::Uds(_) => unreachable!(),
        };

        Ok(internal_addr)
    }
}

pub async fn report_health() -> Json<Identification> {
    Json(Identification::get())
}
