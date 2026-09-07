/// Utilities for getting source documents for indexing
use std::path::PathBuf;

/// Recursively walk `roots` (absolute paths) for `.org` files.
async fn walk_org_files(roots: Vec<PathBuf>) -> Vec<PathBuf> {
    let mut result = Vec::new();
    let mut pending = roots;

    while let Some(dir) = pending.pop() {
        let Ok(mut entries) = tokio::fs::read_dir(&dir).await else {
            continue;
        };
        while let Some(entry) = entries.next_entry().await.unwrap_or(None) {
            let Ok(meta) = entry.metadata().await else {
                continue;
            };
            let p = entry.path();
            if meta.is_dir() {
                pending.push(p);
            } else {
                // `org_archive` files hold archived Org subtrees (Org mode's
                // `org-archive-location` convention), so index them alongside
                // plain `.org` notes.
                let name = p.to_string_lossy();
                if name.ends_with(".org") || crate::core::orgmode::is_archive_file(&name) {
                    result.push(p);
                }
            }
        }
    }
    result
}

/// Recursively walk the `notes/` and `projects/` subdirectories of `path`
/// (the notes repo root) for `.org` files. The repo root itself is excluded,
/// but both source subdirectories are indexed.
pub async fn notes(path: &str) -> Vec<PathBuf> {
    walk_org_files(vec![
        PathBuf::from(format!("{path}/notes")),
        PathBuf::from(format!("{path}/projects")),
    ])
    .await
}

/// Return a list of notes filtered by file names
pub async fn note_filter(path: &str, file_paths: Vec<PathBuf>) -> Vec<PathBuf> {
    // By using the notes source function we also inherit all the
    // extra filtering and rules for which files are eligible so they
    // don't need to be repeated in multiple places.
    notes(path)
        .await
        .into_iter()
        .filter(|p| file_paths.contains(p))
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use tempfile::TempDir;

    /// `notes()` must walk both the `notes/` and `projects/` subdirectories so
    /// project files are indexed alongside regular notes, and must recurse into
    /// nested subdirectories within each.
    #[tokio::test]
    async fn notes_walks_notes_and_projects() {
        let dir = TempDir::new().unwrap();
        let root = dir.path();

        fs::create_dir_all(root.join("notes/sub")).unwrap();
        fs::create_dir_all(root.join("projects")).unwrap();
        fs::create_dir_all(root.join("ignored")).unwrap();

        fs::write(root.join("notes/top.org"), "").unwrap();
        fs::write(root.join("notes/sub/nested.org"), "").unwrap();
        fs::write(root.join("projects/project.org"), "").unwrap();
        fs::write(root.join("projects/work.org_archive"), "").unwrap();
        fs::write(root.join("ignored/root.org"), "").unwrap();
        fs::write(root.join("top_level.org"), "").unwrap();

        let mut found = notes(root.to_str().unwrap()).await;
        found.sort();

        let expected: Vec<PathBuf> = vec![
            root.join("notes/sub/nested.org"),
            root.join("notes/top.org"),
            root.join("projects/project.org"),
            root.join("projects/work.org_archive"),
        ];
        assert_eq!(found, expected);
    }
}
