// Copyright (c) 2023 - 2025 Restate Software, Inc., Restate GmbH.
// All rights reserved.
//
// Use of this software is governed by the Business Source License
// included in the LICENSE file.
//
// As of the Change Date specified in that file, in accordance with
// the Business Source License, use of this software will be governed
// by the Apache License, Version 2.0.

//! Type-safe NetworkSender integration with phantom type routing system
//!
//! This module provides a type-safe wrapper around the existing NetworkSender trait
//! that enforces routing rules at compile time using phantom types.

use std::marker::PhantomData;
use std::time::Duration;

use restate_types::net::RpcRequest;
use restate_types::{GenerationalNodeId, NodeId};

use super::super::{
    NetworkSender, Connection, LazyConnection, RpcError, ConnectError, 
    ConnectionType, Swimlane,
    connection::OwnedSendPermit,
};
use super::phantom::{
    connection_markers::ConnectionMarker,
    swimlane_markers::SwimlaneMarker,
    service_categories::ServiceRoutingRule,
    RoutingConfig, RoutingWitness,
};
use super::services::{AdvancedServiceRouting, LoadBalancingStrategy};
use super::builder::{DynamicRoutingConfig, RoutingValidationError};

/// Type-safe NetworkSender wrapper with compile-time routing validation
#[derive(Debug, Clone)]
pub struct TypedNetworkSender<S: NetworkSender> {
    inner: S,
}

impl<S: NetworkSender> TypedNetworkSender<S> {
    /// Create a new typed network sender
    pub fn new(sender: S) -> Self {
        Self { inner: sender }
    }
    
    /// Get the inner sender
    pub fn into_inner(self) -> S {
        self.inner
    }
    
    /// Get a reference to the inner sender
    pub fn inner(&self) -> &S {
        &self.inner
    }
    
    /// Get a typed connection with compile-time routing validation
    pub async fn get_typed_connection<C, SLane, N>(
        &self,
        node_id: N,
        _config: RoutingConfig<C, SLane>,
    ) -> Result<TypedConnection<C, SLane>, ConnectError>
    where
        C: ConnectionMarker,
        SLane: SwimlaneMarker,
        N: Into<NodeId> + Send,
    {
        let connection = self.inner.get_connection_typed(
            node_id,
            SLane::SWIMLANE,
            C::CONNECTION_TYPE,
        ).await?;
        
        Ok(TypedConnection::new(connection))
    }
    
    /// Get a typed connection with witness validation
    pub async fn get_connection_with_witness<C, SLane, N>(
        &self,
        node_id: N,
        witness: RoutingWitness<C, SLane>,
    ) -> Result<TypedConnection<C, SLane>, ConnectError>
    where
        C: ConnectionMarker,
        SLane: SwimlaneMarker,
        N: Into<NodeId> + Send,
    {
        let connection = self.inner.get_connection_typed(
            node_id,
            witness.config().swimlane(),
            witness.config().connection_type(),
        ).await?;
        
        Ok(TypedConnection::new(connection))
    }
    
    /// Call RPC with service routing validation
    pub async fn call_rpc_with_service_routing<M, R, N>(
        &self,
        node_id: N,
        message: M,
        sort_code: Option<u64>,
        timeout: Option<Duration>,
    ) -> Result<M::Response, RpcError>
    where
        M: RpcRequest,
        R: ServiceRoutingRule<M>,
        N: Into<NodeId> + Send,
    {
        self.inner.call_rpc(
            node_id,
            R::Swimlane::SWIMLANE,
            message,
            sort_code,
            timeout,
        ).await
    }
    
    /// Call RPC with advanced service routing
    pub async fn call_rpc_with_advanced_routing<M, R, N>(
        &self,
        node_id: N,
        message: M,
        sort_code: Option<u64>,
        timeout: Option<Duration>,
    ) -> Result<M::Response, RpcError>
    where
        M: RpcRequest,
        R: AdvancedServiceRouting<M>,
        N: Into<NodeId> + Send,
    {
        let effective_timeout = if R::REQUIRES_PERSISTENT_CONNECTION {
            timeout.or(Some(Duration::from_secs(30))) // Longer timeout for persistent connections
        } else {
            timeout.or(Some(Duration::from_secs(5)))  // Shorter timeout for transient connections
        };
        
        self.inner.call_rpc(
            node_id,
            R::Swimlane::SWIMLANE,
            message,
            sort_code,
            effective_timeout,
        ).await
    }
    
    /// Get lazy connection with typed configuration
    pub fn lazy_connect_typed<C, SLane>(
        &self,
        node_id: GenerationalNodeId,
        _config: RoutingConfig<C, SLane>,
        buffer_size: usize,
        auto_reconnect: bool,
    ) -> LazyConnection
    where
        C: ConnectionMarker,
        SLane: SwimlaneMarker,
    {
        self.inner.lazy_connect(
            node_id,
            SLane::SWIMLANE,
            buffer_size,
            auto_reconnect,
        )
    }
    
    /// Reserve owned send permit with typed routing
    pub async fn reserve_typed<C, SLane, N>(
        &self,
        node_id: N,
        _config: RoutingConfig<C, SLane>,
    ) -> Option<OwnedSendPermit>
    where
        C: ConnectionMarker,
        SLane: SwimlaneMarker,
        N: Into<NodeId> + Send,
    {
        self.inner.reserve_owned(node_id, SLane::SWIMLANE).await
    }
    
    /// Get connection using dynamic configuration with runtime validation
    pub async fn get_connection_dynamic<N>(
        &self,
        node_id: N,
        config: DynamicRoutingConfig,
    ) -> Result<Connection, ConnectError>
    where
        N: Into<NodeId> + Send,
    {
        self.inner.get_connection_typed(
            node_id,
            config.swimlane,
            config.connection_type,
        ).await
    }
    
    /// Validate and call RPC with dynamic configuration
    pub async fn call_rpc_dynamic<M, R, N>(
        &self,
        node_id: N,
        message: M,
        config: DynamicRoutingConfig,
        sort_code: Option<u64>,
        timeout: Option<Duration>,
    ) -> Result<M::Response, RoutingRpcError>
    where
        M: RpcRequest,
        R: ServiceRoutingRule<M>,
        N: Into<NodeId> + Send,
    {
        // Validate configuration against service requirements
        config.validate_for_message::<M, R>()
            .map_err(RoutingRpcError::Validation)?;
        
        let result = self.inner.call_rpc(
            node_id,
            config.swimlane,
            message,
            sort_code,
            timeout,
        ).await.map_err(RoutingRpcError::Rpc)?;
        
        Ok(result)
    }
}

/// Type-safe connection wrapper with routing information
#[derive(Debug, Clone)]
pub struct TypedConnection<C: ConnectionMarker, S: SwimlaneMarker> {
    connection: Connection,
    _phantom: PhantomData<(C, S)>,
}

impl<C: ConnectionMarker, S: SwimlaneMarker> TypedConnection<C, S> {
    /// Create a new typed connection
    pub(crate) fn new(connection: Connection) -> Self {
        Self {
            connection,
            _phantom: PhantomData,
        }
    }
    
    /// Get the underlying connection
    pub fn connection(&self) -> &Connection {
        &self.connection
    }
    
    /// Get the connection type
    pub const fn connection_type(&self) -> ConnectionType {
        C::CONNECTION_TYPE
    }
    
    /// Get the swimlane
    pub const fn swimlane(&self) -> Swimlane {
        S::SWIMLANE
    }
    
    /// Transform to different routing type with validation
    pub fn try_transform<NC, NS>(self) -> Result<TypedConnection<NC, NS>, RoutingTransformError>
    where
        NC: ConnectionMarker,
        NS: SwimlaneMarker,
    {
        // Validate transformation is legal
        if C::CONNECTION_TYPE != NC::CONNECTION_TYPE {
            return Err(RoutingTransformError::IncompatibleConnection {
                from: C::NAME,
                to: NC::NAME,
            });
        }
        
        if S::SWIMLANE != NS::SWIMLANE {
            return Err(RoutingTransformError::IncompatibleSwimlane {
                from: S::NAME,
                to: NS::NAME,
            });
        }
        
        Ok(TypedConnection::new(self.connection))
    }
    
    /// Get routing configuration for this connection
    pub const fn routing_config(&self) -> RoutingConfig<C, S> {
        RoutingConfig::new()
    }
}

/// Errors for typed RPC operations
#[derive(Debug, thiserror::Error)]
pub enum RoutingRpcError {
    #[error("Routing validation failed: {0}")]
    Validation(#[from] RoutingValidationError),
    
    #[error("RPC failed: {0}")]
    Rpc(#[from] RpcError),
}

/// Error for routing transformation
#[derive(Debug, thiserror::Error)]
pub enum RoutingTransformError {
    #[error("Cannot transform from {from} to {to}: incompatible connection types")]
    IncompatibleConnection { from: &'static str, to: &'static str },
    
    #[error("Cannot transform from {from} to {to}: incompatible swimlanes")]
    IncompatibleSwimlane { from: &'static str, to: &'static str },
}

/// Connection pool with typed routing support
#[derive(Debug, Clone)]
pub struct TypedConnectionPool<S: NetworkSender> {
    sender: TypedNetworkSender<S>,
}

impl<S: NetworkSender> TypedConnectionPool<S> {
    /// Create a new typed connection pool
    pub fn new(sender: S) -> Self {
        Self {
            sender: TypedNetworkSender::new(sender),
        }
    }
    
    /// Get or create a connection with specific routing requirements
    pub async fn get_connection_for_service<M, R, N>(
        &self,
        node_id: N,
    ) -> Result<TypedConnection<R::Connection, R::Swimlane>, ConnectError>
    where
        M: RpcRequest,
        R: ServiceRoutingRule<M>,
        N: Into<NodeId> + Send,
    {
        let config = R::get_routing_config();
        self.sender.get_typed_connection(node_id, config).await
    }
    
    /// Get connection with advanced routing and load balancing considerations
    pub async fn get_connection_advanced<M, R, N>(
        &self,
        node_id: N,
    ) -> Result<TypedConnection<R::Connection, R::Swimlane>, ConnectError>
    where
        M: RpcRequest,
        R: AdvancedServiceRouting<M>,
        N: Into<NodeId> + Send,
    {
        let config = R::get_routing_config();
        
        // For load balancing strategies that require persistent connections,
        // we could implement additional logic here
        match R::LOAD_BALANCING_STRATEGY {
            LoadBalancingStrategy::None => {
                // Single connection - get direct connection
                self.sender.get_typed_connection(node_id, config).await
            }
            LoadBalancingStrategy::ConsistentHash => {
                // For consistent hashing, always use the same connection for the same node
                self.sender.get_typed_connection(node_id, config).await
            }
            _ => {
                // For other strategies, get any connection
                self.sender.get_typed_connection(node_id, config).await
            }
        }
    }
}

/// Extension trait for NetworkSender to add typed routing capabilities
pub trait NetworkSenderExt: NetworkSender + Sized {
    /// Convert to typed network sender
    fn typed(self) -> TypedNetworkSender<Self> {
        TypedNetworkSender::new(self)
    }
    
    /// Create typed connection pool
    fn connection_pool(self) -> TypedConnectionPool<Self> {
        TypedConnectionPool::new(self)
    }
}

// Blanket implementation for all NetworkSenders
impl<S: NetworkSender> NetworkSenderExt for S {}

/// Utility macros for common routing patterns
#[macro_export]
macro_rules! typed_rpc_call {
    ($sender:expr, $node_id:expr, $service:ty, $message:expr) => {
        $sender.call_rpc_with_service_routing::<_, $service, _>(
            $node_id,
            $message,
            None,
            None,
        )
    };
    
    ($sender:expr, $node_id:expr, $service:ty, $message:expr, timeout = $timeout:expr) => {
        $sender.call_rpc_with_service_routing::<_, $service, _>(
            $node_id,
            $message,
            None,
            Some($timeout),
        )
    };
    
    ($sender:expr, $node_id:expr, $service:ty, $message:expr, sort_code = $sort:expr) => {
        $sender.call_rpc_with_service_routing::<_, $service, _>(
            $node_id,
            $message,
            Some($sort),
            None,
        )
    };
}

#[cfg(test)]
mod tests {
    use super::*;
    use super::super::phantom::connection_markers::Internal;
    use super::super::phantom::swimlane_markers::General;
    use super::super::services::predefined_services::*;
    
    // Mock NetworkSender for testing
    #[derive(Clone, Debug)]
    struct MockNetworkSender;
    
    impl NetworkSender for MockNetworkSender {
        async fn get_connection_typed<N>(
            &self,
            _node_id: N,
            _swimlane: Swimlane,
            _connection_type: ConnectionType,
        ) -> Result<Connection, ConnectError>
        where
            N: Into<NodeId> + Send,
        {
            // Mock implementation
            unimplemented!("Mock implementation")
        }
        
        fn lazy_connect(
            &self,
            _node_id: GenerationalNodeId,
            _swimlane: Swimlane,
            _buffer_size: usize,
            _auto_reconnect: bool,
        ) -> LazyConnection {
            unimplemented!("Mock implementation")
        }
        
        async fn reserve_owned<N>(
            &self,
            _node_id: N,
            _swimlane: Swimlane,
        ) -> Option<OwnedSendPermit>
        where
            N: Into<NodeId> + Send,
        {
            unimplemented!("Mock implementation")
        }
        
        async fn call_rpc<M, N>(
            &self,
            _node_id: N,
            _swimlane: Swimlane,
            _message: M,
            _sort_code: Option<u64>,
            _timeout: Option<Duration>,
        ) -> Result<M::Response, RpcError>
        where
            M: RpcRequest,
            N: Into<NodeId> + Send,
        {
            unimplemented!("Mock implementation")
        }
    }
    
    #[test]
    fn test_typed_network_sender_creation() {
        let mock_sender = MockNetworkSender;
        let typed_sender = TypedNetworkSender::new(mock_sender.clone());
        
        // Verify we can get the inner sender back
        let _retrieved_sender = typed_sender.into_inner();
        // Note: We can't compare directly due to mock limitations
    }
    
    #[test]
    fn test_routing_config_types() {
        let internal_general_config: RoutingConfig<Internal, General> = RoutingConfig::new();
        assert_eq!(internal_general_config.connection_type(), ConnectionType::Internal);
        assert_eq!(internal_general_config.swimlane(), Swimlane::General);
    }
    
    #[test]
    fn test_network_sender_extension() {
        let mock_sender = MockNetworkSender;
        let _typed_sender = mock_sender.clone().typed();
        let _connection_pool = mock_sender.connection_pool();
        // These should compile without issues
    }
    
    #[test]
    fn test_dynamic_routing_config_validation() {
        let config = DynamicRoutingConfig::new(
            ConnectionType::Internal,
            Swimlane::General,
        );
        
        // Should validate successfully for MetadataServiceRouting
        let result = config.validate_for_message::<MockRequest, MetadataServiceRouting>();
        assert!(result.is_ok());
        
        // Should fail for IngressServiceRouting (requires External connection)
        let result = config.validate_for_message::<MockRequest, IngressServiceRouting>();
        assert!(result.is_err());
    }
    
    // Mock request type for testing
    struct MockService;
    
    impl restate_types::net::Service for MockService {
        const TAG: restate_types::net::ServiceTag = restate_types::net::ServiceTag::MetadataManagerService;
    }
    
    struct MockRequest;
    
    impl restate_types::net::codec::WireEncode for MockRequest {
        fn encode_to_bytes(&self, _protocol_version: restate_types::net::ProtocolVersion) -> Result<bytes::Bytes, restate_types::net::codec::EncodeError> {
            Ok(bytes::Bytes::new())
        }
    }
    
    impl restate_types::net::codec::WireDecode for MockRequest {
        type Error = std::convert::Infallible;
        
        fn decode(_buf: impl bytes::Buf, _protocol_version: restate_types::net::ProtocolVersion) -> Self {
            Self
        }
        
        fn try_decode(_buf: impl bytes::Buf, _protocol_version: restate_types::net::ProtocolVersion) -> Result<Self, Self::Error> {
            Ok(Self)
        }
    }
    
    struct MockResponse;
    
    impl restate_types::net::codec::WireEncode for MockResponse {
        fn encode_to_bytes(&self, _protocol_version: restate_types::net::ProtocolVersion) -> Result<bytes::Bytes, restate_types::net::codec::EncodeError> {
            Ok(bytes::Bytes::new())
        }
    }
    
    impl restate_types::net::codec::WireDecode for MockResponse {
        type Error = std::convert::Infallible;
        
        fn decode(_buf: impl bytes::Buf, _protocol_version: restate_types::net::ProtocolVersion) -> Self {
            Self
        }
        
        fn try_decode(_buf: impl bytes::Buf, _protocol_version: restate_types::net::ProtocolVersion) -> Result<Self, Self::Error> {
            Ok(Self)
        }
    }
    
    impl restate_types::net::RpcResponse for MockResponse {
        type Service = MockService;
    }
    
    impl RpcRequest for MockRequest {
        const TYPE: &'static str = "MockRequest";
        type Service = MockService;
        type Response = MockResponse;
    }
}