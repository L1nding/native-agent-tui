//! Execution compatibility checks for the Codex backend; no release is pinned.
use serde_json::Value;
use thiserror::Error;

/// The release the protocol fixtures were recorded against; informational only.
pub const VERIFIED_CODEX_VERSION: &str = "0.161.0";

const VERSION_BYTES: usize = 64;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Error)]
pub enum CompatibilityError {
    #[error("Codex version query did not return a codex-cli version banner; no task was started.")]
    Version,
    #[error(
        "Codex initialize response does not match the supported protocol; no task was started."
    )]
    Initialize,
    #[error(
        "Codex thread/start response does not carry a valid thread identity; no task was started."
    )]
    ThreadStart,
}

pub(crate) fn verify_version(output: &[u8]) -> Result<(), CompatibilityError> {
    // Any release is accepted; the banner only has to identify a Codex CLI.
    // Never copy an untrusted version banner into a diagnostic or journal.
    let line = output
        .strip_suffix(b"\r\n")
        .or_else(|| output.strip_suffix(b"\n"))
        .unwrap_or(output);
    let version = line
        .strip_prefix(b"codex-cli ")
        .ok_or(CompatibilityError::Version)?;
    if !version.is_empty()
        && version.len() <= VERSION_BYTES
        && version
            .iter()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'.' | b'-' | b'+'))
    {
        Ok(())
    } else {
        Err(CompatibilityError::Version)
    }
}

pub(crate) fn verify_initialize(result: &Value) -> Result<(), CompatibilityError> {
    // Required fields come from the pinned binary's InitializeResponse schema.
    // They describe the handshake, not an advertisement of every tool capability.
    for (field, limit) in [
        ("userAgent", 1024),
        ("codexHome", 128 * 1024),
        ("platformFamily", 32),
        ("platformOs", 32),
    ] {
        if !result[field]
            .as_str()
            .is_some_and(|value| !value.trim().is_empty() && value.len() <= limit)
        {
            return Err(CompatibilityError::Initialize);
        }
    }
    Ok(())
}

pub(crate) fn verify_thread_start(result: &Value) -> Result<(), CompatibilityError> {
    if !result
        .pointer("/thread/id")
        .and_then(Value::as_str)
        .is_some_and(|id| !id.trim().is_empty() && id.len() <= 1024)
    {
        return Err(CompatibilityError::ThreadStart);
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn execution_accepts_any_release_with_a_codex_cli_banner() {
        for valid in [
            b"codex-cli 0.161.0".as_slice(),
            b"codex-cli 0.161.0\n",
            b"codex-cli 0.161.0\r\n",
            b"codex-cli 0.162.1\n",
            b"codex-cli 0.163.0-alpha.5\n",
            b"codex-cli 1.0.0+build.7\r\n",
        ] {
            assert_eq!(verify_version(valid), Ok(()));
        }
        let oversized = format!("codex-cli {}\n", "1".repeat(VERSION_BYTES + 1));
        for invalid in [
            b"".as_slice(),
            b"codex-cli\n",
            b"codex-cli \n",
            b"codex-cli 0.161.0 PRIVATE_BANNER\n",
            b"codex-cli 0.161.0\nPRIVATE_BANNER",
            b"PRIVATE_BANNER 0.161.0",
            b"\xff",
            oversized.as_bytes(),
        ] {
            assert_eq!(verify_version(invalid), Err(CompatibilityError::Version));
            assert!(!verify_version(invalid)
                .unwrap_err()
                .to_string()
                .contains("PRIVATE_"));
        }
    }

    #[test]
    fn required_initialize_fields_are_checked_without_retaining_private_metadata() {
        let fixture: Value = serde_json::from_str(include_str!(
            "../tests/fixtures/codex-0.161.0/initialize.json"
        ))
        .unwrap();
        assert_eq!(verify_initialize(&fixture["result"]), Ok(()));
        let valid = &fixture["result"];
        for field in ["userAgent", "codexHome", "platformFamily", "platformOs"] {
            let mut missing = valid.clone();
            missing.as_object_mut().unwrap().remove(field);
            assert_eq!(
                verify_initialize(&missing),
                Err(CompatibilityError::Initialize)
            );
            for invalid in [Value::Null, json!(0), json!(true), json!(""), json!(" ")] {
                let mut result = valid.clone();
                result[field] = invalid;
                assert_eq!(
                    verify_initialize(&result),
                    Err(CompatibilityError::Initialize)
                );
            }
        }
        let mut extra = valid.clone();
        extra["futureOptionalField"] = json!("PRIVATE_METADATA");
        assert_eq!(verify_initialize(&extra), Ok(()));
        assert_eq!(
            verify_initialize(&json!({"userAgent":"PRIVATE_METADATA"})),
            Err(CompatibilityError::Initialize)
        );
    }

    #[test]
    fn a_new_thread_needs_a_valid_identity_but_no_particular_release() {
        let transcript: Vec<Value> = include_str!("../tests/fixtures/codex-0.161.0/startup.jsonl")
            .lines()
            .map(|line| serde_json::from_str(line).unwrap())
            .collect();
        let valid = &transcript[1]["result"];
        assert_eq!(verify_thread_start(valid), Ok(()));
        for version in [Value::Null, json!("0.162.1"), json!("PRIVATE_VERSION")] {
            let mut result = valid.clone();
            result["thread"]["cliVersion"] = version;
            assert_eq!(verify_thread_start(&result), Ok(()));
        }
        for id in [Value::Null, json!(""), json!(" "), json!(0)] {
            let mut result = valid.clone();
            result["thread"]["id"] = id;
            assert_eq!(
                verify_thread_start(&result),
                Err(CompatibilityError::ThreadStart)
            );
        }
    }
}
