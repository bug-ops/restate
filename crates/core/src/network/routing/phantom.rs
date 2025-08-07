// Copyright (c) 2023 - 2025 Restate Software, Inc., Restate GmbH.
// All rights reserved.
//
// Use of this software is governed by the Business Source License
// included in the LICENSE file.
//
// As of the Change Date specified in that file, in accordance with
// the Business Source License, use of this software will be governed
// by the Apache License, Version 2.0.

//! Phantom type-based routing system for type-safe network routing
//!
//! This module provides a sophisticated compile-time routing system using phantom types
//! to encode routing rules and ensure invalid configurations are impossible to construct.
//! The system leverages the Rust type system to eliminate routing errors at compile time.

use std::marker::PhantomData;
use crate::network::{ConnectionType, Swimlane};

/// Phantom marker for connection types
pub mod connection_markers {
    use crate::network::ConnectionType;
    
    /// Marker for internal connections (node-to-node communication)
    #[derive(Debug, Clone, Copy, PartialEq, Eq)]
    pub struct Internal;
    
    /// Marker for external connections (user-facing APIs)
    #[derive(Debug, Clone, Copy, PartialEq, Eq)] 
    pub struct External;
    
    /// Type-level boolean for connection type validation
    pub trait ConnectionMarker: 'static + Send + Sync + Copy {
        /// The corresponding runtime ConnectionType
        const CONNECTION_TYPE: ConnectionType;
        
        /// Human-readable name for debugging
        const NAME: &'static str;
        
        /// Convert to runtime type
        fn to_connection_type() -> ConnectionType {
            Self::CONNECTION_TYPE
        }
    }
    
    impl ConnectionMarker for Internal {
        const CONNECTION_TYPE: ConnectionType = ConnectionType::Internal;
        const NAME: &'static str = "Internal";
    }
    
    impl ConnectionMarker for External {
        const CONNECTION_TYPE: ConnectionType = ConnectionType::External;
        const NAME: &'static str = "External";
    }
}

/// Phantom markers for swimlane types
pub mod swimlane_markers {
    use crate::network::Swimlane;
    
    /// Marker for general purpose communication
    #[derive(Debug, Clone, Copy, PartialEq, Eq)]
    pub struct General;
    
    /// Marker for gossip protocol communication  
    #[derive(Debug, Clone, Copy, PartialEq, Eq)]
    pub struct Gossip;
    
    /// Marker for Bifrost data streaming
    #[derive(Debug, Clone, Copy, PartialEq, Eq)]
    pub struct BifrostData;
    
    /// Marker for ingress data communication
    #[derive(Debug, Clone, Copy, PartialEq, Eq)]
    pub struct IngressData;
    
    /// Type-level trait for swimlane validation
    pub trait SwimlaneMarker: 'static + Send + Sync + Copy {
        /// The corresponding runtime Swimlane
        const SWIMLANE: Swimlane;
        
        /// Human-readable name for debugging
        const NAME: &'static str;
        
        /// Convert to runtime type
        fn to_swimlane() -> Swimlane {
            Self::SWIMLANE
        }
    }
    
    impl SwimlaneMarker for General {
        const SWIMLANE: Swimlane = Swimlane::General;
        const NAME: &'static str = "General";
    }
    
    impl SwimlaneMarker for Gossip {
        const SWIMLANE: Swimlane = Swimlane::Gossip;
        const NAME: &'static str = "Gossip";
    }
    
    impl SwimlaneMarker for BifrostData {
        const SWIMLANE: Swimlane = Swimlane::BifrostData;
        const NAME: &'static str = "BifrostData";
    }
    
    impl SwimlaneMarker for IngressData {
        const SWIMLANE: Swimlane = Swimlane::IngressData;
        const NAME: &'static str = "IngressData";
    }
}

/// Type-level service categorization
pub mod service_categories {
    use crate::network::ConnectionType;
    use super::connection_markers::{Internal, External, ConnectionMarker};
    use super::swimlane_markers::{General, Gossip, BifrostData, IngressData, SwimlaneMarker};
    use restate_types::net::RpcRequest;
    
    /// Core trait for compile-time service routing rules
    pub trait ServiceRoutingRule<M: RpcRequest>: 'static + Send + Sync {
        /// The connection type required for this service
        type Connection: ConnectionMarker;
        
        /// The swimlane used for this service
        type Swimlane: SwimlaneMarker;
        
        /// Validate if the service can route through the specified connection type
        fn can_route_via<C: ConnectionMarker>() -> bool {
            matches!(
                (Self::Connection::CONNECTION_TYPE, C::CONNECTION_TYPE),
                (ConnectionType::Internal, ConnectionType::Internal) |
                (ConnectionType::External, ConnectionType::External)
            )
        }
        
        /// Get the required routing configuration
        fn get_routing_config() -> super::RoutingConfig<Self::Connection, Self::Swimlane> {
            super::RoutingConfig::new()
        }
    }
    
    /// Default routing rule for internal services
    pub struct InternalService;
    
    /// Default routing rule for external services  
    pub struct ExternalService;
    
    /// Specialized routing for gossip protocol
    pub struct GossipService;
    
    /// Specialized routing for Bifrost data streaming
    pub struct BifrostService;
    
    /// Specialized routing for ingress data
    pub struct IngressService;
    
    /// Helper trait to define service-specific routing at compile time
    pub trait DefineServiceRouting<M: RpcRequest> {
        type Routing: ServiceRoutingRule<M>;
    }
    
    // Example implementations for common message types
    impl<M: RpcRequest> ServiceRoutingRule<M> for InternalService {
        type Connection = Internal;
        type Swimlane = General;
    }
    
    impl<M: RpcRequest> ServiceRoutingRule<M> for ExternalService {
        type Connection = External;
        type Swimlane = General;
    }
    
    impl<M: RpcRequest> ServiceRoutingRule<M> for GossipService {
        type Connection = Internal;
        type Swimlane = Gossip;
    }
    
    impl<M: RpcRequest> ServiceRoutingRule<M> for BifrostService {
        type Connection = Internal;
        type Swimlane = BifrostData;
    }
    
    impl<M: RpcRequest> ServiceRoutingRule<M> for IngressService {
        type Connection = External;
        type Swimlane = IngressData;
    }
}

/// Type-safe routing configuration
#[derive(Debug, Clone, Copy)]
pub struct RoutingConfig<C: connection_markers::ConnectionMarker, S: swimlane_markers::SwimlaneMarker> {
    _connection: PhantomData<C>,
    _swimlane: PhantomData<S>,
}

impl<C: connection_markers::ConnectionMarker, S: swimlane_markers::SwimlaneMarker> RoutingConfig<C, S> {
    /// Create a new routing configuration
    pub const fn new() -> Self {
        Self {
            _connection: PhantomData,
            _swimlane: PhantomData,
        }
    }
    
    /// Get the connection type for this configuration
    pub const fn connection_type(&self) -> ConnectionType {
        C::CONNECTION_TYPE
    }
    
    /// Get the swimlane for this configuration
    pub const fn swimlane(&self) -> Swimlane {
        S::SWIMLANE
    }
    
    /// Validate routing compatibility at compile time
    pub const fn is_compatible_with<OC: connection_markers::ConnectionMarker, OS: swimlane_markers::SwimlaneMarker>(
        &self,
        _other: &RoutingConfig<OC, OS>,
    ) -> bool {
        matches!(
            (C::CONNECTION_TYPE, OC::CONNECTION_TYPE),
            (ConnectionType::Internal, ConnectionType::Internal) |
            (ConnectionType::External, ConnectionType::External)
        ) && matches!(
            (S::SWIMLANE, OS::SWIMLANE),
            (Swimlane::General, Swimlane::General) |
            (Swimlane::Gossip, Swimlane::Gossip) |
            (Swimlane::BifrostData, Swimlane::BifrostData) |
            (Swimlane::IngressData, Swimlane::IngressData)
        )
    }
}

/// Phantom type-based routing witness for compile-time validation
#[derive(Debug, Clone, Copy)]
pub struct RoutingWitness<C: connection_markers::ConnectionMarker, S: swimlane_markers::SwimlaneMarker> {
    config: RoutingConfig<C, S>,
}

impl<C: connection_markers::ConnectionMarker, S: swimlane_markers::SwimlaneMarker> RoutingWitness<C, S> {
    /// Create a routing witness with compile-time validation
    pub const fn new() -> Self {
        Self {
            config: RoutingConfig::new(),
        }
    }
    
    /// Get the validated routing configuration
    pub const fn config(&self) -> &RoutingConfig<C, S> {
        &self.config
    }
    
    /// Transform to different routing with type-level validation
    pub fn transform_to<NC: connection_markers::ConnectionMarker, NS: swimlane_markers::SwimlaneMarker>(
        self,
    ) -> Result<RoutingWitness<NC, NS>, RoutingTransformError>
    where
        Self: CanTransformTo<NC, NS>,
    {
        Ok(RoutingWitness::<NC, NS>::new())
    }
}

/// Trait for compile-time routing transformation validation
pub trait CanTransformTo<NC: connection_markers::ConnectionMarker, NS: swimlane_markers::SwimlaneMarker> {}

/// Error type for routing transformation
#[derive(Debug, thiserror::Error)]
pub enum RoutingTransformError {
    #[error("Cannot transform from {from} to {to}: incompatible connection types")]
    IncompatibleConnection { from: &'static str, to: &'static str },
    
    #[error("Cannot transform from {from} to {to}: incompatible swimlanes")]
    IncompatibleSwimlane { from: &'static str, to: &'static str },
}

// Implement safe transformations
use connection_markers::{Internal, External};

// Same connection type transformations are always allowed
impl<S: swimlane_markers::SwimlaneMarker> CanTransformTo<Internal, S> for RoutingWitness<Internal, S> {}
impl<S: swimlane_markers::SwimlaneMarker> CanTransformTo<External, S> for RoutingWitness<External, S> {}

// Cross-connection transformations are forbidden (compile-time error)
// impl<S: swimlane_markers::SwimlaneMarker> CanTransformTo<External, S> for RoutingWitness<Internal, S> {} // Deliberately not implemented
// impl<S: swimlane_markers::SwimlaneMarker> CanTransformTo<Internal, S> for RoutingWitness<External, S> {} // Deliberately not implemented

#[cfg(test)]
mod tests {
    use super::*;
    use connection_markers::{Internal, External};
    use swimlane_markers::{General, Gossip};
    
    #[test]
    fn test_routing_config_creation() {
        let config: RoutingConfig<Internal, General> = RoutingConfig::new();
        assert_eq!(config.connection_type(), ConnectionType::Internal);
        assert_eq!(config.swimlane(), Swimlane::General);
    }
    
    #[test]
    fn test_routing_witness_creation() {
        let witness: RoutingWitness<Internal, Gossip> = RoutingWitness::new();
        assert_eq!(witness.config().connection_type(), ConnectionType::Internal);
        assert_eq!(witness.config().swimlane(), Swimlane::Gossip);
    }
    
    #[test]
    fn test_compatibility_check() {
        let internal_general: RoutingConfig<Internal, General> = RoutingConfig::new();
        let internal_gossip: RoutingConfig<Internal, Gossip> = RoutingConfig::new();
        let external_general: RoutingConfig<External, General> = RoutingConfig::new();
        
        // Same connection type, different swimlanes should be incompatible
        assert!(!internal_general.is_compatible_with(&internal_gossip));
        
        // Different connection types should be incompatible  
        assert!(!internal_general.is_compatible_with(&external_general));
        
        // Same config should be compatible
        assert!(internal_general.is_compatible_with(&internal_general));
    }
    
    // This should not compile - demonstrates compile-time safety
    // #[test] 
    // fn test_invalid_transformation() {
    //     let internal_witness: RoutingWitness<Internal, General> = RoutingWitness::new();
    //     let _external_witness: RoutingWitness<External, General> = internal_witness.transform_to().unwrap(); // Compile error!
    // }
}