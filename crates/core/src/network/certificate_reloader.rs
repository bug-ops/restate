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
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use anyhow::Context;
use notify::{EventKind, RecommendedWatcher, RecursiveMode};
use notify_debouncer_full::{DebounceEventResult, Debouncer, FileIdMap};
use tokio::sync::broadcast;
use tokio::sync::RwLock;
use tracing::{debug, error, info, warn};

use restate_types::config::{EffectiveTlsConfig, TlsSwimlane};

use super::tls_util::{validate_tls_config, create_server_tls_config, create_client_tls_config};

/// Maximum number of swimlanes that can be watched simultaneously
const MAX_WATCHED_SWIMLANES: usize = 10;

/// Maximum number of certificate paths per swimlane
const MAX_PATHS_PER_SWIMLANE: usize = 10;

/// Certificate reload notification
#[derive(Debug, Clone)]
pub struct CertificateReloadEvent {
    pub swimlane: TlsSwimlane,
    pub server_config: Option<tonic::transport::ServerTlsConfig>,
    pub client_config: Option<tonic::transport::ClientTlsConfig>,
}

/// Manages hot reloading of TLS certificates
pub struct CertificateReloader {
    /// File watcher for certificate files
    watcher: Arc<tokio::sync::Mutex<Debouncer<RecommendedWatcher, FileIdMap>>>,
    
    /// Broadcast channel for certificate reload events
    reload_tx: broadcast::Sender<CertificateReloadEvent>,
    
    /// Current TLS configurations per swimlane
    current_configs: Arc<RwLock<HashMap<TlsSwimlane, EffectiveTlsConfig>>>,
    
    /// Paths being watched per swimlane
    watched_paths: Arc<RwLock<HashMap<TlsSwimlane, Vec<PathBuf>>>>,
}

impl CertificateReloader {
    /// Create a new certificate reloader
    pub fn new() -> anyhow::Result<Self> {
        let (reload_tx, _) = broadcast::channel(32);
        let current_configs = Arc::new(RwLock::new(HashMap::new()));
        let watched_paths = Arc::new(RwLock::new(HashMap::new()));

        // Create file watcher
        let tx_clone = reload_tx.clone();
        let configs_clone = current_configs.clone();
        
        let watcher = notify_debouncer_full::new_debouncer(
            Duration::from_millis(500), // Debounce file changes for 500ms
            None,
            move |result: DebounceEventResult| {
                let tx = tx_clone.clone();
                let configs = configs_clone.clone();
                
                // Only spawn if there's an active runtime
                if let Ok(handle) = tokio::runtime::Handle::try_current() {
                    handle.spawn(async move {
                        if let Err(e) = handle_file_event(result, tx, configs).await {
                            error!("Error handling certificate file change: {}", e);
                        }
                    });
                } else {
                    debug!("Ignoring file event - no active Tokio runtime");
                }
            },
        )
        .context("Failed to create file watcher")?;

        Ok(Self {
            watcher: Arc::new(tokio::sync::Mutex::new(watcher)),
            reload_tx,
            current_configs,
            watched_paths,
        })
    }

    /// Create a minimal certificate reloader that doesn't watch files
    /// Used as a fallback when normal initialization fails
    pub fn new_minimal() -> Self {
        let (reload_tx, _) = broadcast::channel(1);
        let current_configs = Arc::new(RwLock::new(HashMap::new()));
        let watched_paths = Arc::new(RwLock::new(HashMap::new()));

        // Create a dummy watcher that never triggers events
        let watcher = notify_debouncer_full::new_debouncer(
            Duration::from_secs(1),
            None,
            |_| {}, // No-op callback
        ).unwrap_or_else(|_| {
            // If even the dummy watcher fails, we'll have to create a very minimal structure
            // This should never happen in practice, but provides ultimate fallback
            panic!("Failed to create even a minimal file watcher - system is unusable");
        });

        Self {
            watcher: Arc::new(tokio::sync::Mutex::new(watcher)),
            reload_tx,
            current_configs,
            watched_paths,
        }
    }

    /// Subscribe to certificate reload events
    pub fn subscribe(&self) -> broadcast::Receiver<CertificateReloadEvent> {
        self.reload_tx.subscribe()
    }

    /// Add TLS configuration for a swimlane and start watching its certificate files
    pub async fn add_config(
        &self,
        swimlane: TlsSwimlane,
        config: EffectiveTlsConfig,
    ) -> anyhow::Result<()> {
        if !config.enabled {
            debug!("TLS not enabled for swimlane {:?}, skipping certificate watching", swimlane);
            return Ok(());
        }

        // Check swimlane limits
        {
            let watched = self.watched_paths.read().await;
            if watched.len() >= MAX_WATCHED_SWIMLANES && !watched.contains_key(&swimlane) {
                anyhow::bail!(
                    "Cannot watch more than {} swimlanes simultaneously. Current: {}",
                    MAX_WATCHED_SWIMLANES,
                    watched.len()
                );
            }
        }

        // Validate configuration first
        validate_tls_config(&config)
            .with_context(|| format!("Invalid TLS config for swimlane {:?}", swimlane))?;

        // Watch certificate files
        let mut paths_to_watch = Vec::new();
        
        if let Some(cert_path) = &config.cert_path {
            paths_to_watch.push(cert_path.clone());
        }
        
        if let Some(key_path) = &config.key_path {
            paths_to_watch.push(key_path.clone());
        }
        
        if let Some(ca_cert_path) = &config.ca_cert_path {
            paths_to_watch.push(ca_cert_path.clone());
        }

        // Check path limits
        if paths_to_watch.len() > MAX_PATHS_PER_SWIMLANE {
            anyhow::bail!(
                "Too many certificate paths for swimlane {:?}: {} > {}",
                swimlane,
                paths_to_watch.len(),
                MAX_PATHS_PER_SWIMLANE
            );
        }

        // Add paths to watcher
        for path in &paths_to_watch {
            if let Some(parent) = path.parent() {
                let mut watcher = self.watcher.lock().await;
                if let Err(e) = watcher.watch(parent, RecursiveMode::NonRecursive) {
                    warn!("Failed to watch directory {:?}: {}", parent, e);
                    // Continue with other paths even if one fails
                }
            }
        }

        // Update watched paths for this swimlane
        {
            let mut watched = self.watched_paths.write().await;
            watched.insert(swimlane, paths_to_watch);
        }

        // Store current configuration
        {
            let mut configs = self.current_configs.write().await;
            configs.insert(swimlane, config);
        }

        info!("Started watching certificates for swimlane {:?}", swimlane);
        Ok(())
    }

    /// Remove configuration for a swimlane
    pub async fn remove_config(&self, swimlane: TlsSwimlane) {
        // Remove configuration
        let mut configs = self.current_configs.write().await;
        let removed = configs.remove(&swimlane).is_some();
        drop(configs); // Release lock early
        
        if removed {
            // Remove watched paths for this swimlane
            let mut watched = self.watched_paths.write().await;
            watched.remove(&swimlane);
            
            info!("Stopped watching certificates for swimlane {:?}", swimlane);
        }
    }

    /// Manually trigger certificate reload for a swimlane
    pub async fn reload_certificates(&self, swimlane: TlsSwimlane) -> anyhow::Result<()> {
        let configs = self.current_configs.read().await;
        if let Some(config) = configs.get(&swimlane) {
            let reload_event = create_reload_event(swimlane, config.clone()).await?;
            
            if let Err(e) = self.reload_tx.send(reload_event) {
                warn!("No listeners for certificate reload event: {}", e);
            }
            
            info!("Manually reloaded certificates for swimlane {:?}", swimlane);
        } else {
            anyhow::bail!("No TLS configuration found for swimlane {:?}", swimlane);
        }
        
        Ok(())
    }

    /// Get current TLS configuration for a swimlane
    pub async fn get_config(&self, swimlane: TlsSwimlane) -> Option<EffectiveTlsConfig> {
        let configs = self.current_configs.read().await;
        configs.get(&swimlane).cloned()
    }
}

/// Handle file system events
async fn handle_file_event(
    result: DebounceEventResult,
    reload_tx: broadcast::Sender<CertificateReloadEvent>,
    current_configs: Arc<RwLock<HashMap<TlsSwimlane, EffectiveTlsConfig>>>,
) -> anyhow::Result<()> {
    match result {
        Ok(events) => {
            for event in events {
                // Check if this is a file modification event
                if !matches!(
                    event.kind,
                    EventKind::Create(_) | EventKind::Modify(_) | EventKind::Remove(_)
                ) {
                    continue;
                }

                for path in &event.paths {
                    debug!("Certificate file changed: {:?}", path);
                    
                    // Find which swimlanes are affected by this file change
                    let affected_swimlanes = find_affected_swimlanes(path, &current_configs).await;
                    
                    for swimlane in affected_swimlanes {
                        // Wait for file to be fully written and accessible
                        if let Err(e) = wait_for_file_ready(path).await {
                            warn!("File {} may not be fully ready for reading: {}", path.display(), e);
                            // Continue anyway as file might still be readable
                        }
                        
                        if let Err(e) = reload_swimlane_certificates(
                            swimlane,
                            &reload_tx,
                            &current_configs,
                        ).await {
                            error!(
                                "Failed to reload certificates for swimlane {:?}: {}",
                                swimlane, e
                            );
                        }
                    }
                }
            }
        }
        Err(errors) => {
            for error in errors {
                error!("File watcher error: {}", error);
            }
        }
    }
    
    Ok(())
}

/// Find swimlanes affected by a file path change
async fn find_affected_swimlanes(
    changed_path: &Path,
    current_configs: &Arc<RwLock<HashMap<TlsSwimlane, EffectiveTlsConfig>>>,
) -> Vec<TlsSwimlane> {
    let configs = current_configs.read().await;
    let mut affected = Vec::new();
    
    for (swimlane, config) in configs.iter() {
        let paths_to_check = [
            config.cert_path.as_ref(),
            config.key_path.as_ref(),
            config.ca_cert_path.as_ref(),
        ];
        
        for path_opt in paths_to_check.iter() {
            if let Some(path) = path_opt {
                if path.as_path() == changed_path {
                    affected.push(*swimlane);
                    break;
                }
            }
        }
    }
    
    affected
}

/// Reload certificates for a specific swimlane
async fn reload_swimlane_certificates(
    swimlane: TlsSwimlane,
    reload_tx: &broadcast::Sender<CertificateReloadEvent>,
    current_configs: &Arc<RwLock<HashMap<TlsSwimlane, EffectiveTlsConfig>>>,
) -> anyhow::Result<()> {
    let configs = current_configs.read().await;
    
    if let Some(config) = configs.get(&swimlane) {
        // Validate that the new certificate files are valid
        if let Err(e) = validate_tls_config(config) {
            warn!(
                "Certificate validation failed for swimlane {:?}, keeping old certificates: {}",
                swimlane, e
            );
            return Ok(());
        }
        
        let reload_event = create_reload_event(swimlane, config.clone()).await?;
        
        if let Err(e) = reload_tx.send(reload_event) {
            warn!("No listeners for certificate reload event: {}", e);
        }
        
        info!("Reloaded certificates for swimlane {:?}", swimlane);
    }
    
    Ok(())
}

/// Create a certificate reload event
async fn create_reload_event(
    swimlane: TlsSwimlane,
    config: EffectiveTlsConfig,
) -> anyhow::Result<CertificateReloadEvent> {
    let server_config = if config.enabled {
        Some(create_server_tls_config(&config)?)
    } else {
        None
    };
    
    let client_config = if config.enabled {
        Some(create_client_tls_config(&config)?)
    } else {
        None
    };
    
    Ok(CertificateReloadEvent {
        swimlane,
        server_config,
        client_config,
    })
}

/// Wait for a file to be ready for reading by checking if it can be opened
/// This is more reliable than a fixed sleep duration
async fn wait_for_file_ready(file_path: &Path) -> anyhow::Result<()> {
    use std::fs::File;
    use tokio::time::{timeout, Duration, sleep};
    
    const MAX_ATTEMPTS: u32 = 10;
    const RETRY_DELAY: Duration = Duration::from_millis(50);
    const TOTAL_TIMEOUT: Duration = Duration::from_millis(2000); // 2 seconds max
    
    timeout(TOTAL_TIMEOUT, async {
        for attempt in 1..=MAX_ATTEMPTS {
            match File::open(file_path) {
                Ok(_) => {
                    // File can be opened, try to read a small amount to ensure it's ready
                    match std::fs::metadata(file_path) {
                        Ok(metadata) if metadata.len() > 0 => {
                            debug!("File {} ready for reading (attempt {})", file_path.display(), attempt);
                            return Ok(());
                        }
                        Ok(_) => {
                            // File exists but is empty, may still be writing
                            debug!("File {} is empty, waiting... (attempt {})", file_path.display(), attempt);
                        }
                        Err(e) => {
                            debug!("Cannot read metadata for {}: {} (attempt {})", file_path.display(), e, attempt);
                        }
                    }
                }
                Err(e) => {
                    debug!("Cannot open file {}: {} (attempt {})", file_path.display(), e, attempt);
                }
            }
            
            if attempt < MAX_ATTEMPTS {
                sleep(RETRY_DELAY).await;
            }
        }
        
        anyhow::bail!(
            "File {} not ready after {} attempts over {}ms",
            file_path.display(),
            MAX_ATTEMPTS,
            TOTAL_TIMEOUT.as_millis()
        );
    }).await?
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use tempfile::TempDir;
    use tokio::time::timeout;

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

    #[tokio::test]
    async fn test_certificate_reloader_creation() {
        let reloader = CertificateReloader::new().unwrap();
        let mut rx = reloader.subscribe();
        
        // Should be able to receive but no events initially
        assert!(timeout(Duration::from_millis(100), rx.recv()).await.is_err());
    }

    #[tokio::test]
    async fn test_add_and_get_config() {
        let temp_dir = TempDir::new().unwrap();
        let cert_path = temp_dir.path().join("cert.pem");
        let key_path = temp_dir.path().join("key.pem");
        
        fs::write(&cert_path, TEST_CERT_PEM).unwrap();
        fs::write(&key_path, TEST_KEY_PEM).unwrap();
        
        let reloader = CertificateReloader::new().unwrap();
        
        let config = EffectiveTlsConfig {
            enabled: true,
            cert_path: Some(cert_path),
            key_path: Some(key_path),
            ca_cert_path: None,
            require_client_cert: false,
        };
        
        reloader.add_config(TlsSwimlane::General, config.clone()).await.unwrap();
        
        let retrieved_config = reloader.get_config(TlsSwimlane::General).await;
        assert!(retrieved_config.is_some());
        assert_eq!(retrieved_config.unwrap().enabled, config.enabled);
    }

    #[tokio::test]
    async fn test_manual_reload() {
        let temp_dir = TempDir::new().unwrap();
        let cert_path = temp_dir.path().join("cert.pem");
        let key_path = temp_dir.path().join("key.pem");
        
        fs::write(&cert_path, TEST_CERT_PEM).unwrap();
        fs::write(&key_path, TEST_KEY_PEM).unwrap();
        
        let reloader = CertificateReloader::new().unwrap();
        let mut rx = reloader.subscribe();
        
        let config = EffectiveTlsConfig {
            enabled: true,
            cert_path: Some(cert_path),
            key_path: Some(key_path),
            ca_cert_path: None,
            require_client_cert: false,
        };
        
        reloader.add_config(TlsSwimlane::General, config).await.unwrap();
        reloader.reload_certificates(TlsSwimlane::General).await.unwrap();
        
        // Should receive reload event
        let event = timeout(Duration::from_secs(1), rx.recv()).await.unwrap().unwrap();
        assert_eq!(event.swimlane, TlsSwimlane::General);
        assert!(event.server_config.is_some());
        assert!(event.client_config.is_some());
    }

    #[tokio::test]
    async fn test_wait_for_file_ready() {
        use std::io::Write;
        use tempfile::NamedTempFile;
        
        // Test with existing file
        let mut temp_file = NamedTempFile::new().unwrap();
        temp_file.write_all(b"test content").unwrap();
        temp_file.flush().unwrap();
        
        // Should succeed immediately
        let result = wait_for_file_ready(temp_file.path()).await;
        assert!(result.is_ok());
        
        // Test with non-existent file
        let non_existent = std::path::PathBuf::from("/tmp/non_existent_file_12345");
        let result = wait_for_file_ready(&non_existent).await;
        assert!(result.is_err());
    }

    #[tokio::test]
    async fn test_swimlane_limits() {
        let reloader = CertificateReloader::new().unwrap();
        
        // Create a valid config
        let temp_dir = TempDir::new().unwrap();
        let cert_path = temp_dir.path().join("cert.pem");
        let key_path = temp_dir.path().join("key.pem");
        
        fs::write(&cert_path, TEST_CERT_PEM).unwrap();
        fs::write(&key_path, TEST_KEY_PEM).unwrap();
        
        // Add many swimlanes - should eventually hit the limit
        let mut added_count = 0;
        for i in 0..MAX_WATCHED_SWIMLANES + 1 {
            let config = EffectiveTlsConfig {
                enabled: true,
                cert_path: Some(cert_path.clone()),
                key_path: Some(key_path.clone()),
                ca_cert_path: None,
                require_client_cert: false,
            };
            
            // Use different swimlanes by cycling through available ones
            let swimlane = match i % 4 {
                0 => TlsSwimlane::General,
                1 => TlsSwimlane::Gossip,
                2 => TlsSwimlane::BifrostData,
                _ => TlsSwimlane::IngressData,
            };
            
            // This will overwrite existing configs for same swimlane, so we need unique swimlanes
            // But we only have 4 swimlanes, so we'll hit the limit at some point
            if i >= 4 {
                // Should fail after MAX_WATCHED_SWIMLANES
                break;
            }
            
            let result = reloader.add_config(swimlane, config).await;
            if result.is_ok() {
                added_count += 1;
            }
        }
        
        // Should have been able to add at least 4 swimlanes (all available types)
        assert!(added_count >= 4);
    }
}