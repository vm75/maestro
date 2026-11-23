//! Metadata-only directory selection for the non-root Linux workload account.
use std::{
    collections::BTreeMap,
    fs,
    path::{Component, Path, PathBuf},
};

use anyhow::{Context, Result, ensure};
use serde::Serialize;

const LIMIT: usize = 256;

#[derive(Serialize)]
pub struct Listing {
    path: String,
    parent: String,
    directories: Vec<String>,
    files: Vec<String>,
}

pub fn browse(target: &str) -> Result<Listing> {
    ensure!(
        target.chars().count() <= 4096 && !target.contains('\0'),
        "invalid directory path"
    );
    // Match filepath.Clean on an absolute path; never resolve against the
    // daemon's working directory or read any file's contents.
    let mut clean = PathBuf::from("/");
    for component in Path::new(target).components() {
        match component {
            Component::Normal(name) => clean.push(name),
            Component::ParentDir => {
                clean.pop();
            }
            _ => {}
        }
    }
    let entries = fs::read_dir(&clean).context("Directory cannot be read")?;
    // Retain only the lexically first 256 visible names, matching the Go
    // reference's sorted ReadDir followed by its visible-entry limit.
    let mut selected = BTreeMap::new();
    for entry in entries {
        let entry = entry.context("Directory cannot be read")?;
        let name = entry.file_name().to_string_lossy().into_owned();
        if name.starts_with('.') {
            continue;
        }
        if selected.len() == LIMIT
            && selected
                .last_key_value()
                .is_some_and(|(last, _)| &name >= last)
        {
            continue;
        }
        let directory = entry
            .file_type()
            .context("Directory entry cannot be inspected")?
            .is_dir();
        selected.insert(name, directory);
        if selected.len() > LIMIT {
            selected.pop_last();
        }
    }
    let mut directories = Vec::new();
    let mut files = Vec::new();
    for (name, directory) in selected {
        if directory {
            directories.push(name);
        } else {
            files.push(name);
        }
    }
    Ok(Listing {
        path: clean.to_string_lossy().into_owned(),
        parent: clean
            .parent()
            .unwrap_or(Path::new("/"))
            .to_string_lossy()
            .into_owned(),
        directories,
        files,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn listing_hides_private_names_and_never_follows_entry_symlinks() {
        let root = tempfile::tempdir().unwrap();
        fs::create_dir(root.path().join("folder")).unwrap();
        fs::write(root.path().join("file"), "private contents").unwrap();
        fs::write(root.path().join(".hidden"), "secret").unwrap();
        std::os::unix::fs::symlink(root.path().join("folder"), root.path().join("link")).unwrap();
        let result = browse(&format!("{}/folder/..", root.path().display())).unwrap();
        assert_eq!(result.directories, ["folder"]);
        assert_eq!(result.files, ["file", "link"]);
        assert_eq!(result.path, root.path().to_string_lossy());
        assert!(
            !serde_json::to_string(&result)
                .unwrap()
                .contains("private contents")
        );
    }
    #[test]
    fn limit_is_applied_to_sorted_visible_entries() {
        let root = tempfile::tempdir().unwrap();
        for index in (0..300).rev() {
            fs::create_dir(root.path().join(format!("item{index:03}"))).unwrap();
        }
        let result = browse(root.path().to_str().unwrap()).unwrap();
        assert_eq!(result.directories.len(), LIMIT);
        assert_eq!(result.directories.last().unwrap(), "item255");
        assert!(browse("bad\0path").is_err());
        assert!(browse(&"a".repeat(4097)).is_err());
    }
}
