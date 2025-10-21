/// Utilities for getting source documents for indexing
use std::fs;
use std::path::PathBuf;

/// Get first level files in the directory, does not follow sub
/// directories.
pub fn notes(path: &str) -> Vec<PathBuf> {
    let Ok(entries) = fs::read_dir(path) else {
        return vec![];
    };

    // TODO: make this recursive if there is more than one directory of notes
    entries
        .flatten()
        .flat_map(|entry| {
            let Ok(meta) = entry.metadata() else {
                return vec![];
            };
            // Skip directories and non org files
            let path = entry.path();
            let ext = path.extension().unwrap_or_default();
            let name = path.file_name().unwrap_or_default();
            if meta.is_file() && ext == "org" && name != "config.org" {
                return vec![entry.path()];
            }
            vec![]
        })
        .collect()
}

/// Return a list of notes filtered by file names
pub fn note_filter(path: &str, file_paths: Vec<PathBuf>) -> Vec<PathBuf> {
    // By using the notes source function we also inherit all the
    // extra filtering and rules for which files are eligible so they
    // don't need to be repeated in multiple places.
    notes(path)
        .into_iter()
        .filter(|p| file_paths.contains(p))
        .collect()
}
