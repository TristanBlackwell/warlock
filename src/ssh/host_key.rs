use anyhow::{Context, Result};
use russh_keys::key::KeyPair;
use std::fs;
use std::path::Path;
use tracing::{info, warn};

/// Default path for persistent SSH host key
const DEFAULT_HOST_KEY_PATH: &str = "/etc/warlock/ssh/ssh_host_ed25519_key";

/// Load an existing SSH host key from disk, or generate and save a new one.
///
/// This ensures the SSH server uses a persistent host key, preventing
/// "WARNING: REMOTE HOST IDENTIFICATION HAS CHANGED!" errors when the
/// server restarts.
///
/// # Arguments
/// * `key_path` - Path to store/load the host key (defaults to /etc/warlock/ssh/ssh_host_ed25519_key)
///
/// # Returns
/// * `Ok(KeyPair)` - The loaded or newly generated host key
/// * `Err(_)` - If key generation fails or there's a critical I/O error
///
/// # Fallback Behavior
/// If the key cannot be saved to disk (e.g., permission denied), a warning is logged
/// but the function returns the generated ephemeral key. This allows the server to
/// start even in restricted environments (non-root, read-only filesystem).
pub fn load_or_generate_host_key(key_path: Option<&str>) -> Result<KeyPair> {
    let path = Path::new(key_path.unwrap_or(DEFAULT_HOST_KEY_PATH));

    // Try to load existing key
    if path.exists() {
        info!("Loading SSH host key from {}", path.display());
        match russh_keys::load_secret_key(path, None) {
            Ok(key) => {
                info!("Successfully loaded persistent SSH host key");
                return Ok(key);
            }
            Err(e) => {
                warn!(
                    error = ?e,
                    "Failed to load SSH host key from {}, will generate new one",
                    path.display()
                );
            }
        }
    }

    // Generate new key
    info!("Generating new Ed25519 SSH host key");
    let host_key = KeyPair::generate_ed25519()
        .ok_or_else(|| anyhow::anyhow!("Failed to generate Ed25519 host key"))?;

    // Try to save the key to disk
    match save_host_key(&host_key, path) {
        Ok(()) => {
            info!("SSH host key saved to {}", path.display());
        }
        Err(e) => {
            warn!(
                error = ?e,
                "Failed to save SSH host key to {}. Using ephemeral key (will change on restart).",
                path.display()
            );
            warn!(
                "To fix: Ensure {} is writable or run with appropriate permissions",
                path.display()
            );
        }
    }

    Ok(host_key)
}

/// Save an SSH host key to disk in OpenSSH format.
///
/// This function:
/// 1. Creates the parent directory if it doesn't exist
/// 2. Converts the russh KeyPair to ssh-key format
/// 3. Encodes the key in OpenSSH format
/// 4. Writes to disk with restrictive permissions (0600)
/// 5. Optionally saves the public key (.pub file)
fn save_host_key(key: &KeyPair, path: &Path) -> Result<()> {
    // Create parent directory if it doesn't exist
    if let Some(parent) = path.parent() {
        if !parent.exists() {
            fs::create_dir_all(parent)
                .context(format!("Failed to create directory {}", parent.display()))?;
            info!("Created directory: {}", parent.display());
        }
    }

    // Convert russh KeyPair to ssh-key format
    // russh's KeyPair is an enum, we need to extract the bytes and convert
    let ssh_private_key = match key {
        KeyPair::Ed25519(signing_key) => {
            // Get the private key bytes from ed25519_dalek (32 bytes)
            let secret_bytes = signing_key.to_bytes();

            // Get the public key bytes (32 bytes)
            let public_bytes = signing_key.verifying_key().to_bytes();

            // ssh-key expects 64 bytes: [secret (32 bytes) || public (32 bytes)]
            let mut combined = [0u8; 64];
            combined[..32].copy_from_slice(&secret_bytes);
            combined[32..].copy_from_slice(&public_bytes);

            // Create an Ed25519Keypair from the combined bytes
            let ed25519_keypair = ssh_key::private::Ed25519Keypair::from_bytes(&combined)?;
            ssh_key::PrivateKey::from(ed25519_keypair)
        }
        _ => {
            anyhow::bail!("Only Ed25519 keys are currently supported for persistence");
        }
    };

    // Encode the private key in OpenSSH format
    let encoded = ssh_private_key
        .to_openssh(ssh_key::LineEnding::LF)?
        .to_string();

    // Write the private key with restrictive permissions
    fs::write(path, encoded.as_bytes())
        .context(format!("Failed to write private key to {}", path.display()))?;

    // Set permissions to 0600 (owner read/write only)
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let perms = fs::Permissions::from_mode(0o600);
        fs::set_permissions(path, perms).context("Failed to set private key permissions")?;
    }

    // Also save the public key for user convenience
    let pub_key_path = path.with_extension("pub");
    let public_key = ssh_private_key.public_key();
    let pub_encoded = public_key.to_openssh()?;

    fs::write(&pub_key_path, pub_encoded.as_bytes()).context(format!(
        "Failed to write public key to {}",
        pub_key_path.display()
    ))?;

    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let perms = fs::Permissions::from_mode(0o644);
        fs::set_permissions(&pub_key_path, perms)
            .context("Failed to set public key permissions")?;
    }

    info!("Saved public key to {}", pub_key_path.display());

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use tempfile::TempDir;

    #[test]
    fn test_generate_and_save_host_key() {
        let temp_dir = TempDir::new().unwrap();
        let key_path = temp_dir.path().join("ssh_host_ed25519_key");

        // Generate and save a key
        let key1 = load_or_generate_host_key(Some(key_path.to_str().unwrap())).unwrap();

        // Verify files were created
        assert!(key_path.exists(), "Private key should be created");
        assert!(
            key_path.with_extension("pub").exists(),
            "Public key should be created"
        );

        // Verify permissions on Unix
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let metadata = fs::metadata(&key_path).unwrap();
            let mode = metadata.permissions().mode();
            assert_eq!(
                mode & 0o777,
                0o600,
                "Private key should have 0600 permissions"
            );

            let pub_metadata = fs::metadata(&key_path.with_extension("pub")).unwrap();
            let pub_mode = pub_metadata.permissions().mode();
            assert_eq!(
                pub_mode & 0o777,
                0o644,
                "Public key should have 0644 permissions"
            );
        }

        // Load the key again
        let key2 = load_or_generate_host_key(Some(key_path.to_str().unwrap())).unwrap();

        // Verify we got the same key (compare public key bytes)
        let pub1 = key1.clone_public_key().unwrap();
        let pub2 = key2.clone_public_key().unwrap();

        // Both should be Ed25519 keys
        match (&pub1, &pub2) {
            (russh_keys::key::PublicKey::Ed25519(k1), russh_keys::key::PublicKey::Ed25519(k2)) => {
                assert_eq!(
                    k1.as_bytes(),
                    k2.as_bytes(),
                    "Loaded key should match saved key"
                );
            }
            _ => panic!("Expected Ed25519 keys"),
        }
    }

    #[test]
    fn test_fallback_to_ephemeral_on_permission_error() {
        // Try to save to a path that doesn't exist and can't be created
        // (e.g., /root/... when running as non-root)
        let key = load_or_generate_host_key(Some("/root/impossible/path/key")).unwrap();

        // Should still return a valid key (ephemeral)
        assert!(matches!(key, KeyPair::Ed25519(_)));
    }
}
