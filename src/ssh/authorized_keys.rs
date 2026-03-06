use anyhow::Result;
use russh_keys::key::PublicKey;
use tracing::debug;

/// Validates that a given SSH public key is authorized for the provided list
/// of SSH keys in OpenSSH authorized_keys format.
///
/// # Arguments
/// * `key` - The public key from the SSH authentication attempt
/// * `authorized_keys` - List of authorized public keys in OpenSSH format
///   (e.g., "ssh-ed25519 AAAA..." or "ssh-rsa BBBB...")
///
/// # Returns
/// * `Ok(true)` if the key is authorized
/// * `Ok(false)` if the key is not in the authorized list
/// * `Err(_)` if there's an error parsing the authorized keys
pub fn is_key_authorized(key: &PublicKey, authorized_keys: &[String]) -> Result<bool> {
    if authorized_keys.is_empty() {
        debug!("No authorized keys configured for this VM");
        return Ok(false);
    }

    debug!(
        "Validating SSH key against {} authorized keys",
        authorized_keys.len()
    );

    for (idx, authorized_key) in authorized_keys.iter().enumerate() {
        let authorized_key = authorized_key.trim();

        if authorized_key.is_empty() || authorized_key.starts_with('#') {
            // Skip empty lines and comments
            continue;
        }

        // Parse the OpenSSH authorized_keys format: <key-type> <base64-data> [comment]
        // The russh parse_public_key_base64 expects ONLY the base64 data,
        // not the key type prefix or comment
        let parts: Vec<&str> = authorized_key.split_whitespace().collect();
        if parts.len() < 2 {
            debug!(
                idx,
                "Skipping malformed authorized key line (expected at least 2 parts)"
            );
            continue;
        }

        let base64_data = parts[1]; // e.g., "AAAAC3NzaC1lZDI1..."

        // Try to parse the authorized key (just the base64 part)
        match russh_keys::parse_public_key_base64(base64_data) {
            Ok(auth_key) => {
                // Compare keys using russh's key equality
                if keys_equal(key, &auth_key) {
                    debug!(idx, "SSH key matched authorized key at index {}", idx);
                    return Ok(true);
                }
            }
            Err(e) => {
                debug!(
                    idx,
                    error = ?e,
                    "Skipping malformed authorized key line"
                );
                continue;
            }
        }
    }

    debug!("SSH key not found in authorized keys list");
    Ok(false)
}

/// Compare two SSH public keys for equality.
///
/// This uses russh's internal key comparison which handles different
/// key types appropriately.
fn keys_equal(a: &PublicKey, b: &PublicKey) -> bool {
    use russh_keys::key::PublicKey::*;

    match (a, b) {
        (Ed25519(a_key), Ed25519(b_key)) => a_key == b_key,
        (RSA { key: a_key, .. }, RSA { key: b_key, .. }) => a_key == b_key,
        (EC { key: a_key }, EC { key: b_key }) => a_key == b_key,
        _ => false, // Different key types
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use russh_keys::key::KeyPair;

    #[test]
    fn test_is_key_authorized_empty_list() {
        // Generate a test key
        let key = KeyPair::generate_ed25519().unwrap();
        let public_key = key.clone_public_key().unwrap();

        let result = is_key_authorized(&public_key, &[]);
        assert!(result.is_ok());
        assert!(!result.unwrap());
    }

    #[test]
    fn test_is_key_authorized_matching_key() {
        // Generate a test key
        let key = KeyPair::generate_ed25519().unwrap();
        let public_key = key.clone_public_key().unwrap();

        // Serialize the public key to OpenSSH format
        let mut buf = Vec::new();
        russh_keys::write_public_key_base64(&mut buf, &public_key).unwrap();
        let key_str = String::from_utf8(buf).unwrap();

        let authorized_keys = vec![format!("{} comment@example.com", key_str.trim())];

        let result = is_key_authorized(&public_key, &authorized_keys);
        assert!(result.is_ok());
        assert!(result.unwrap(), "Key should be authorized");
    }

    #[test]
    fn test_is_key_authorized_non_matching_key() {
        // Generate two different keys
        let key1 = KeyPair::generate_ed25519().unwrap();
        let key2 = KeyPair::generate_ed25519().unwrap();

        let public_key1 = key1.clone_public_key().unwrap();
        let public_key2 = key2.clone_public_key().unwrap();

        // Serialize key2 to OpenSSH format
        let mut buf = Vec::new();
        russh_keys::write_public_key_base64(&mut buf, &public_key2).unwrap();
        let key2_str = String::from_utf8(buf).unwrap();

        let authorized_keys = vec![format!("{} different@example.com", key2_str.trim())];

        let result = is_key_authorized(&public_key1, &authorized_keys);
        assert!(result.is_ok());
        assert!(!result.unwrap());
    }

    #[test]
    fn test_is_key_authorized_multiple_keys() {
        // Generate test keys
        let key1 = KeyPair::generate_ed25519().unwrap();
        let key2 = KeyPair::generate_ed25519().unwrap();
        let target_key = KeyPair::generate_ed25519().unwrap();

        let public_key1 = key1.clone_public_key().unwrap();
        let public_key2 = key2.clone_public_key().unwrap();
        let target_public = target_key.clone_public_key().unwrap();

        // Serialize keys to OpenSSH format
        let mut buf1 = Vec::new();
        russh_keys::write_public_key_base64(&mut buf1, &public_key1).unwrap();
        let key1_str = String::from_utf8(buf1).unwrap();

        let mut buf_target = Vec::new();
        russh_keys::write_public_key_base64(&mut buf_target, &target_public).unwrap();
        let target_str = String::from_utf8(buf_target).unwrap();

        let mut buf2 = Vec::new();
        russh_keys::write_public_key_base64(&mut buf2, &public_key2).unwrap();
        let key2_str = String::from_utf8(buf2).unwrap();

        let authorized_keys = vec![
            format!("{} user1@example.com", key1_str.trim()),
            format!("{} user2@example.com", target_str.trim()),
            format!("{} user3@example.com", key2_str.trim()),
        ];

        let result = is_key_authorized(&target_public, &authorized_keys);
        assert!(result.is_ok());
        assert!(result.unwrap());
    }

    #[test]
    fn test_is_key_authorized_with_comments_and_empty_lines() {
        // Generate a test key
        let key = KeyPair::generate_ed25519().unwrap();
        let public_key = key.clone_public_key().unwrap();

        // Serialize to OpenSSH format
        let mut buf = Vec::new();
        russh_keys::write_public_key_base64(&mut buf, &public_key).unwrap();
        let key_str = String::from_utf8(buf).unwrap();

        let authorized_keys = vec![
            "# This is a comment".to_string(),
            "".to_string(),
            format!("{} valid@example.com", key_str.trim()),
            "# Another comment".to_string(),
        ];

        let result = is_key_authorized(&public_key, &authorized_keys);
        assert!(result.is_ok());
        assert!(result.unwrap());
    }
}
