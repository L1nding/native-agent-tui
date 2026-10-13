//! Workspace paths offered while the user types an `@path` reference.
//!
//! Core reads one directory level, read-only and bounded. A query can only name
//! paths below the workspace: absolute paths and `..` are refused, not resolved.

use std::path::{Component, Path};

/// Entries offered at once.
pub const MAX_HINTS: usize = 50;
/// Directory entries examined before giving up on a very large directory.
const MAX_SCANNED: usize = 4000;
const MAX_QUERY_BYTES: usize = 512;

/// Completions for the query the UI last asked about.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct PathHints {
    /// What followed the `@`; hints for another query are stale.
    pub query: String,
    /// Workspace-relative paths with `/` separators; directories end in `/`.
    pub entries: Vec<String>,
}

/// Paths under `root` that complete `query`, directories first, then by name.
pub fn complete(root: &Path, query: &str) -> PathHints {
    let mut hints = PathHints {
        query: query.to_owned(),
        entries: Vec::new(),
    };
    let normalized = query.replace('\\', "/");
    let (directory, prefix) = match normalized.rsplit_once('/') {
        Some((directory, prefix)) => (directory, prefix),
        None => ("", normalized.as_str()),
    };
    if query.len() > MAX_QUERY_BYTES
        || normalized.starts_with('/')
        || Path::new(directory)
            .components()
            .any(|part| !matches!(part, Component::Normal(_)))
    {
        return hints;
    }
    let Ok(listing) = std::fs::read_dir(root.join(directory)) else {
        return hints;
    };
    let folded = prefix.to_lowercase();
    let mut found: Vec<(bool, String)> = listing
        .take(MAX_SCANNED)
        .filter_map(Result::ok)
        .filter_map(|entry| {
            let name = entry.file_name().into_string().ok()?;
            // Hidden entries appear only when the user starts typing the dot.
            if (name.starts_with('.') && !prefix.starts_with('.'))
                || !name.to_lowercase().starts_with(&folded)
                || name.chars().any(char::is_control)
            {
                return None;
            }
            let is_dir = entry.file_type().ok()?.is_dir();
            Some((is_dir, name))
        })
        .collect();
    found.sort_by(|a, b| b.0.cmp(&a.0).then_with(|| a.1.cmp(&b.1)));
    hints.entries = found
        .into_iter()
        .take(MAX_HINTS)
        .map(|(is_dir, name)| {
            let mut path = if directory.is_empty() {
                name
            } else {
                format!("{directory}/{name}")
            };
            if is_dir {
                path.push('/');
            }
            path
        })
        .collect();
    hints
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn completion_lists_one_level_inside_the_workspace_and_refuses_to_leave_it() {
        let root = std::env::temp_dir().join(format!(
            "path-hints-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        std::fs::create_dir_all(root.join("src/ui")).unwrap();
        std::fs::create_dir_all(root.join(".git")).unwrap();
        for file in [
            "Cargo.toml",
            "src/lib.rs",
            "src/Layout.rs",
            "src/ui/view.rs",
            ".gitignore",
            "说明.md",
        ] {
            std::fs::write(root.join(file), "").unwrap();
        }
        let entries = |query: &str| complete(&root, query).entries;
        assert_eq!(entries(""), ["src/", "Cargo.toml", "说明.md"]);
        assert_eq!(entries("."), [".git/", ".gitignore"]);
        assert_eq!(entries("s"), ["src/"]);
        assert_eq!(entries("src/"), ["src/ui/", "src/Layout.rs", "src/lib.rs"]);
        assert_eq!(entries("src/l"), ["src/Layout.rs", "src/lib.rs"]);
        assert_eq!(entries("src\\ui\\v"), ["src/ui/view.rs"]);
        assert_eq!(entries("说"), ["说明.md"]);
        assert_eq!(complete(&root, "src/l").query, "src/l");
        for outside in [
            "../",
            "src/../../",
            "/etc/",
            "C:/Windows/",
            "missing/x",
            &"x".repeat(600),
        ] {
            assert!(entries(outside).is_empty(), "{outside}");
        }
        std::fs::remove_dir_all(root).unwrap();
    }
}
