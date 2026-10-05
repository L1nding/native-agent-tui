//! Bounded, live file-change context owned by Core. No filesystem reads.
use std::collections::VecDeque;

use crate::protocol::{ObservedFileChangeKind, ObservedFileChangeSnapshot};

use super::{FilePreview, RequestKind, RequestView};

const PREVIEW_BYTES: usize = 32 * 1024;
const CACHE_BYTES: usize = 256 * 1024;
const CACHE_ITEMS: usize = 64;

struct Entry {
    thread: String,
    turn: String,
    item: String,
    preview: FilePreview,
}

impl Entry {
    fn bytes(&self) -> usize {
        self.thread.len() + self.turn.len() + self.item.len() + self.preview.text.len()
    }
}

#[derive(Default)]
pub(crate) struct FilePreviews {
    entries: VecDeque<Entry>,
}

impl FilePreviews {
    /// 仅接收协议层完整校验后的类型化快照。
    pub fn observe(
        &mut self,
        snapshot: &ObservedFileChangeSnapshot<'_>,
        seq: u64,
        requests: &mut [RequestView],
    ) {
        let preview = decode(snapshot, seq);
        self.store(
            snapshot.thread_id,
            snapshot.turn_id,
            snapshot.item_id,
            preview,
            requests,
        );
    }

    pub fn attach(&self, request: &mut RequestView) {
        if !matches!(request.kind, RequestKind::FileApproval) {
            return;
        }
        request.details.file_preview = self
            .entries
            .iter()
            .find(|entry| {
                entry.thread == request.thread_id
                    && entry.turn == request.turn_id
                    && Some(entry.item.as_str()) == request.details.item_id.as_deref()
            })
            .map(|entry| entry.preview.clone());
    }

    fn store(
        &mut self,
        thread: &str,
        turn: &str,
        item: &str,
        preview: FilePreview,
        requests: &mut [RequestView],
    ) {
        for request in requests {
            if matches!(request.kind, RequestKind::FileApproval)
                && request.thread_id == thread
                && request.turn_id == turn
                && request.details.item_id.as_deref() == Some(item)
            {
                request.details.file_preview = Some(preview.clone());
            }
        }
        self.entries
            .retain(|entry| entry.thread != thread || entry.turn != turn || entry.item != item);
        let entry = Entry {
            thread: thread.into(),
            turn: turn.into(),
            item: item.into(),
            preview,
        };
        while self.entries.len() >= CACHE_ITEMS
            || self.entries.iter().map(Entry::bytes).sum::<usize>() + entry.bytes() > CACHE_BYTES
        {
            self.entries.pop_front();
        }
        self.entries.push_back(entry);
    }

    pub fn retire(&mut self, thread: &str, turn: &str) {
        self.entries
            .retain(|entry| entry.thread != thread || entry.turn != turn);
    }
}

fn append(text: &mut String, value: &str, truncated: &mut bool) {
    let room = PREVIEW_BYTES.saturating_sub(text.len());
    let mut end = value.len().min(room);
    while !value.is_char_boundary(end) {
        end -= 1;
    }
    text.push_str(&value[..end]);
    *truncated |= end < value.len();
}

fn decode(snapshot: &ObservedFileChangeSnapshot<'_>, seq: u64) -> FilePreview {
    let mut preview = FilePreview {
        text: String::new(),
        source_seq: seq,
        truncated: false,
        unavailable: false,
    };
    let Some(changes) = snapshot.changes.as_ref() else {
        preview.unavailable = true;
        return preview;
    };
    for change in changes {
        let kind = match change.kind {
            ObservedFileChangeKind::Add => "add",
            ObservedFileChangeKind::Delete => "delete",
            ObservedFileChangeKind::Update => "update",
        };
        for part in [kind, " ", change.path, "\n"] {
            append(&mut preview.text, part, &mut preview.truncated);
        }
        if let Some(path) = change.move_path {
            append(&mut preview.text, "Move to: ", &mut preview.truncated);
            append(&mut preview.text, path, &mut preview.truncated);
            append(&mut preview.text, "\n", &mut preview.truncated);
        }
        append(&mut preview.text, change.diff, &mut preview.truncated);
        append(&mut preview.text, "\n", &mut preview.truncated);
    }
    preview
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::protocol::RpcId;
    use serde_json::json;

    #[test]
    fn previews_are_bounded_and_bound_to_thread_turn_and_item() {
        let mut cache = FilePreviews::default();
        for n in 0..90 {
            let event = json!({"threadId":"root","turnId":"one","item":{"id":n.to_string(),"type":"fileChange","changes":[{"path":"中文.rs","kind":{"type":"update","move_path":"new.rs"},"diff":"变更".repeat(20000)}]}});
            let snapshot = crate::protocol::decode_file_change_snapshot("item/started", &event)
                .unwrap()
                .unwrap();
            cache.observe(&snapshot, n, &mut []);
        }
        assert!(cache.entries.len() <= CACHE_ITEMS);
        assert!(cache.entries.iter().map(Entry::bytes).sum::<usize>() <= CACHE_BYTES);
        let mut request = RequestView::decode(
            RpcId::Number(7),
            "item/fileChange/requestApproval",
            &json!({"threadId":"root","turnId":"one","itemId":"89"}),
        )
        .unwrap();
        cache.attach(&mut request);
        let preview = request.details.file_preview.as_ref().unwrap();
        assert!(preview.truncated);
        assert!(preview.text.starts_with("update 中文.rs\nMove to: new.rs"));
        assert!(preview.text.len() <= PREVIEW_BYTES);
        request.turn_id = "two".into();
        cache.attach(&mut request);
        assert!(request.details.file_preview.is_none());
        cache.retire("root", "one");
        assert!(cache.entries.is_empty());
    }

    #[test]
    fn patch_updates_reject_malformed_move_paths_without_replacing_cached_preview() {
        let mut cache = FilePreviews::default();
        let mut requests = [RequestView::decode(
            RpcId::Number(8),
            "item/fileChange/requestApproval",
            &json!({"threadId":"root","turnId":"one","itemId":"file-1"}),
        )
        .unwrap()];
        let started = json!({"threadId":"root","turnId":"one","item":{"id":"file-1","type":"fileChange","changes":[{"path":"old.rs","kind":{"type":"update"},"diff":"old"}]}});
        let snapshot = crate::protocol::decode_file_change_snapshot("item/started", &started)
            .unwrap()
            .unwrap();
        cache.observe(&snapshot, 1, &mut requests);
        cache.attach(&mut requests[0]);
        let old_preview = requests[0].details.file_preview.clone().unwrap();

        let malformed = json!({"threadId":"root","turnId":"one","itemId":"file-1","changes":[{"path":"new.rs","kind":{"type":"update","move_path":42},"diff":"new"}]});
        assert!(crate::protocol::decode_file_change_snapshot(
            "item/fileChange/patchUpdated",
            &malformed
        )
        .unwrap()
        .is_err());
        cache.attach(&mut requests[0]);
        assert_eq!(requests[0].details.file_preview, Some(old_preview.clone()));
        assert_eq!(cache.entries[0].preview, old_preview);

        let updated = json!({"threadId":"root","turnId":"one","itemId":"file-1","changes":[{"path":"new.rs","kind":{"type":"update","move_path":null},"diff":"new"}]});
        let snapshot =
            crate::protocol::decode_file_change_snapshot("item/fileChange/patchUpdated", &updated)
                .unwrap()
                .unwrap();
        cache.observe(&snapshot, 3, &mut requests);
        cache.attach(&mut requests[0]);
        let updated = requests[0].details.file_preview.as_ref().unwrap();
        assert_eq!(updated.source_seq, 3);
        assert!(updated.text.contains("new"));
    }
}
