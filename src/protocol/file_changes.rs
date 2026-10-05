use super::ProtocolError;
use serde_json::Value;
use std::hash::{BuildHasher, Hash, RandomState};
use std::sync::OnceLock;

/// 完整有效快照的定长内容指纹，不保存原文。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct FileChangeFingerprint([u64; 2]);

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum FileChangeSnapshotSource {
    ItemStarted,
    ItemCompleted,
    PatchUpdated,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub(crate) enum ObservedFileChangeKind {
    Add,
    Delete,
    Update,
}

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub(crate) struct ObservedFileChange<'a> {
    pub path: &'a str,
    pub kind: ObservedFileChangeKind,
    pub move_path: Option<&'a str>,
    pub diff: &'a str,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct ObservedFileChangeSnapshot<'a> {
    pub source: FileChangeSnapshotSource,
    pub thread_id: &'a str,
    pub turn_id: &'a str,
    pub item_id: &'a str,
    pub changes: Option<Vec<ObservedFileChange<'a>>>,
    pub fingerprint: Option<FileChangeFingerprint>,
}

/// 无关通知返回 `None`；缺少 `changes` 与空快照分开，避免错误建立基线。
/// patchUpdated 缺少 `changes` 时忽略，保留当前预览。
pub(crate) fn decode_file_change_snapshot<'a>(
    method: &str,
    params: &'a Value,
) -> Option<Result<ObservedFileChangeSnapshot<'a>, ProtocolError>> {
    let (item, item_id, source) = match method {
        "item/started" => {
            let item = params.get("item")?;
            if item.get("type")?.as_str()? != "fileChange" {
                return None;
            }
            (item, item.get("id"), FileChangeSnapshotSource::ItemStarted)
        }
        "item/completed" => {
            let item = params.get("item")?;
            if item.get("type")?.as_str()? != "fileChange" {
                return None;
            }
            (
                item,
                item.get("id"),
                FileChangeSnapshotSource::ItemCompleted,
            )
        }
        "item/fileChange/patchUpdated" => {
            params.get("changes")?;
            (
                params,
                params.get("itemId"),
                FileChangeSnapshotSource::PatchUpdated,
            )
        }
        _ => return None,
    };

    Some((|| {
        fn identity<'a>(
            value: Option<&'a Value>,
            name: &'static str,
        ) -> Result<&'a str, ProtocolError> {
            value
                .and_then(Value::as_str)
                .filter(|value| !value.is_empty() && value.len() <= 1024)
                .ok_or(ProtocolError::InvalidEnvelope(name))
        }
        let thread_id = identity(
            params.get("threadId"),
            "invalid file change thread identity",
        )?;
        let turn_id = identity(params.get("turnId"), "invalid file change turn identity")?;
        let item_id = identity(item_id, "invalid file change item identity")?;

        let changes = item
            .get("changes")
            .map(|changes| {
                let changes = changes.as_array().ok_or(ProtocolError::InvalidEnvelope(
                    "file changes must be an array",
                ))?;
                let mut observed = Vec::with_capacity(changes.len());
                for change in changes {
                    let path = change
                        .get("path")
                        .and_then(Value::as_str)
                        .filter(|path| !path.is_empty() && path.len() <= 4096)
                        .ok_or(ProtocolError::InvalidEnvelope("invalid file change path"))?;
                    let diff = change
                        .get("diff")
                        .and_then(Value::as_str)
                        .ok_or(ProtocolError::InvalidEnvelope("invalid file change diff"))?;
                    let kind = match change.pointer("/kind/type").and_then(Value::as_str) {
                        Some("add") => ObservedFileChangeKind::Add,
                        Some("delete") => ObservedFileChangeKind::Delete,
                        Some("update") => ObservedFileChangeKind::Update,
                        _ => {
                            return Err(ProtocolError::InvalidEnvelope("invalid file change kind"))
                        }
                    };
                    let move_path = match change.pointer("/kind/move_path") {
                        None | Some(Value::Null) => None,
                        Some(value) => Some(
                            value
                                .as_str()
                                .filter(|path| !path.is_empty() && path.len() <= 4096)
                                .ok_or(ProtocolError::InvalidEnvelope("invalid file move path"))?,
                        ),
                    };
                    observed.push(ObservedFileChange {
                        path,
                        kind,
                        move_path,
                        diff,
                    });
                }
                Ok(observed)
            })
            .transpose()?;
        let fingerprint = changes.as_deref().map(fingerprint);
        Ok(ObservedFileChangeSnapshot {
            source,
            thread_id,
            turn_id,
            item_id,
            changes,
            fingerprint,
        })
    })())
}

fn fingerprint(changes: &[ObservedFileChange<'_>]) -> FileChangeFingerprint {
    static HASHERS: OnceLock<[RandomState; 2]> = OnceLock::new();
    let hashers = HASHERS.get_or_init(|| [RandomState::new(), RandomState::new()]);
    let mut values = [0; 2];
    for (value, hasher) in values.iter_mut().zip(hashers) {
        *value = hasher.hash_one(changes);
    }
    FileChangeFingerprint(values)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn decode<'a>(method: &str, params: &'a Value) -> Option<ObservedFileChangeSnapshot<'a>> {
        decode_file_change_snapshot(method, params)?.ok()
    }

    #[test]
    fn distinguishes_missing_empty_duplicate_and_full_snapshot_changes() {
        let missing = json!({"threadId":"t","turnId":"u","itemId":"i"});
        assert!(decode_file_change_snapshot("item/fileChange/patchUpdated", &missing).is_none());
        let empty = json!({"threadId":"t","turnId":"u","itemId":"i","changes":[]});
        let empty_snapshot = decode("item/fileChange/patchUpdated", &empty).unwrap();
        assert!(empty_snapshot.changes.as_ref().unwrap().is_empty());
        assert_eq!(
            empty_snapshot.fingerprint,
            decode("item/fileChange/patchUpdated", &empty)
                .unwrap()
                .fingerprint
        );

        let one = json!({"threadId":"t","turnId":"u","itemId":"i","changes":[{"path":"a","kind":{"type":"add"},"diff":"+a"}]});
        let two = json!({"threadId":"t","turnId":"u","itemId":"i","changes":[{"path":"a","kind":{"type":"add"},"diff":"+a"},{"path":"b","kind":{"type":"add"},"diff":"+b"}]});
        let removed = json!({"threadId":"t","turnId":"u","itemId":"i","changes":[{"path":"a","kind":{"type":"add"},"diff":"+a"}]});
        assert_ne!(
            empty_snapshot.fingerprint,
            decode("item/fileChange/patchUpdated", &one)
                .unwrap()
                .fingerprint
        );
        assert_ne!(
            decode("item/fileChange/patchUpdated", &one)
                .unwrap()
                .fingerprint,
            decode("item/fileChange/patchUpdated", &two)
                .unwrap()
                .fingerprint
        );
        assert_eq!(
            decode("item/fileChange/patchUpdated", &one)
                .unwrap()
                .fingerprint,
            decode("item/fileChange/patchUpdated", &removed)
                .unwrap()
                .fingerprint
        );
    }

    #[test]
    fn validates_before_exposing_a_snapshot_and_decodes_started_file_items() {
        let started = json!({"threadId":"t","turnId":"u","item":{"id":"i","type":"fileChange","changes":[{"path":"a","kind":{"type":"update","move_path":null},"diff":"x"}]}});
        assert_eq!(
            decode("item/started", &started)
                .unwrap()
                .changes
                .unwrap()
                .len(),
            1
        );
        for malformed in [
            json!({"threadId":"t","turnId":"u","itemId":"i","changes":{}}),
            json!({"threadId":"t","turnId":"u","itemId":"i","changes":[{"path":"a","kind":{"type":"update","move_path":42},"diff":"x"}]}),
        ] {
            assert!(matches!(
                decode_file_change_snapshot("item/fileChange/patchUpdated", &malformed),
                Some(Err(_))
            ));
        }
    }
}
