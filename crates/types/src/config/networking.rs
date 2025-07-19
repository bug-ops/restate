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
use std::num::NonZeroUsize;
use std::path::PathBuf;
use std::time::Duration;

use restate_serde_util::NonZeroByteCount;

use crate::retries::RetryPolicy;
use serde::{Deserialize, Serialize};
use serde_with::serde_as;

/// # Swimlane identifier for TLS configuration
///
/// Different types of inter-node communication channels that can have
/// independent TLS configurations.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[cfg_attr(feature = "schemars", derive(schemars::JsonSchema))]
#[serde(rename_all = "snake_case")]
pub enum TlsSwimlane {
    /// General inter-node communication
    General,
    /// Gossip protocol for failure detection
    Gossip,
    /// Bifrost data replication streams
    BifrostData,
    /// Ingress data streams
    IngressData,
}

/// # TLS Configuration
///
/// Configuration for TLS encryption in inter-node communication.
#[derive(Debug, Clone, Serialize, Deserialize, derive_builder::Builder)]
#[cfg_attr(feature = "schemars", derive(schemars::JsonSchema))]
#[cfg_attr(feature = "schemars", schemars(rename = "TlsConfig", default))]
#[builder(default)]
#[serde(rename_all = "kebab-case")]
pub struct TlsConfig {
    /// # Enable TLS
    ///
    /// Enable TLS encryption for inter-node communication.
    pub enabled: bool,

    /// # Certificate file path
    ///
    /// Path to the TLS certificate file in PEM format.
    /// Required when TLS is enabled.
    pub cert_path: Option<PathBuf>,

    /// # Private key file path
    ///
    /// Path to the TLS private key file in PEM format.
    /// Required when TLS is enabled.
    pub key_path: Option<PathBuf>,

    /// # CA certificate file path
    ///
    /// Path to the Certificate Authority (CA) certificate file in PEM format.
    /// Used for client certificate verification in mutual TLS.
    pub ca_cert_path: Option<PathBuf>,

    /// # Require client certificates
    ///
    /// When enabled, requires clients to present valid certificates (mutual TLS).
    /// The CA certificate path must be specified for client verification.
    pub require_client_cert: bool,

    /// # Per-swimlane TLS overrides
    ///
    /// Override TLS settings for specific communication channels.
    /// If not specified for a swimlane, the global TLS settings apply.
    pub swimlane_overrides: HashMap<TlsSwimlane, SwimlaneTlsConfig>,
}

/// # Swimlane-specific TLS Configuration
///
/// TLS configuration override for a specific communication channel.
#[derive(Debug, Clone, Serialize, Deserialize, derive_builder::Builder)]
#[cfg_attr(feature = "schemars", derive(schemars::JsonSchema))]
#[cfg_attr(feature = "schemars", schemars(rename = "SwimlaneTlsConfig", default))]
#[builder(default)]
#[serde(rename_all = "kebab-case")]
pub struct SwimlaneTlsConfig {
    /// # Enable TLS for this swimlane
    ///
    /// If not specified, inherits from global TLS configuration.
    pub enabled: Option<bool>,

    /// # Certificate file path override
    ///
    /// Override certificate path for this specific swimlane.
    /// If not specified, uses global certificate configuration.
    pub cert_path: Option<PathBuf>,

    /// # Private key file path override
    ///
    /// Override private key path for this specific swimlane.
    /// If not specified, uses global private key configuration.
    pub key_path: Option<PathBuf>,

    /// # CA certificate override
    ///
    /// Override CA certificate for this specific swimlane.
    /// If not specified, uses global CA certificate configuration.
    pub ca_cert_path: Option<PathBuf>,

    /// # Require client certificates override
    ///
    /// Override mutual TLS requirement for this specific swimlane.
    /// If not specified, uses global requirement setting.
    pub require_client_cert: Option<bool>,
}

impl Default for TlsConfig {
    fn default() -> Self {
        Self {
            enabled: false,
            cert_path: None,
            key_path: None,
            ca_cert_path: None,
            require_client_cert: false,
            swimlane_overrides: HashMap::new(),
        }
    }
}

impl Default for SwimlaneTlsConfig {
    fn default() -> Self {
        Self {
            enabled: None,
            cert_path: None,
            key_path: None,
            ca_cert_path: None,
            require_client_cert: None,
        }
    }
}

impl TlsConfig {
    /// Get effective TLS configuration for a specific swimlane
    pub fn for_swimlane(&self, swimlane: TlsSwimlane) -> EffectiveTlsConfig {
        let override_config = self.swimlane_overrides.get(&swimlane);

        EffectiveTlsConfig {
            enabled: override_config
                .and_then(|c| c.enabled)
                .unwrap_or(self.enabled),
            cert_path: override_config
                .and_then(|c| c.cert_path.as_ref())
                .or(self.cert_path.as_ref())
                .cloned(),
            key_path: override_config
                .and_then(|c| c.key_path.as_ref())
                .or(self.key_path.as_ref())
                .cloned(),
            ca_cert_path: override_config
                .and_then(|c| c.ca_cert_path.as_ref())
                .or(self.ca_cert_path.as_ref())
                .cloned(),
            require_client_cert: override_config
                .and_then(|c| c.require_client_cert)
                .unwrap_or(self.require_client_cert),
        }
    }
}

/// # Effective TLS Configuration
///
/// Resolved TLS configuration for a specific swimlane after applying overrides.
#[derive(Debug, Clone)]
pub struct EffectiveTlsConfig {
    pub enabled: bool,
    pub cert_path: Option<PathBuf>,
    pub key_path: Option<PathBuf>,
    pub ca_cert_path: Option<PathBuf>,
    pub require_client_cert: bool,
}

/// # Networking options
///
/// Common network configuration options for communicating with Restate cluster nodes. Note that
/// similar keys are present in other config sections, such as in Service Client options.
#[serde_as]
#[derive(Debug, Clone, Serialize, Deserialize, derive_builder::Builder)]
#[cfg_attr(feature = "schemars", derive(schemars::JsonSchema))]
#[cfg_attr(feature = "schemars", schemars(rename = "NetworkingOptions", default))]
#[builder(default)]
#[serde(rename_all = "kebab-case")]
pub struct NetworkingOptions {
    /// # Connect timeout
    ///
    /// TCP connection timeout for Restate cluster node-to-node network connections.
    #[serde_as(as = "serde_with::DisplayFromStr")]
    #[cfg_attr(feature = "schemars", schemars(with = "String"))]
    pub connect_timeout: humantime::Duration,

    /// # Connect retry policy
    ///
    /// Retry policy to use for internal node-to-node networking.
    pub connect_retry_policy: RetryPolicy,

    /// # Handshake timeout
    ///
    /// Timeout for receiving a handshake response from Restate cluster peers.
    #[serde_as(as = "serde_with::DisplayFromStr")]
    #[cfg_attr(feature = "schemars", schemars(with = "String"))]
    pub handshake_timeout: humantime::Duration,

    /// # HTTP/2 Keep Alive Interval
    #[serde_as(as = "serde_with::DisplayFromStr")]
    #[cfg_attr(feature = "schemars", schemars(with = "String"))]
    pub http2_keep_alive_interval: humantime::Duration,

    /// # HTTP/2 Keep Alive Timeout
    #[serde_as(as = "serde_with::DisplayFromStr")]
    #[cfg_attr(feature = "schemars", schemars(with = "String"))]
    pub http2_keep_alive_timeout: humantime::Duration,

    /// # HTTP/2 Adaptive Window
    pub http2_adaptive_window: bool,

    /// # Data Stream Window Size
    ///
    /// Controls the number of bytes the can be sent on every data stream before inducing
    /// back pressure. Data streams are used for sending messages between nodes.
    ///
    /// The value should is often derived from BDP (Bandwidth Delay Product) of the network. For
    /// instance, if the network has a bandwidth of 10 Gbps with a round-trip time of 5 ms, the BDP
    /// is 10 Gbps * 0.005 s = 6.25 MB. This means that the window size should be at least 6.25 MB
    /// to fully utilize the network bandwidth assuming the latency is constant. Our recommendation
    /// is to set the window size to 2x the BDP to account for any variations in latency.
    ///
    /// If network latency is high, it's recommended to set this to a higher value.
    /// Maximum theoretical value is 2^31-1 (2 GiB - 1), but we will sanitize this value to 500 MiB.
    data_stream_window_size: NonZeroByteCount,

    /// # TLS Configuration
    ///
    /// TLS encryption settings for inter-node communication.
    /// When enabled, all communication between Restate nodes will be encrypted.
    pub tls: TlsConfig,
}

impl NetworkingOptions {
    pub fn stream_window_size(&self) -> u32 {
        // santize to 500MiB if set higher
        let stream_window_size = self.data_stream_window_size.as_u64().min(500 * 1024 * 1024); // Sanitize to 500MiB if set higher.

        u32::try_from(stream_window_size).expect("window size too big")
    }

    pub fn connection_window_size(&self) -> u32 {
        self.stream_window_size() * 3
    }
}

impl Default for NetworkingOptions {
    fn default() -> Self {
        Self {
            connect_timeout: Duration::from_secs(3).into(),
            connect_retry_policy: RetryPolicy::exponential(
                Duration::from_millis(250),
                2.0,
                Some(10),
                Some(Duration::from_millis(3000)),
            ),
            handshake_timeout: Duration::from_secs(3).into(),
            http2_keep_alive_interval: Duration::from_secs(1).into(),
            http2_keep_alive_timeout: Duration::from_secs(3).into(),
            http2_adaptive_window: true,
            // 2MiB
            data_stream_window_size: NonZeroByteCount::new(
                NonZeroUsize::new(2 * 1024 * 1024).expect("Non zero number"),
            ),
            tls: TlsConfig::default(),
        }
    }
}
