// Copyright (c) 2023 - 2025 Restate Software, Inc., Restate GmbH.
// All rights reserved.
//
// Use of this software is governed by the Business Source License
// included in the LICENSE file.
//
// As of the Change Date specified in that file, in accordance with
// the Business Source License, use of this software will be governed
// by the Apache License, Version 2.0.

use std::fs;
use std::path::Path;

use anyhow::Context;
use tonic::transport::{Certificate, Identity, ServerTlsConfig, ClientTlsConfig};

use restate_types::config::EffectiveTlsConfig;

/// Load certificates from PEM file
pub fn load_certificates(cert_path: &Path) -> anyhow::Result<Vec<u8>> {
    fs::read(cert_path)
        .with_context(|| format!("Failed to read certificate file: {}", cert_path.display()))
}

/// Load private key from PEM file
pub fn load_private_key(key_path: &Path) -> anyhow::Result<Vec<u8>> {
    fs::read(key_path)
        .with_context(|| format!("Failed to read private key file: {}", key_path.display()))
}

/// Load CA certificate from PEM file
pub fn load_ca_certificate(ca_cert_path: &Path) -> anyhow::Result<Vec<u8>> {
    fs::read(ca_cert_path)
        .with_context(|| format!("Failed to read CA certificate file: {}", ca_cert_path.display()))
}

/// Create server TLS configuration from effective TLS config
pub fn create_server_tls_config(config: &EffectiveTlsConfig) -> anyhow::Result<ServerTlsConfig> {
    if !config.enabled {
        anyhow::bail!("TLS is not enabled");
    }

    let cert_path = config.cert_path.as_ref()
        .ok_or_else(|| anyhow::anyhow!("Certificate path is required when TLS is enabled"))?;
    let key_path = config.key_path.as_ref()
        .ok_or_else(|| anyhow::anyhow!("Private key path is required when TLS is enabled"))?;

    let cert_pem = load_certificates(cert_path)?;
    let key_pem = load_private_key(key_path)?;

    let identity = Identity::from_pem(cert_pem, key_pem);
    let mut tls_config = ServerTlsConfig::new().identity(identity);

    // Configure mutual TLS if required
    if config.require_client_cert {
        let ca_cert_path = config.ca_cert_path.as_ref()
            .ok_or_else(|| anyhow::anyhow!("CA certificate path is required for mutual TLS"))?;
        
        let ca_cert_pem = load_ca_certificate(ca_cert_path)?;
        let ca_cert = Certificate::from_pem(ca_cert_pem);
        
        tls_config = tls_config.client_ca_root(ca_cert);
    }

    Ok(tls_config)
}

/// Create client TLS configuration from effective TLS config
pub fn create_client_tls_config(config: &EffectiveTlsConfig) -> anyhow::Result<ClientTlsConfig> {
    if !config.enabled {
        anyhow::bail!("TLS is not enabled");
    }

    let mut tls_config = ClientTlsConfig::new();

    // Add CA certificate for server verification if provided
    if let Some(ca_cert_path) = &config.ca_cert_path {
        let ca_cert_pem = load_ca_certificate(ca_cert_path)?;
        let ca_cert = Certificate::from_pem(ca_cert_pem);
        tls_config = tls_config.ca_certificate(ca_cert);
    }

    // Add client certificate for mutual TLS if required
    if config.require_client_cert {
        let cert_path = config.cert_path.as_ref()
            .ok_or_else(|| anyhow::anyhow!("Certificate path is required for mutual TLS"))?;
        let key_path = config.key_path.as_ref()
            .ok_or_else(|| anyhow::anyhow!("Private key path is required for mutual TLS"))?;

        let cert_pem = load_certificates(cert_path)?;
        let key_pem = load_private_key(key_path)?;

        let identity = Identity::from_pem(cert_pem, key_pem);
        tls_config = tls_config.identity(identity);
    }

    Ok(tls_config)
}

/// Validate TLS configuration
pub fn validate_tls_config(config: &EffectiveTlsConfig) -> anyhow::Result<()> {
    if !config.enabled {
        return Ok(());
    }

    // Check required files exist
    if let Some(cert_path) = &config.cert_path {
        if !cert_path.exists() {
            anyhow::bail!("Certificate file does not exist: {}", cert_path.display());
        }
    } else {
        anyhow::bail!("Certificate path is required when TLS is enabled");
    }

    if let Some(key_path) = &config.key_path {
        if !key_path.exists() {
            anyhow::bail!("Private key file does not exist: {}", key_path.display());
        }
    } else {
        anyhow::bail!("Private key path is required when TLS is enabled");
    }

    // Check CA certificate if mutual TLS is required
    if config.require_client_cert {
        if let Some(ca_cert_path) = &config.ca_cert_path {
            if !ca_cert_path.exists() {
                anyhow::bail!("CA certificate file does not exist: {}", ca_cert_path.display());
            }
        } else {
            anyhow::bail!("CA certificate path is required for mutual TLS");
        }
    }

    // Validate certificate files can be read
    let _cert_pem = load_certificates(config.cert_path.as_ref().unwrap())?;
    let _key_pem = load_private_key(config.key_path.as_ref().unwrap())?;

    if let Some(ca_cert_path) = &config.ca_cert_path {
        let _ca_cert_pem = load_ca_certificate(ca_cert_path)?;
    }

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;
    use tempfile::NamedTempFile;
    use std::io::Write;

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

    #[test]
    fn test_load_certificates() {
        let mut temp_file = NamedTempFile::new().unwrap();
        temp_file.write_all(TEST_CERT_PEM.as_bytes()).unwrap();
        
        let cert_data = load_certificates(temp_file.path()).unwrap();
        assert_eq!(cert_data, TEST_CERT_PEM.as_bytes());
    }

    #[test]
    fn test_load_private_key() {
        let mut temp_file = NamedTempFile::new().unwrap();
        temp_file.write_all(TEST_KEY_PEM.as_bytes()).unwrap();
        
        let key_data = load_private_key(temp_file.path()).unwrap();
        assert_eq!(key_data, TEST_KEY_PEM.as_bytes());
    }

    #[test]
    fn test_validate_disabled_tls_config() {
        let config = EffectiveTlsConfig {
            enabled: false,
            cert_path: None,
            key_path: None,
            ca_cert_path: None,
            require_client_cert: false,
        };
        
        assert!(validate_tls_config(&config).is_ok());
    }

    #[test]
    fn test_validate_missing_cert_path() {
        let config = EffectiveTlsConfig {
            enabled: true,
            cert_path: None,
            key_path: Some(PathBuf::from("key.pem")),
            ca_cert_path: None,
            require_client_cert: false,
        };
        
        let result = validate_tls_config(&config);
        assert!(result.is_err());
        assert!(result.unwrap_err().to_string().contains("Certificate path is required"));
    }
}