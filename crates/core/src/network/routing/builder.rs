// Copyright (c) 2023 - 2025 Restate Software, Inc., Restate GmbH.
// All rights reserved.
//
// Use of this software is governed by the Business Source License
// included in the LICENSE file.
//
// As of the Change Date specified in that file, in accordance with
// the Business Source License, use of this software will be governed
// by the Apache License, Version 2.0.

//! Type-safe routing configuration builder with compile-time validation
//!
//! This module provides a builder pattern with phantom types for constructing
//! routing configurations that are validated at compile time.

use std::marker::PhantomData;
use super::phantom::{
    connection_markers::{ConnectionMarker, Internal, External},
    swimlane_markers::{SwimlaneMarker, General, Gossip, BifrostData, IngressData},
    service_categories::ServiceRoutingRule,
    RoutingConfig, RoutingWitness,
};
use super::super::{ConnectionType, Swimlane};
use restate_types::net::RpcRequest;

/// Type-level state for builder validation
pub mod builder_states {
    /// Initial state - no configuration set
    #[derive(Debug, Clone, Copy)]
    pub struct Initial;
    
    /// Connection type configured
    #[derive(Debug, Clone, Copy)]
    pub struct ConnectionConfigured<C: super::super::phantom::connection_markers::ConnectionMarker>(
        pub(super) std::marker::PhantomData<C>
    );
    
    /// Both connection and swimlane configured - ready to build
    #[derive(Debug, Clone, Copy)]
    pub struct Complete<
        C: super::super::phantom::connection_markers::ConnectionMarker,
        S: super::super::phantom::swimlane_markers::SwimlaneMarker,
    >(
        pub(super) std::marker::PhantomData<C>,
        pub(super) std::marker::PhantomData<S>,
    );
    
    /// Trait to mark valid builder states
    pub trait BuilderState: 'static {}
    
    impl BuilderState for Initial {}
    impl<C: super::super::phantom::connection_markers::ConnectionMarker> BuilderState for ConnectionConfigured<C> {}
    impl<
        C: super::super::phantom::connection_markers::ConnectionMarker,
        S: super::super::phantom::swimlane_markers::SwimlaneMarker,
    > BuilderState for Complete<C, S> {}
}

/// Type-safe routing configuration builder
#[derive(Debug, Clone)]
pub struct RoutingBuilder<State: builder_states::BuilderState> {
    _state: PhantomData<State>,
}

impl RoutingBuilder<builder_states::Initial> {
    /// Create a new routing builder
    pub const fn new() -> Self {
        Self {
            _state: PhantomData,
        }
    }
    
    /// Configure for internal connections
    pub fn internal(self) -> RoutingBuilder<builder_states::ConnectionConfigured<Internal>> {
        RoutingBuilder {
            _state: PhantomData,
        }
    }
    
    /// Configure for external connections
    pub fn external(self) -> RoutingBuilder<builder_states::ConnectionConfigured<External>> {
        RoutingBuilder {
            _state: PhantomData,
        }
    }
    
    /// Configure connection type dynamically (runtime validation)
    pub fn connection_type(self, conn_type: ConnectionType) -> RoutingBuilderDynamic {
        RoutingBuilderDynamic::new(conn_type)
    }
}

impl<C: ConnectionMarker> RoutingBuilder<builder_states::ConnectionConfigured<C>> {
    /// Configure general purpose swimlane
    pub fn general(self) -> RoutingBuilder<builder_states::Complete<C, General>> {
        RoutingBuilder {
            _state: PhantomData,
        }
    }
    
    /// Configure gossip protocol swimlane
    pub fn gossip(self) -> RoutingBuilder<builder_states::Complete<C, Gossip>> {
        RoutingBuilder {
            _state: PhantomData,
        }
    }
    
    /// Configure Bifrost data streaming swimlane
    pub fn bifrost_data(self) -> RoutingBuilder<builder_states::Complete<C, BifrostData>> {
        RoutingBuilder {
            _state: PhantomData,
        }
    }
    
    /// Configure ingress data swimlane
    pub fn ingress_data(self) -> RoutingBuilder<builder_states::Complete<C, IngressData>> {
        RoutingBuilder {
            _state: PhantomData,
        }
    }
    
    /// Configure swimlane dynamically (runtime validation)
    pub fn swimlane(self, swimlane: Swimlane) -> RoutingBuilderDynamic {
        RoutingBuilderDynamic::new_with_connection::<C>(swimlane)
    }
}

impl<C: ConnectionMarker, S: SwimlaneMarker> RoutingBuilder<builder_states::Complete<C, S>> {
    /// Build the final routing configuration
    pub const fn build(self) -> RoutingConfig<C, S> {
        RoutingConfig::new()
    }
    
    /// Build with witness for additional validation
    pub const fn build_with_witness(self) -> RoutingWitness<C, S> {
        RoutingWitness::new()
    }
    
    /// Validate configuration against service requirements
    pub fn validate_for_service<M: RpcRequest, R: ServiceRoutingRule<M>>(
        &self,
    ) -> Result<(), RoutingValidationError>
    where
        C: ConnectionMarker,
        S: SwimlaneMarker,
    {
        // Check connection type compatibility
        if C::CONNECTION_TYPE != R::Connection::CONNECTION_TYPE {
            return Err(RoutingValidationError::ConnectionMismatch {
                expected: R::Connection::NAME,
                found: C::NAME,
            });
        }
        
        // Check swimlane compatibility
        if S::SWIMLANE != R::Swimlane::SWIMLANE {
            return Err(RoutingValidationError::SwimlaneMismatch {
                expected: R::Swimlane::NAME,
                found: S::NAME,
            });
        }
        
        Ok(())
    }
}

/// Dynamic routing builder for runtime configuration
#[derive(Debug, Clone)]
pub struct RoutingBuilderDynamic {
    connection_type: Option<ConnectionType>,
    swimlane: Option<Swimlane>,
}

impl RoutingBuilderDynamic {
    /// Create new dynamic builder
    fn new(connection_type: ConnectionType) -> Self {
        Self {
            connection_type: Some(connection_type),
            swimlane: None,
        }
    }
    
    /// Create with pre-configured connection type
    fn new_with_connection<C: ConnectionMarker>(swimlane: Swimlane) -> Self {
        Self {
            connection_type: Some(C::CONNECTION_TYPE),
            swimlane: Some(swimlane),
        }
    }
    
    /// Set swimlane
    pub fn swimlane(mut self, swimlane: Swimlane) -> Self {
        self.swimlane = Some(swimlane);
        self
    }
    
    /// Build dynamic configuration
    pub fn build(self) -> Result<DynamicRoutingConfig, RoutingValidationError> {
        let connection_type = self.connection_type.ok_or(
            RoutingValidationError::MissingConfiguration("connection_type")
        )?;
        let swimlane = self.swimlane.ok_or(
            RoutingValidationError::MissingConfiguration("swimlane")
        )?;
        
        Ok(DynamicRoutingConfig {
            connection_type,
            swimlane,
        })
    }
}

/// Runtime routing configuration (fallback for dynamic scenarios)
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct DynamicRoutingConfig {
    pub connection_type: ConnectionType,
    pub swimlane: Swimlane,
}

impl DynamicRoutingConfig {
    /// Create new dynamic routing configuration
    pub const fn new(connection_type: ConnectionType, swimlane: Swimlane) -> Self {
        Self {
            connection_type,
            swimlane,
        }
    }
    
    /// Convert to typed configuration if types match
    pub fn try_to_typed<C: ConnectionMarker, S: SwimlaneMarker>(
        &self,
    ) -> Option<RoutingConfig<C, S>> {
        if self.connection_type == C::CONNECTION_TYPE && self.swimlane == S::SWIMLANE {
            Some(RoutingConfig::new())
        } else {
            None
        }
    }
    
    /// Validate against service requirements
    pub fn validate_for_message<M: RpcRequest, R: ServiceRoutingRule<M>>(
        &self,
    ) -> Result<(), RoutingValidationError> {
        if self.connection_type != R::Connection::CONNECTION_TYPE {
            return Err(RoutingValidationError::ConnectionMismatch {
                expected: R::Connection::NAME,
                found: match self.connection_type {
                    ConnectionType::Internal => "Internal",
                    ConnectionType::External => "External",
                },
            });
        }
        
        if self.swimlane != R::Swimlane::SWIMLANE {
            return Err(RoutingValidationError::SwimlaneMismatch {
                expected: R::Swimlane::NAME,
                found: match self.swimlane {
                    Swimlane::General => "General",
                    Swimlane::Gossip => "Gossip",
                    Swimlane::BifrostData => "BifrostData",
                    Swimlane::IngressData => "IngressData",
                },
            });
        }
        
        Ok(())
    }
}

/// Validation errors for routing configuration
#[derive(Debug, thiserror::Error)]
pub enum RoutingValidationError {
    #[error("Connection type mismatch: expected {expected}, found {found}")]
    ConnectionMismatch {
        expected: &'static str,
        found: &'static str,
    },
    
    #[error("Swimlane mismatch: expected {expected}, found {found}")]
    SwimlaneMismatch {
        expected: &'static str,
        found: &'static str,
    },
    
    #[error("Missing configuration: {0}")]
    MissingConfiguration(&'static str),
}

/// Compile-time routing configuration factory
pub struct RoutingFactory;

impl RoutingFactory {
    /// Create routing configuration for internal gossip
    pub const fn internal_gossip() -> RoutingConfig<Internal, Gossip> {
        RoutingConfig::new()
    }
    
    /// Create routing configuration for internal bifrost data
    pub const fn internal_bifrost() -> RoutingConfig<Internal, BifrostData> {
        RoutingConfig::new()
    }
    
    /// Create routing configuration for external ingress
    pub const fn external_ingress() -> RoutingConfig<External, IngressData> {
        RoutingConfig::new()
    }
    
    /// Create routing configuration for internal general
    pub const fn internal_general() -> RoutingConfig<Internal, General> {
        RoutingConfig::new()
    }
    
    /// Create routing configuration for external general
    pub const fn external_general() -> RoutingConfig<External, General> {
        RoutingConfig::new()
    }
}

/// Macro for creating routing configurations with type safety
#[macro_export]
macro_rules! routing_config {
    (internal, general) => {
        $crate::network::routing::builder::RoutingFactory::internal_general()
    };
    (internal, gossip) => {
        $crate::network::routing::builder::RoutingFactory::internal_gossip()
    };
    (internal, bifrost_data) => {
        $crate::network::routing::builder::RoutingFactory::internal_bifrost()
    };
    (external, general) => {
        $crate::network::routing::builder::RoutingFactory::external_general()
    };
    (external, ingress_data) => {
        $crate::network::routing::builder::RoutingFactory::external_ingress()
    };
}

/// Trait for services to define their routing requirements
pub trait HasRoutingRequirements<M: RpcRequest> {
    type Requirements: ServiceRoutingRule<M>;
    
    /// Get the routing configuration for this service
    fn routing_config() -> RoutingConfig<
        <Self::Requirements as ServiceRoutingRule<M>>::Connection,
        <Self::Requirements as ServiceRoutingRule<M>>::Swimlane,
    > {
        RoutingConfig::new()
    }
    
    /// Validate a dynamic configuration against requirements
    fn validate_config(config: &DynamicRoutingConfig) -> Result<(), RoutingValidationError> {
        config.validate_for_message::<M, Self::Requirements>()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::network::routing::phantom::service_categories::ExternalService;
    
    #[test]
    fn test_routing_builder_compile_time() {
        // Should compile - valid configuration
        let config = RoutingBuilder::new()
            .internal()
            .general()
            .build();
        
        assert_eq!(config.connection_type(), ConnectionType::Internal);
        assert_eq!(config.swimlane(), Swimlane::General);
    }
    
    #[test]
    fn test_routing_builder_with_witness() {
        let witness = RoutingBuilder::new()
            .external()
            .ingress_data()
            .build_with_witness();
        
        assert_eq!(witness.config().connection_type(), ConnectionType::External);
        assert_eq!(witness.config().swimlane(), Swimlane::IngressData);
    }
    
    #[test]
    fn test_dynamic_routing_builder() {
        let config = RoutingBuilder::new()
            .connection_type(ConnectionType::Internal)
            .swimlane(Swimlane::Gossip)
            .build()
            .unwrap();
        
        assert_eq!(config.connection_type, ConnectionType::Internal);
        assert_eq!(config.swimlane, Swimlane::Gossip);
    }
    
    #[test]
    fn test_routing_factory() {
        let gossip_config = RoutingFactory::internal_gossip();
        assert_eq!(gossip_config.connection_type(), ConnectionType::Internal);
        assert_eq!(gossip_config.swimlane(), Swimlane::Gossip);
        
        let ingress_config = RoutingFactory::external_ingress();
        assert_eq!(ingress_config.connection_type(), ConnectionType::External);
        assert_eq!(ingress_config.swimlane(), Swimlane::IngressData);
    }
    
    #[test]
    fn test_routing_macro() {
        let _internal_gossip = routing_config!(internal, gossip);
        let _external_general = routing_config!(external, general);
        // These should compile without issues
    }
    
    #[test]
    fn test_validation_error() {
        let dynamic_config = DynamicRoutingConfig::new(
            ConnectionType::Internal,
            Swimlane::General,
        );
        
        // This should fail validation for ExternalService
        let result = dynamic_config.validate_for_message::<MockRequest, ExternalService>();
        assert!(result.is_err());
        
        match result.unwrap_err() {
            RoutingValidationError::ConnectionMismatch { expected, found } => {
                assert_eq!(expected, "External");
                assert_eq!(found, "Internal");
            }
            _ => panic!("Expected connection mismatch error"),
        }
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
    
    impl restate_types::net::RpcRequest for MockRequest {
        const TYPE: &'static str = "MockRequest";
        type Service = MockService;
        type Response = MockResponse;
    }
}