use anyhow::{Context, Result};
use uuid::Uuid;

/// Parse VM ID from SSH username.
///
/// Expected format: `vm-{uuid}` (e.g., `vm-abc-123-def-456`)
///
/// Returns:
/// - `Ok(Some(uuid))` if username matches `vm-{uuid}` format
/// - `Ok(None)` if username doesn't match (will reject auth)
/// - `Err(_)` if UUID parsing fails
pub fn parse_vm_id_from_username(username: &str) -> Result<Option<Uuid>> {
    if let Some(uuid_str) = username.strip_prefix("vm-") {
        let vm_id = Uuid::parse_str(uuid_str)
            .context(format!("Invalid VM UUID in username: {}", uuid_str))?;
        Ok(Some(vm_id))
    } else {
        // Reject any other username format (including "root", etc.)
        Ok(None)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_parse_valid_vm_username() {
        let uuid = Uuid::new_v4();
        let username = format!("vm-{}", uuid);
        let result = parse_vm_id_from_username(&username).unwrap();
        assert_eq!(result, Some(uuid));
    }

    #[test]
    fn test_parse_invalid_username() {
        assert_eq!(parse_vm_id_from_username("root").unwrap(), None);
        assert_eq!(parse_vm_id_from_username("admin").unwrap(), None);
        assert_eq!(parse_vm_id_from_username("vm").unwrap(), None);
    }

    #[test]
    fn test_parse_malformed_uuid() {
        let result = parse_vm_id_from_username("vm-not-a-uuid");
        assert!(result.is_err());
    }
}
