use std::{
    collections::{BTreeMap, HashSet},
    fs, io,
    path::{Path, PathBuf},
};

use colabo_domain::{FileEntry, validate_relative_path};
use collab_core::Snapshot;
use getrandom::fill as random_fill;
use serde::{Deserialize, Serialize};
use tempfile::NamedTempFile;
use uuid::Uuid;

use crate::{Store, StoreError, write_bytes_atomic, write_json_atomic};

#[derive(Clone, Debug)]
pub struct CollaborationFile {
    pub id: Uuid,
    pub path: String,
}

#[derive(Default, Deserialize, Serialize)]
struct FileIndex {
    paths: BTreeMap<String, Uuid>,
}

impl Store {
    pub fn list_directory(
        &self,
        workspace_id: Uuid,
        directory: &str,
    ) -> Result<Vec<FileEntry>, StoreError> {
        let directory =
            validate_relative_path(directory, true).map_err(|_| StoreError::InvalidPath)?;
        let path = checked_path(&self.files_root(workspace_id), &directory, false)?;
        if !fs::metadata(&path)?.is_dir() {
            return Err(StoreError::FileMissing);
        }
        let mut entries = Vec::new();
        for entry in fs::read_dir(path)? {
            let entry = entry?;
            let file_type = entry.file_type()?;
            if file_type.is_symlink() {
                continue;
            }
            let name = entry.file_name().to_string_lossy().into_owned();
            let relative = if directory.is_empty() {
                name.clone()
            } else {
                format!("{directory}/{name}")
            };
            let relative =
                validate_relative_path(&relative, false).map_err(|_| StoreError::InvalidPath)?;
            let size = if file_type.is_file() {
                entry.metadata()?.len()
            } else {
                0
            };
            entries.push(FileEntry {
                path: relative,
                name,
                directory: file_type.is_dir(),
                size,
            });
        }
        entries.sort_by(|a, b| {
            b.directory
                .cmp(&a.directory)
                .then_with(|| a.name.to_lowercase().cmp(&b.name.to_lowercase()))
        });
        Ok(entries)
    }

    pub fn read_file(&self, workspace_id: Uuid, file: &str) -> Result<Vec<u8>, StoreError> {
        let file = validate_relative_path(file, false).map_err(|_| StoreError::InvalidPath)?;
        let path = checked_path(&self.files_root(workspace_id), &file, false)?;
        if !fs::metadata(&path)?.is_file() {
            return Err(StoreError::FileMissing);
        }
        Ok(fs::read(path)?)
    }

    pub fn create_file(
        &self,
        workspace_id: Uuid,
        file: &str,
        bytes: &[u8],
    ) -> Result<(), StoreError> {
        let file = validate_relative_path(file, false).map_err(|_| StoreError::InvalidPath)?;
        let path = checked_path(&self.files_root(workspace_id), &file, true)?;
        if path.exists() {
            return Err(StoreError::AlreadyExists);
        }
        write_bytes_atomic(&path, bytes)?;
        Ok(())
    }

    pub fn create_file_from_path(
        &self,
        workspace_id: Uuid,
        file: &str,
        source: &Path,
    ) -> Result<(), StoreError> {
        let file = validate_relative_path(file, false).map_err(|_| StoreError::InvalidPath)?;
        let root = self.files_root(workspace_id);
        create_parent_directories(&root, &file)?;
        let path = checked_path(&root, &file, true)?;
        if path.exists() {
            return Err(StoreError::AlreadyExists);
        }
        copy_file_atomic(source, &path)
    }

    pub fn replace_file(
        &self,
        workspace_id: Uuid,
        file: &str,
        bytes: &[u8],
    ) -> Result<(), StoreError> {
        let file = validate_relative_path(file, false).map_err(|_| StoreError::InvalidPath)?;
        let path = checked_path(&self.files_root(workspace_id), &file, false)?;
        if !fs::metadata(&path)?.is_file() {
            return Err(StoreError::FileMissing);
        }
        write_bytes_atomic(&path, bytes)?;
        Ok(())
    }

    pub fn replace_file_from_path(
        &self,
        workspace_id: Uuid,
        file: &str,
        source: &Path,
    ) -> Result<(), StoreError> {
        let file = validate_relative_path(file, false).map_err(|_| StoreError::InvalidPath)?;
        let path = checked_path(&self.files_root(workspace_id), &file, false)?;
        if !fs::metadata(&path)?.is_file() {
            return Err(StoreError::FileMissing);
        }
        copy_file_atomic(source, &path)
    }

    pub fn create_directory(&self, workspace_id: Uuid, directory: &str) -> Result<(), StoreError> {
        let directory =
            validate_relative_path(directory, false).map_err(|_| StoreError::InvalidPath)?;
        let path = checked_path(&self.files_root(workspace_id), &directory, true)?;
        fs::create_dir(path)?;
        Ok(())
    }

    pub fn move_path(
        &self,
        workspace_id: Uuid,
        source: &str,
        destination: &str,
    ) -> Result<(), StoreError> {
        let source = validate_relative_path(source, false).map_err(|_| StoreError::InvalidPath)?;
        let destination =
            validate_relative_path(destination, false).map_err(|_| StoreError::InvalidPath)?;
        if destination == source || destination.starts_with(&format!("{source}/")) {
            return Err(StoreError::InvalidPath);
        }
        let root = self.files_root(workspace_id);
        let source_path = checked_path(&root, &source, false)?;
        let destination_path = checked_path(&root, &destination, true)?;
        if destination_path.exists() {
            return Err(StoreError::DestinationExists);
        }
        fs::rename(source_path, destination_path)?;

        let mut index = self.read_file_index(workspace_id)?;
        let affected: Vec<_> = index
            .paths
            .iter()
            .filter(|(path, _)| **path == source || path.starts_with(&format!("{source}/")))
            .map(|(path, id)| (path.clone(), *id))
            .collect();
        for (old, id) in affected {
            let suffix = old.strip_prefix(&source).unwrap_or_default();
            index.paths.remove(&old);
            index.paths.insert(format!("{destination}{suffix}"), id);
        }
        self.write_file_index(workspace_id, &index)?;
        Ok(())
    }

    pub fn delete_path(&self, workspace_id: Uuid, target: &str) -> Result<Vec<Uuid>, StoreError> {
        let target = validate_relative_path(target, false).map_err(|_| StoreError::InvalidPath)?;
        let root = self.files_root(workspace_id);
        let path = checked_path(&root, &target, false)?;
        let metadata = fs::symlink_metadata(&path)?;
        if metadata.file_type().is_symlink() {
            return Err(StoreError::Symlink);
        }
        if metadata.is_dir() {
            fs::remove_dir_all(&path)?;
        } else {
            fs::remove_file(&path)?;
        }

        let mut index = self.read_file_index(workspace_id)?;
        let removed: Vec<_> = index
            .paths
            .iter()
            .filter(|(file, _)| **file == target || file.starts_with(&format!("{target}/")))
            .map(|(file, id)| (file.clone(), *id))
            .collect();
        for (file, _) in &removed {
            index.paths.remove(file);
        }
        self.write_file_index(workspace_id, &index)?;
        Ok(removed.into_iter().map(|(_, id)| id).collect())
    }

    pub fn remove_collaboration_files(
        &self,
        workspace_id: Uuid,
        file_ids: &[Uuid],
    ) -> Result<(), StoreError> {
        self.ensure_workspace(workspace_id)?;
        let root = self.collaboration_root(workspace_id);
        if fs::symlink_metadata(&root)?.file_type().is_symlink() {
            return Err(StoreError::Symlink);
        }
        for file_id in file_ids {
            for suffix in ["json", "actors.json"] {
                let path = root.join(format!("{file_id}.{suffix}"));
                match fs::remove_file(path) {
                    Ok(()) => {}
                    Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
                    Err(error) => return Err(error.into()),
                }
            }
        }
        Ok(())
    }

    pub fn collaboration_file(
        &self,
        workspace_id: Uuid,
        file: &str,
    ) -> Result<CollaborationFile, StoreError> {
        let file = validate_relative_path(file, false).map_err(|_| StoreError::InvalidPath)?;
        let path = checked_path(&self.files_root(workspace_id), &file, false)?;
        if !fs::metadata(path)?.is_file() {
            return Err(StoreError::FileMissing);
        }
        let mut index = self.read_file_index(workspace_id)?;
        let id = if let Some(id) = index.paths.get(&file) {
            *id
        } else {
            let id = Uuid::now_v7();
            index.paths.insert(file.clone(), id);
            self.write_file_index(workspace_id, &index)?;
            id
        };
        Ok(CollaborationFile { id, path: file })
    }

    pub fn collaboration_file_path(
        &self,
        workspace_id: Uuid,
        file_id: Uuid,
    ) -> Result<String, StoreError> {
        self.read_file_index(workspace_id)?
            .paths
            .into_iter()
            .find_map(|(path, id)| (id == file_id).then_some(path))
            .ok_or(StoreError::FileMissing)
    }

    pub fn collaboration_files(
        &self,
        workspace_id: Uuid,
    ) -> Result<Vec<CollaborationFile>, StoreError> {
        let index = self.read_file_index(workspace_id)?;
        Ok(index
            .paths
            .into_iter()
            .filter(|(path, _)| checked_path(&self.files_root(workspace_id), path, false).is_ok())
            .map(|(path, id)| CollaborationFile { id, path })
            .collect())
    }

    pub fn editor_actor_id(
        &self,
        workspace_id: Uuid,
        file_id: Uuid,
        user_id: Uuid,
    ) -> Result<u64, StoreError> {
        let path = self
            .collaboration_root(workspace_id)
            .join(format!("{file_id}.actors.json"));
        let mut actors: BTreeMap<Uuid, u64> = match crate::read_json(&path) {
            Ok(actors) => actors,
            Err(StoreError::Io(error)) if error.kind() == std::io::ErrorKind::NotFound => {
                BTreeMap::new()
            }
            Err(error) => return Err(error),
        };
        if let Some(actor) = actors.get(&user_id) {
            return Ok(*actor);
        }
        let used: HashSet<_> = actors.values().copied().collect();
        let actor = loop {
            let mut bytes = [0; 8];
            random_fill(&mut bytes).map_err(|error| std::io::Error::other(error.to_string()))?;
            let candidate = u64::from_le_bytes(bytes) & ((1 << 52) - 1);
            if candidate != 0 && !used.contains(&candidate) {
                break candidate;
            }
        };
        actors.insert(user_id, actor);
        write_json_atomic(&path, &actors)?;
        Ok(actor)
    }

    pub fn load_collaboration_snapshot(
        &self,
        workspace_id: Uuid,
        file_id: Uuid,
    ) -> Result<Option<Snapshot>, StoreError> {
        let path = self
            .collaboration_root(workspace_id)
            .join(format!("{file_id}.json"));
        match crate::read_json(&path) {
            Ok(snapshot) => Ok(Some(snapshot)),
            Err(StoreError::Io(error)) if error.kind() == std::io::ErrorKind::NotFound => Ok(None),
            Err(error) => Err(error),
        }
    }

    pub fn sync_collaboration_files(&self, workspace_id: Uuid) -> Result<(), StoreError> {
        self.ensure_workspace(workspace_id)?;
        let root = self.files_root(workspace_id);
        let index = self.read_file_index(workspace_id)?;
        for (relative_path, file_id) in index.paths {
            let Some(snapshot) = self.load_collaboration_snapshot(workspace_id, file_id)? else {
                continue;
            };
            let path = checked_path(&root, &relative_path, false)?;
            if !fs::metadata(&path)?.is_file() {
                return Err(StoreError::FileMissing);
            }
            write_bytes_atomic(&path, snapshot.document.text().as_bytes())?;
        }
        Ok(())
    }

    pub fn save_collaboration_state(
        &self,
        workspace_id: Uuid,
        file_id: Uuid,
        snapshot: &Snapshot,
        text: &str,
    ) -> Result<(), StoreError> {
        let path = self.collaboration_file_path(workspace_id, file_id)?;
        let file = checked_path(&self.files_root(workspace_id), &path, false)?;
        write_bytes_atomic(&file, text.as_bytes())?;
        write_json_atomic(
            &self
                .collaboration_root(workspace_id)
                .join(format!("{file_id}.json")),
            snapshot,
        )
    }

    pub fn save_collaboration_snapshot(
        &self,
        workspace_id: Uuid,
        file_id: Uuid,
        snapshot: &Snapshot,
    ) -> Result<(), StoreError> {
        self.ensure_workspace(workspace_id)?;
        let root = self.collaboration_root(workspace_id);
        if fs::symlink_metadata(&root)?.file_type().is_symlink() {
            return Err(StoreError::Symlink);
        }
        write_json_atomic(&root.join(format!("{file_id}.json")), snapshot)
    }

    pub fn reset_collaboration(&self, workspace_id: Uuid) -> Result<(), StoreError> {
        let root = self.collaboration_root(workspace_id);
        if fs::symlink_metadata(&root)?.file_type().is_symlink() {
            return Err(StoreError::Symlink);
        }
        fs::remove_dir_all(&root)?;
        fs::create_dir(&root)?;
        Ok(())
    }

    pub fn clear_collaboration_snapshot(
        &self,
        workspace_id: Uuid,
        file_id: Uuid,
    ) -> Result<(), StoreError> {
        let path = self
            .collaboration_root(workspace_id)
            .join(format!("{file_id}.json"));
        match fs::remove_file(path) {
            Ok(()) => Ok(()),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
            Err(error) => Err(error.into()),
        }
    }

    fn read_file_index(&self, workspace_id: Uuid) -> Result<FileIndex, StoreError> {
        let path = self
            .collaboration_root(workspace_id)
            .join("file-index.json");
        match crate::read_json(&path) {
            Ok(index) => Ok(index),
            Err(StoreError::Io(error)) if error.kind() == std::io::ErrorKind::NotFound => {
                Ok(FileIndex::default())
            }
            Err(error) => Err(error),
        }
    }

    fn write_file_index(&self, workspace_id: Uuid, index: &FileIndex) -> Result<(), StoreError> {
        crate::write_json_atomic(
            &self
                .collaboration_root(workspace_id)
                .join("file-index.json"),
            index,
        )
    }

    fn collaboration_root(&self, workspace_id: Uuid) -> PathBuf {
        self.workspace_path(workspace_id).join("collaboration")
    }
}

fn create_parent_directories(root: &Path, file: &str) -> Result<(), StoreError> {
    if fs::symlink_metadata(root)?.file_type().is_symlink() {
        return Err(StoreError::Symlink);
    }
    let components: Vec<_> = file.split('/').collect();
    let mut current = root.to_path_buf();
    for component in &components[..components.len().saturating_sub(1)] {
        current.push(component);
        match fs::symlink_metadata(&current) {
            Ok(metadata) if metadata.file_type().is_symlink() => return Err(StoreError::Symlink),
            Ok(metadata) if metadata.is_dir() => {}
            Ok(_) => return Err(StoreError::InvalidPath),
            Err(error) if error.kind() == io::ErrorKind::NotFound => {
                fs::create_dir(&current)?;
            }
            Err(error) => return Err(error.into()),
        }
    }
    Ok(())
}

fn copy_file_atomic(source: &Path, destination: &Path) -> Result<(), StoreError> {
    let metadata = fs::symlink_metadata(source)?;
    if metadata.file_type().is_symlink() || !metadata.is_file() {
        return Err(StoreError::InvalidPath);
    }
    let mut source = fs::File::open(source)?;
    let parent = destination.parent().ok_or(StoreError::InvalidPath)?;
    fs::create_dir_all(parent)?;
    let mut temporary = NamedTempFile::new_in(parent)?;
    io::copy(&mut source, temporary.as_file_mut())?;
    temporary.as_file().sync_all()?;
    temporary
        .persist(destination)
        .map_err(|error| StoreError::Io(error.error))?;
    Ok(())
}

fn checked_path(
    root: &Path,
    relative: &str,
    final_may_not_exist: bool,
) -> Result<PathBuf, StoreError> {
    let relative = validate_relative_path(relative, true).map_err(|_| StoreError::InvalidPath)?;
    if fs::symlink_metadata(root)?.file_type().is_symlink() {
        return Err(StoreError::Symlink);
    }
    let mut current = root.to_path_buf();
    if relative.is_empty() {
        return Ok(current);
    }
    let components: Vec<_> = relative.split('/').collect();
    for (index, component) in components.iter().enumerate() {
        current.push(component);
        match fs::symlink_metadata(&current) {
            Ok(metadata) => {
                if metadata.file_type().is_symlink() {
                    return Err(StoreError::Symlink);
                }
                if index + 1 < components.len() && !metadata.is_dir() {
                    return Err(StoreError::InvalidPath);
                }
            }
            Err(error)
                if error.kind() == std::io::ErrorKind::NotFound
                    && final_may_not_exist
                    && index + 1 == components.len() =>
            {
                break;
            }
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                return Err(StoreError::FileMissing);
            }
            Err(error) => return Err(error.into()),
        }
    }
    Ok(current)
}
