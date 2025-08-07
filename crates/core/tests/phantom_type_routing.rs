// Copyright (c) 2023 - 2025 Restate Software, Inc., Restate GmbH.
// All rights reserved.
//
// Use of this software is governed by the Business Source License
// included in the LICENSE file.
//
// As of the Change Date specified in that file, in accordance with
// the Business Source License, use of this software will be governed
// by the Apache License, Version 2.0.

//! Integration tests for phantom type routing system in split mode

use restate_core::network::{ConnectionType, Swimlane};
use restate_core::network::routing::{
    RoutingConfig, RoutingWitness, RoutingMatrix, RoutingFactory,
    Internal, External, General, Gossip, BifrostData, IngressData,
    ConnectionMarker, SwimlaneMarker,
    common_configs,
};

#[test]
fn test_phantom_type_compile_time_safety() {
    // Test that phantom types provide compile-time routing validation
    let _config_internal_general: RoutingConfig<Internal, General> = RoutingConfig::new();
    let _config_external_ingress: RoutingConfig<External, IngressData> = RoutingConfig::new();
    
    // Test type aliases work
    let _internal_general: common_configs::InternalGeneral = RoutingConfig::new();
    let _external_ingress: common_configs::ExternalIngress = RoutingConfig::new();
}

#[test]
fn test_phantom_marker_properties() {
    // Test connection type markers
    assert_eq!(Internal::CONNECTION_TYPE, ConnectionType::Internal);
    assert_eq!(External::CONNECTION_TYPE, ConnectionType::External);
    
    // Test swimlane markers
    assert_eq!(General::SWIMLANE, Swimlane::General);
    assert_eq!(Gossip::SWIMLANE, Swimlane::Gossip);
    assert_eq!(BifrostData::SWIMLANE, Swimlane::BifrostData);
    assert_eq!(IngressData::SWIMLANE, Swimlane::IngressData);
}

#[test]
fn test_routing_witness_creation() {
    let witness: RoutingWitness<Internal, General> = RoutingWitness::new();
    let config = witness.config();
    
    // Verify witness provides correct runtime values
    assert_eq!(config.connection_type(), ConnectionType::Internal);
    assert_eq!(config.swimlane(), Swimlane::General);
}

#[test]
fn test_routing_matrix_validation() {
    // Test valid routing combinations
    let _internal_general: RoutingMatrix<Internal, General> = RoutingMatrix::new();
    let _internal_gossip: RoutingMatrix<Internal, Gossip> = RoutingMatrix::new();
    let _external_ingress: RoutingMatrix<External, IngressData> = RoutingMatrix::new();
    
    // These demonstrate compile-time validation
    // Invalid combinations would fail to compile
}

#[test]
fn test_routing_factory_methods() {
    // Test factory methods for common configurations
    let _internal_general = RoutingFactory::internal_general();
    let _internal_gossip = RoutingFactory::internal_gossip();
    let _external_general = RoutingFactory::external_general();
    let _external_ingress = RoutingFactory::external_ingress();
}

#[test]
fn test_split_mode_routing_configurations() {
    // Test configurations specifically for split mode servers
    
    // Internal server configurations (port 5123)
    let internal_gossip: RoutingConfig<Internal, Gossip> = RoutingConfig::new();
    assert_eq!(internal_gossip.connection_type(), ConnectionType::Internal);
    assert_eq!(internal_gossip.swimlane(), Swimlane::Gossip);
    
    let internal_general: RoutingConfig<Internal, General> = RoutingConfig::new();
    assert_eq!(internal_general.connection_type(), ConnectionType::Internal);
    assert_eq!(internal_general.swimlane(), Swimlane::General);
    
    // External server configurations (port 5122)
    let external_general: RoutingConfig<External, General> = RoutingConfig::new();
    assert_eq!(external_general.connection_type(), ConnectionType::External);
    assert_eq!(external_general.swimlane(), Swimlane::General);
    
    let external_ingress: RoutingConfig<External, IngressData> = RoutingConfig::new();
    assert_eq!(external_ingress.connection_type(), ConnectionType::External);
    assert_eq!(external_ingress.swimlane(), Swimlane::IngressData);
}

#[test]
fn test_phantom_zero_cost_abstraction() {
    use std::mem;
    
    // Verify phantom types are zero-cost abstractions
    assert_eq!(mem::size_of::<RoutingConfig<Internal, General>>(), 0);
    assert_eq!(mem::size_of::<RoutingConfig<External, IngressData>>(), 0);
    assert_eq!(mem::size_of::<RoutingMatrix<Internal, General>>(), 0);
    
    // Phantom markers should also be zero-cost
    assert_eq!(mem::size_of::<Internal>(), 0);
    assert_eq!(mem::size_of::<External>(), 0);
    assert_eq!(mem::size_of::<General>(), 0);
    assert_eq!(mem::size_of::<Gossip>(), 0);
}

#[test]
fn test_routing_config_compatibility() {
    let internal_general = RoutingConfig::<Internal, General>::new();
    let internal_general_2 = RoutingConfig::<Internal, General>::new();
    let external_general = RoutingConfig::<External, General>::new();
    
    // Same types should be compatible
    assert!(internal_general.is_compatible_with(&internal_general_2));
    
    // Different connection types should not be compatible
    assert!(!internal_general.is_compatible_with(&external_general));
}

#[test] 
fn test_split_mode_service_routing() {
    // Test that different services would use correct routing configurations
    // This demonstrates the intended usage patterns for split mode
    
    // Admin operations -> External server
    let admin_config = RoutingConfig::<External, General>::new();
    assert_eq!(admin_config.connection_type(), ConnectionType::External);
    
    // Gossip operations -> Internal server
    let gossip_config = RoutingConfig::<Internal, Gossip>::new();
    assert_eq!(gossip_config.connection_type(), ConnectionType::Internal);
    
    // Bifrost replication -> Internal server  
    let bifrost_config = RoutingConfig::<Internal, BifrostData>::new();
    assert_eq!(bifrost_config.connection_type(), ConnectionType::Internal);
    
    // Ingress data -> External server
    let ingress_config = RoutingConfig::<External, IngressData>::new();
    assert_eq!(ingress_config.connection_type(), ConnectionType::External);
}