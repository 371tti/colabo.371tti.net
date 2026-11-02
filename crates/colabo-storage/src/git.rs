use std::{
    collections::{BTreeMap, BTreeSet},
    fs,
    path::{Path, PathBuf},
};

use chrono::{DateTime, Utc};
use colabo_domain::{CommitChangeKind, CommitFileChange, CommitInfo};
use gix::{Repository, object::tree::EntryKind};
use uuid::Uuid;

use crate::StoreError;

pub(super) fn initialize(path: &Path) -> Result<(), StoreError> {
    let repo = gix::init_bare(path).map_err(|error| StoreError::Git(error.to_string()))?;
    fs::write(repo.git_dir().join("HEAD"), "ref: refs/heads/main\n")?;
    Ok(())
}

pub(super) fn checkpoint(
    repo_path: &Path,
    files_root: &Path,
    message: &str,
    author_id: Uuid,
    author_name: &str,
) -> Result<Option<CommitInfo>, StoreError> {
    let mut repo = gix::open(repo_path).map_err(|error| StoreError::Git(error.to_string()))?;
    let root_tree = repo
        .head_tree_id_or_empty()
        .map_err(|error| StoreError::Git(error.to_string()))?
        .detach();
    let mut tree = repo
        .edit_tree(root_tree)
        .map_err(|error| StoreError::Git(error.to_string()))?;
    let mut previous_paths = Vec::new();
    collect_tree_paths(&repo, root_tree, &mut PathBuf::new(), &mut previous_paths)?;
    for path in previous_paths {
        tree.remove(path.as_str())
            .map_err(|error| StoreError::Git(error.to_string()))?;
    }
    let mut files = Vec::new();
    collect_worktree_files(files_root, files_root, &mut files)?;
    for (relative, bytes) in files {
        let blob = repo
            .write_blob(bytes)
            .map_err(|error| StoreError::Git(error.to_string()))?
            .detach();
        tree.upsert(relative.as_str(), EntryKind::Blob, blob)
            .map_err(|error| StoreError::Git(error.to_string()))?;
    }
    let tree_id = tree
        .write()
        .map_err(|error| StoreError::Git(error.to_string()))?
        .detach();
    if tree_id == root_tree {
        return Ok(None);
    }

    let email = format!("{}@users.noreply.colabo.invalid", author_id);
    let display_name = clean_signature(author_name);
    {
        let mut config = repo.config_snapshot_mut();
        config
            .set_raw_value("user.name", display_name.as_str())
            .map_err(|error| StoreError::Git(error.to_string()))?;
        config
            .set_raw_value("user.email", email.as_str())
            .map_err(|error| StoreError::Git(error.to_string()))?;
    }
    repo.committer_or_set_fallback(display_name.as_str(), email.as_str())
        .map_err(|error| StoreError::Git(error.to_string()))?;
    let parents = if repo
        .head()
        .map_err(|error| StoreError::Git(error.to_string()))?
        .is_unborn()
    {
        Vec::new()
    } else {
        vec![
            repo.head_id()
                .map_err(|error| StoreError::Git(error.to_string()))?
                .detach(),
        ]
    };
    let commit_id = repo
        .commit("refs/heads/main", message, tree_id, parents)
        .map_err(|error| StoreError::Git(error.to_string()))?
        .to_string();
    let id = repo
        .rev_parse_single(commit_id.as_bytes())
        .map_err(|error| StoreError::Git(error.to_string()))?
        .detach();
    Ok(Some(commit_info(&repo, id)?))
}

pub(super) fn history(repo_path: &Path, limit: usize) -> Result<Vec<CommitInfo>, StoreError> {
    let repo = gix::open(repo_path).map_err(|error| StoreError::Git(error.to_string()))?;
    let head = repo
        .head()
        .map_err(|error| StoreError::Git(error.to_string()))?;
    if head.is_unborn() {
        return Ok(Vec::new());
    }
    drop(head);

    let mut current = repo
        .head_id()
        .map_err(|error| StoreError::Git(error.to_string()))?
        .detach();
    let mut entries = Vec::new();
    for _ in 0..limit.min(500) {
        let commit = repo
            .find_commit(current)
            .map_err(|error| StoreError::Git(error.to_string()))?;
        entries.push(commit_info(&repo, current)?);
        let Some(parent) = commit.parent_ids().next() else {
            break;
        };
        current = parent.detach();
    }
    Ok(entries)
}

pub(super) fn changes(
    repo_path: &Path,
    commit_id: &str,
) -> Result<Vec<CommitFileChange>, StoreError> {
    if !matches!(commit_id.len(), 40 | 64)
        || !commit_id.bytes().all(|byte| byte.is_ascii_hexdigit())
    {
        return Err(StoreError::InvalidPath);
    }
    let repo = gix::open(repo_path).map_err(|error| StoreError::Git(error.to_string()))?;
    let id = repo
        .rev_parse_single(commit_id.as_bytes())
        .map_err(|error| StoreError::Git(error.to_string()))?
        .detach();
    let commit = repo
        .find_commit(id)
        .map_err(|error| StoreError::Git(error.to_string()))?;
    let after_tree = commit
        .tree_id()
        .map_err(|error| StoreError::Git(error.to_string()))?
        .detach();
    let after = tree_blobs(&repo, after_tree)?;
    let before = if let Some(parent) = commit.parent_ids().next() {
        let parent = repo
            .find_commit(parent.detach())
            .map_err(|error| StoreError::Git(error.to_string()))?;
        let tree = parent
            .tree_id()
            .map_err(|error| StoreError::Git(error.to_string()))?
            .detach();
        tree_blobs(&repo, tree)?
    } else {
        BTreeMap::new()
    };

    let paths: BTreeSet<String> = before.keys().chain(after.keys()).cloned().collect();
    Ok(paths
        .into_iter()
        .filter_map(|path| match (before.get(&path), after.get(&path)) {
            (None, Some(_)) => Some(CommitFileChange {
                path,
                kind: CommitChangeKind::Added,
            }),
            (Some(_), None) => Some(CommitFileChange {
                path,
                kind: CommitChangeKind::Deleted,
            }),
            (Some(previous), Some(current)) if previous != current => Some(CommitFileChange {
                path,
                kind: CommitChangeKind::Modified,
            }),
            _ => None,
        })
        .collect())
}

fn commit_info(repo: &Repository, id: gix::ObjectId) -> Result<CommitInfo, StoreError> {
    let commit = repo
        .find_commit(id)
        .map_err(|error| StoreError::Git(error.to_string()))?;
    let message = commit
        .message_raw()
        .map_err(|error| StoreError::Git(error.to_string()))?;
    let author = commit
        .author()
        .map_err(|error| StoreError::Git(error.to_string()))?;
    let timestamp = commit
        .time()
        .map_err(|error| StoreError::Git(error.to_string()))?
        .seconds;
    let parent_id = commit.parent_ids().next().map(|parent| parent.to_string());
    Ok(CommitInfo {
        id: id.to_string(),
        message: String::from_utf8_lossy(message.as_ref())
            .lines()
            .next()
            .unwrap_or_default()
            .to_owned(),
        author: String::from_utf8_lossy(author.name.as_ref())
            .trim()
            .to_owned(),
        committed_at: DateTime::<Utc>::from_timestamp(timestamp, 0)
            .unwrap_or(DateTime::<Utc>::UNIX_EPOCH),
        parent_id,
    })
}

fn tree_blobs(
    repo: &Repository,
    tree_id: gix::ObjectId,
) -> Result<BTreeMap<String, String>, StoreError> {
    let mut blobs = BTreeMap::new();
    collect_tree_blobs(repo, tree_id, &mut PathBuf::new(), &mut blobs)?;
    Ok(blobs)
}

fn collect_tree_blobs(
    repo: &Repository,
    tree_id: gix::ObjectId,
    prefix: &mut PathBuf,
    output: &mut BTreeMap<String, String>,
) -> Result<(), StoreError> {
    let tree = repo
        .find_tree(tree_id)
        .map_err(|error| StoreError::Git(error.to_string()))?;
    for entry in tree.iter() {
        let entry = entry.map_err(|error| StoreError::Git(error.to_string()))?;
        let name = String::from_utf8_lossy(entry.inner.filename.as_ref()).into_owned();
        if entry.inner.mode.is_tree() {
            prefix.push(&name);
            collect_tree_blobs(repo, entry.inner.oid.to_owned(), prefix, output)?;
            prefix.pop();
        } else if entry.inner.mode.kind() == EntryKind::Blob {
            let path = prefix.join(&name).to_string_lossy().replace('\\', "/");
            let path = colabo_domain::validate_relative_path(&path, false)
                .map_err(|_| StoreError::InvalidPath)?;
            output.insert(path, entry.inner.oid.to_string());
        } else {
            return Err(StoreError::InvalidPath);
        }
    }
    Ok(())
}

pub(super) fn read_file_at_commit(
    repo_path: &Path,
    commit_id: &str,
    path: &str,
) -> Result<Vec<u8>, StoreError> {
    let files = files_at_commit(repo_path, commit_id)?;
    files
        .into_iter()
        .find_map(|(file, bytes)| (file == path).then_some(bytes))
        .ok_or(StoreError::FileMissing)
}

pub(super) fn files_at_commit(
    repo_path: &Path,
    commit_id: &str,
) -> Result<Vec<(String, Vec<u8>)>, StoreError> {
    if !matches!(commit_id.len(), 40 | 64)
        || !commit_id.bytes().all(|byte| byte.is_ascii_hexdigit())
    {
        return Err(StoreError::InvalidPath);
    }
    let repo = gix::open(repo_path).map_err(|error| StoreError::Git(error.to_string()))?;
    let id = repo
        .rev_parse_single(commit_id.as_bytes())
        .map_err(|error| StoreError::Git(error.to_string()))?;
    let commit = repo
        .find_commit(id.detach())
        .map_err(|error| StoreError::Git(error.to_string()))?;
    let tree_id = commit
        .tree_id()
        .map_err(|error| StoreError::Git(error.to_string()))?
        .detach();
    let mut files = Vec::new();
    collect_tree_files(&repo, tree_id, &mut PathBuf::new(), &mut files)?;
    Ok(files)
}

fn collect_worktree_files(
    root: &Path,
    directory: &Path,
    output: &mut Vec<(String, Vec<u8>)>,
) -> Result<(), StoreError> {
    for entry in fs::read_dir(directory)? {
        let entry = entry?;
        let file_type = entry.file_type()?;
        if file_type.is_symlink() {
            return Err(StoreError::Symlink);
        }
        let path = entry.path();
        if file_type.is_dir() {
            collect_worktree_files(root, &path, output)?;
        } else if file_type.is_file() {
            let relative = path
                .strip_prefix(root)
                .map_err(|_| StoreError::InvalidPath)?
                .components()
                .map(|component| component.as_os_str().to_string_lossy())
                .collect::<Vec<_>>()
                .join("/");
            output.push((relative, fs::read(path)?));
        }
    }
    Ok(())
}

fn collect_tree_files(
    repo: &Repository,
    tree_id: gix::ObjectId,
    prefix: &mut PathBuf,
    output: &mut Vec<(String, Vec<u8>)>,
) -> Result<(), StoreError> {
    let tree = repo
        .find_tree(tree_id)
        .map_err(|error| StoreError::Git(error.to_string()))?;
    for entry in tree.iter() {
        let entry = entry.map_err(|error| StoreError::Git(error.to_string()))?;
        let name = String::from_utf8_lossy(entry.inner.filename.as_ref()).into_owned();
        let relative = colabo_domain::validate_relative_path(
            &prefix.join(&name).to_string_lossy().replace('\\', "/"),
            false,
        )
        .map_err(|_| StoreError::InvalidPath)?;
        if entry.inner.mode.is_tree() {
            prefix.push(&name);
            collect_tree_files(repo, entry.inner.oid.to_owned(), prefix, output)?;
            prefix.pop();
        } else if entry.inner.mode.kind() == EntryKind::Blob {
            let blob = repo
                .find_blob(entry.inner.oid.to_owned())
                .map_err(|error| StoreError::Git(error.to_string()))?;
            output.push((relative, blob.data.to_vec()));
        } else {
            return Err(StoreError::InvalidPath);
        }
    }
    Ok(())
}

fn collect_tree_paths(
    repo: &Repository,
    tree_id: gix::ObjectId,
    prefix: &mut PathBuf,
    output: &mut Vec<String>,
) -> Result<(), StoreError> {
    let tree = repo
        .find_tree(tree_id)
        .map_err(|error| StoreError::Git(error.to_string()))?;
    for entry in tree.iter() {
        let entry = entry.map_err(|error| StoreError::Git(error.to_string()))?;
        let name = String::from_utf8_lossy(entry.inner.filename.as_ref()).into_owned();
        if entry.inner.mode.is_tree() {
            prefix.push(&name);
            collect_tree_paths(repo, entry.inner.oid.to_owned(), prefix, output)?;
            prefix.pop();
        } else if entry.inner.mode.kind() == EntryKind::Blob {
            let relative = prefix.join(&name).to_string_lossy().replace('\\', "/");
            let relative = colabo_domain::validate_relative_path(&relative, false)
                .map_err(|_| StoreError::InvalidPath)?;
            output.push(relative);
        } else {
            return Err(StoreError::InvalidPath);
        }
    }
    Ok(())
}

fn clean_signature(name: &str) -> String {
    let clean: String = name
        .chars()
        .filter(|ch| !ch.is_control())
        .take(80)
        .collect();
    if clean.trim().is_empty() {
        "Colabo user".into()
    } else {
        clean.trim().to_owned()
    }
}
