// Copyright (c) 2023 - 2025 Restate Software, Inc., Restate GmbH.
// All rights reserved.
//
// Use of this software is governed by the Business Source License
// included in the LICENSE file.
//
// As of the Change Date specified in that file, in accordance with
// the Business Source License, use of this software will be governed
// by the Apache License, Version 2.0.

use anyhow::Context;
use tonic::codec::CompressionEncoding;
use tonic::{Request, Response, Status};

use restate_core::protobuf::internal_node_ctl_svc::internal_node_ctl_svc_server::{
    InternalNodeCtlSvc, InternalNodeCtlSvcServer,
};
use restate_core::protobuf::internal_node_ctl_svc::{
    ProvisionClusterRequest, ProvisionClusterResponse,
};
use restate_core::MetadataWriter;
use restate_types::config::{Configuration, NetworkingOptions};
use restate_types::logs::metadata::{NodeSetSize, ProviderConfiguration};
use restate_types::protobuf::cluster::ClusterConfiguration as ProtoClusterConfiguration;
use restate_types::replication::ReplicationProperty;

use crate::{ClusterConfiguration, provision_cluster_metadata};

pub struct InternalNodeCtlSvcHandler {
    metadata_writer: MetadataWriter,
}

impl InternalNodeCtlSvcHandler {
    pub fn new(metadata_writer: MetadataWriter) -> Self {
        Self { metadata_writer }
    }

    pub fn into_server(self, config: &NetworkingOptions) -> InternalNodeCtlSvcServer<Self> {
        let server = InternalNodeCtlSvcServer::new(self)
            // note: the order of those calls defines the priority
            .accept_compressed(CompressionEncoding::Zstd)
            .accept_compressed(CompressionEncoding::Gzip);
        if config.disable_compression {
            server
        } else {
            // note: the order of those calls defines the priority
            // deflate/gzip has significantly higher CPU overhead according to our CPU profiling,
            // so we prefer zstd over gzip.
            server
                .send_compressed(CompressionEncoding::Zstd)
                .send_compressed(CompressionEncoding::Gzip)
        }
    }

    fn resolve_cluster_configuration(
        config: &Configuration,
        request: ProvisionClusterRequest,
    ) -> anyhow::Result<ClusterConfiguration> {
        let num_partitions = request
            .num_partitions
            .map(|num_partitions| {
                u16::try_from(num_partitions)
                    .context("Restate only supports running up to 65535 partitions.")
            })
            .transpose()?
            .unwrap_or(config.common.default_num_partitions);

        let partition_replication: ReplicationProperty = request
            .partition_replication
            .map(TryInto::try_into)
            .transpose()?
            .unwrap_or_else(|| config.common.default_replication.clone());
        let log_provider = request
            .log_provider
            .map(|log_provider| log_provider.parse())
            .transpose()?
            .unwrap_or(config.bifrost.default_provider);
        let target_nodeset_size = request
            .target_nodeset_size
            .map(NodeSetSize::try_from)
            .transpose()?
            .unwrap_or(config.bifrost.replicated_loglet.default_nodeset_size);
        let log_replication = request
            .log_replication
            .map(ReplicationProperty::try_from)
            .transpose()?
            .unwrap_or_else(|| config.common.default_replication.clone());

        let provider_configuration =
            ProviderConfiguration::from((log_provider, log_replication, target_nodeset_size));

        Ok(ClusterConfiguration {
            num_partitions,
            partition_replication: partition_replication.into(),
            bifrost_provider: provider_configuration,
        })
    }
}

#[async_trait::async_trait]
impl InternalNodeCtlSvc for InternalNodeCtlSvcHandler {
    async fn provision_cluster(
        &self,
        request: Request<ProvisionClusterRequest>,
    ) -> Result<Response<ProvisionClusterResponse>, Status> {
        let request = request.into_inner();
        let config = Configuration::pinned();

        let dry_run = request.dry_run;
        let cluster_configuration = Self::resolve_cluster_configuration(&config, request)
            .map_err(|err| Status::invalid_argument(err.to_string()))?;

        if dry_run {
            return Ok(Response::new(ProvisionClusterResponse::dry_run(
                ProtoClusterConfiguration::from(cluster_configuration),
            )));
        }

        let newly_provisioned = provision_cluster_metadata(
            &self.metadata_writer,
            &config.common,
            &cluster_configuration,
        )
        .await
        .map_err(|err| Status::internal(err.to_string()))?;

        if !newly_provisioned {
            return Err(Status::already_exists(
                "The cluster has already been provisioned",
            ));
        }

        Ok(Response::new(ProvisionClusterResponse::provisioned(
            ProtoClusterConfiguration::from(cluster_configuration),
        )))
    }
}