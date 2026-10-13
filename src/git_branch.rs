//! The workspace's current git branch, read from `.git` without running git.

use std::path::Path;

const MAX_BYTES: u64 = 4096;

fn read(path: &Path) -> Option<String> {
    use std::io::Read;
    let mut text = String::new();
    std::fs::File::open(path)
        .ok()?
        .take(MAX_BYTES)
        .read_to_string(&mut text)
        .ok()?;
    Some(text)
}

/// `main` for a branch, `detached abc1234` for a detached head, `None` when
/// `root` is not the top of a git work tree or its head cannot be read.
pub fn current(root: &Path) -> Option<String> {
    let dot_git = root.join(".git");
    // A linked work tree has a `.git` file pointing at its own git directory.
    let head = if dot_git.is_dir() {
        dot_git.join("HEAD")
    } else {
        let pointer = read(&dot_git)?;
        let directory = pointer.trim().strip_prefix("gitdir:")?.trim();
        root.join(directory).join("HEAD")
    };
    let head = read(&head)?;
    let head = head.trim();
    let name = match head.strip_prefix("ref:") {
        Some(reference) => {
            let reference = reference.trim();
            reference
                .strip_prefix("refs/heads/")
                .unwrap_or(reference)
                .to_owned()
        }
        None if head.len() >= 7 && head.bytes().all(|b| b.is_ascii_hexdigit()) => {
            format!("detached {}", &head[..7])
        }
        None => return None,
    };
    (!name.is_empty() && name.len() <= 200 && !name.chars().any(char::is_control)).then_some(name)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn branch_is_read_from_a_repository_a_linked_work_tree_or_not_at_all() {
        let root = std::env::temp_dir().join(format!(
            "git-branch-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        let repository = root.join("repo");
        std::fs::create_dir_all(repository.join(".git/worktrees/linked")).unwrap();
        std::fs::write(
            repository.join(".git/HEAD"),
            "ref: refs/heads/feat/中文-branch\n",
        )
        .unwrap();
        assert_eq!(current(&repository).as_deref(), Some("feat/中文-branch"));
        std::fs::write(
            repository.join(".git/HEAD"),
            "0123456789abcdef0123456789abcdef01234567\n",
        )
        .unwrap();
        assert_eq!(current(&repository).as_deref(), Some("detached 0123456"));
        std::fs::write(repository.join(".git/HEAD"), "garbage\n").unwrap();
        assert_eq!(current(&repository), None);

        let linked = root.join("linked");
        std::fs::create_dir_all(&linked).unwrap();
        std::fs::write(
            repository.join(".git/worktrees/linked/HEAD"),
            "ref: refs/heads/topic\n",
        )
        .unwrap();
        std::fs::write(
            linked.join(".git"),
            "gitdir: ../repo/.git/worktrees/linked\n",
        )
        .unwrap();
        assert_eq!(current(&linked).as_deref(), Some("topic"));
        assert_eq!(current(&root), None);
        std::fs::remove_dir_all(root).unwrap();
    }
}
