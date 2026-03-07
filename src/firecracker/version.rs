use anyhow::{Context, Result, bail};
use semver::Version;

const MIN_FIRECRACKER_VERSION: &str = "1.14.1";

/// Parses Firecracker version output and validates it meets minimum requirements.
///
/// # Errors
///
/// Returns an error if:
/// - Version string cannot be parsed
/// - Version is below minimum required version
pub fn parse_and_validate_version(version_output: &str) -> Result<Version> {
    // Firecracker outputs format like "Firecracker v1.14.1" or "v1.14.1" on the first line
    // Additional lines may contain exit messages and timestamps - ignore them
    let first_line = version_output
        .lines()
        .next()
        .ok_or_else(|| anyhow::anyhow!("Version output was empty"))?;

    // Extract version number from the line (might be "Firecracker v1.14.1" or just "v1.14.1")
    // Split by whitespace and find the part that looks like a version
    let version_str = first_line
        .split_whitespace()
        .find(|s| s.starts_with('v') || s.chars().next().is_some_and(|c| c.is_ascii_digit()))
        .ok_or_else(|| anyhow::anyhow!("No version number found in output: {}", first_line))?
        .trim_start_matches('v');

    let version =
        Version::parse(version_str).context("Failed to parse Firecracker version string")?;

    let min_version =
        Version::parse(MIN_FIRECRACKER_VERSION).expect("MIN_FIRECRACKER_VERSION is invalid");

    if version < min_version {
        bail!(
            "Firecracker version {} is too old. Minimum required: {}",
            version,
            MIN_FIRECRACKER_VERSION
        );
    }

    Ok(version)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_parse_version_with_v_prefix() {
        let result = parse_and_validate_version("v1.14.1");
        assert!(result.is_ok());
        assert_eq!(result.unwrap(), Version::new(1, 14, 1));
    }

    #[test]
    fn test_parse_version_without_v_prefix() {
        let result = parse_and_validate_version("1.14.1");
        assert!(result.is_ok());
        assert_eq!(result.unwrap(), Version::new(1, 14, 1));
    }

    #[test]
    fn test_parse_version_with_whitespace() {
        let result = parse_and_validate_version("  v1.14.1\n");
        assert!(result.is_ok());
        assert_eq!(result.unwrap(), Version::new(1, 14, 1));
    }

    #[test]
    fn test_version_meets_minimum() {
        let result = parse_and_validate_version("v1.14.1");
        assert!(result.is_ok());
    }

    #[test]
    fn test_version_exceeds_minimum() {
        let result = parse_and_validate_version("v1.15.0");
        assert!(result.is_ok());
    }

    #[test]
    fn test_version_below_minimum() {
        let result = parse_and_validate_version("v1.10.0");
        assert!(result.is_err());
        assert!(result.unwrap_err().to_string().contains("too old"));
    }

    #[test]
    fn test_version_exact_minimum() {
        let result = parse_and_validate_version("v1.14.1");
        assert!(result.is_ok());
    }

    #[test]
    fn test_invalid_version_format() {
        let result = parse_and_validate_version("invalid");
        assert!(result.is_err());
    }

    #[test]
    fn test_parse_version_with_multiline_output() {
        // Real Firecracker output includes exit messages after version
        let output = "Firecracker v1.14.1\n\n2026-02-23T21:05:34.876998360 [anonymous-instance:main] Firecracker exiting successfully. exit_code=0\n";
        let result = parse_and_validate_version(output);
        assert!(result.is_ok());
        assert_eq!(result.unwrap(), Version::new(1, 14, 1));
    }

    #[test]
    fn test_parse_version_with_extra_lines() {
        let output = "v1.15.0\nSome other output\nMore lines here";
        let result = parse_and_validate_version(output);
        assert!(result.is_ok());
        assert_eq!(result.unwrap(), Version::new(1, 15, 0));
    }

    #[test]
    fn test_parse_version_single_line_still_works() {
        let result = parse_and_validate_version("v1.14.1");
        assert!(result.is_ok());
        assert_eq!(result.unwrap(), Version::new(1, 14, 1));
    }
}
