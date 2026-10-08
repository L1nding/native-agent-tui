//! Execution compatibility for the one backend release verified by this client.
use serde_json::Value;
use thiserror::Error;

pub const SUPPORTED_CODEX_VERSION: &str = "0.161.0";

#[derive(Debug, Clone, Copy, PartialEq, Eq, Error)]
pub enum CompatibilityError {
    #[error(
        "Unsupported Codex version; execution requires codex-cli 0.161.0. No task was started."
    )]
    Version,
    #[error(
        "Codex initialize response does not match the supported protocol; no task was started."
    )]
    Initialize,
    #[error("Codex thread/start response does not confirm the supported release and a valid thread identity; no task was started.")]
    ThreadStart,
}

pub(crate) fn verify_version(output: &[u8]) -> Result<(), CompatibilityError> {
    // Never copy an untrusted version banner into a diagnostic or journal.
    let banner = format!("codex-cli {SUPPORTED_CODEX_VERSION}");
    if output == banner.as_bytes()
        || output.strip_suffix(b"\r\n") == Some(banner.as_bytes())
        || output.strip_suffix(b"\n") == Some(banner.as_bytes())
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
    if result.pointer("/thread/cliVersion").and_then(Value::as_str) != Some(SUPPORTED_CODEX_VERSION)
        || !result
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
    fn execution_accepts_only_the_exact_verified_release_banner() {
        for valid in [
            b"codex-cli 0.161.0".as_slice(),
            b"codex-cli 0.161.0\n",
            b"codex-cli 0.161.0\r\n",
        ] {
            assert_eq!(verify_version(valid), Ok(()));
        }
        for invalid in [
            b"".as_slice(),
            b"codex-cli 0.161.00\n",
            b"codex-cli 0.161.0-dev\n",
            b"codex-cli 0.161.0\nPRIVATE_BANNER",
            b"PRIVATE_BANNER 0.161.0",
            b"\xff",
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
    fn a_launcher_banner_cannot_replace_the_new_threads_reported_release() {
        let transcript: Vec<Value> = include_str!("../tests/fixtures/codex-0.161.0/startup.jsonl")
            .lines()
            .map(|line| serde_json::from_str(line).unwrap())
            .collect();
        let valid = &transcript[1]["result"];
        assert_eq!(verify_thread_start(valid), Ok(()));
        for version in [
            Value::Null,
            json!("0.161.00"),
            json!("PRIVATE_VERSION"),
            json!(159),
        ] {
            let mut result = valid.clone();
            result["thread"]["cliVersion"] = version;
            assert_eq!(
                verify_thread_start(&result),
                Err(CompatibilityError::ThreadStart)
            );
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
