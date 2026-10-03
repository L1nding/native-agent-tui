//! Bounded, live file-change context owned by Core. No filesystem reads.
use std::collections::VecDeque;

use serde_json::Value;

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
    /// Only called after Core has validated ownership and current turn identity.
    pub fn observe(&mut self, params: &Value, seq: u64, requests: &mut [RequestView]) {
        let item = &params["item"];
        if item["type"] != "fileChange" {
            return;
        }
        let (Some(thread), Some(turn), Some(id)) = (
            params["threadId"].as_str(),
            params["turnId"].as_str(),
            item["id"].as_str(),
        ) else {
            return;
        };
        if [thread, turn, id]
            .iter()
            .any(|id| id.is_empty() || id.len() > 1024)
        {
            return;
        }
        let preview = decode(item, seq);
        for request in requests {
            if matches!(request.kind, RequestKind::FileApproval)
                && request.thread_id == thread
                && request.turn_id == turn
                && request.details.item_id.as_deref() == Some(id)
            {
                request.details.file_preview = Some(preview.clone());
            }
        }
        self.entries
            .retain(|entry| entry.thread != thread || entry.turn != turn || entry.item != id);
        let entry = Entry {
            thread: thread.into(),
            turn: turn.into(),
            item: id.into(),
            preview,
        };
        while self.entries.len() >= CACHE_ITEMS
            || self.entries.iter().map(Entry::bytes).sum::<usize>() + entry.bytes() > CACHE_BYTES
        {
            self.entries.pop_front();
        }
        self.entries.push_back(entry);
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

fn decode(item: &Value, seq: u64) -> FilePreview {
    let mut preview = FilePreview {
        text: String::new(),
        source_seq: seq,
        truncated: false,
        unavailable: false,
    };
    let Some(changes) = item["changes"].as_array() else {
        preview.unavailable = true;
        return preview;
    };
    for change in changes {
        let (Some(path), Some(diff), Some(kind)) = (
            change["path"].as_str(),
            change["diff"].as_str(),
            change.pointer("/kind/type").and_then(Value::as_str),
        ) else {
            preview.unavailable = true;
            break;
        };
        if !matches!(kind, "add" | "delete" | "update") {
            preview.unavailable = true;
            break;
        }
        for part in [kind, " ", path, "\n"] {
            append(&mut preview.text, part, &mut preview.truncated);
        }
        if let Some(path) = change.pointer("/kind/move_path").and_then(Value::as_str) {
            append(&mut preview.text, "Move to: ", &mut preview.truncated);
            append(&mut preview.text, path, &mut preview.truncated);
            append(&mut preview.text, "\n", &mut preview.truncated);
        }
        append(&mut preview.text, diff, &mut preview.truncated);
        append(&mut preview.text, "\n", &mut preview.truncated);
    }
    if preview.unavailable {
        preview.text.clear(); // Do not present a partial malformed event as a complete diff.
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
            cache.observe(&json!({"threadId":"root","turnId":"one","item":{"id":n.to_string(),"type":"fileChange","changes":[{"path":"中文.rs","kind":{"type":"update","move_path":"new.rs"},"diff":"变更".repeat(20000)}]}}), n, &mut []);
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
}
