use std::path::PathBuf;

use anyhow::{Context, Result};
use chrono::Local;
use tokio::fs;
use tokio_rusqlite::Connection;
use uuid::Uuid;

use crate::cli::projects;
use crate::core::orgmode;
use crate::org;
use crate::search::{index_single_file, remove_task_from_indexes};

/// Parse a comma-separated list of tags (e.g. `"urgent, errands"`) into
/// trimmed, non-empty tag strings. Empty entries (`a,,b`) and surrounding
/// whitespace are stripped.
pub(crate) fn parse_tag_list(s: &str) -> Vec<String> {
    s.split(',')
        .map(|t| t.trim().to_string())
        .filter(|t| !t.is_empty())
        .collect()
}

/// Whether a file path points at an Org archive file (`*.org_archive`).
fn is_archive_path(path: &std::path::Path) -> bool {
    crate::core::orgmode::is_archive_file(&path.to_string_lossy())
}

/// Deterministic path a project file would use, so callers can lock it before
/// creating. Creation must be serialized on the target path: when concurrent
/// refiles target a brand-new project they can all miss the DB lookup and race
/// to create it, and an unlocked create can clobber a task another refile just
/// appended.
fn project_file_path(notes_path: &str, project_name: &str) -> Result<PathBuf> {
    let slug = slugify(project_name)?;
    let today = Local::now().format("%Y-%m-%d");

    // Special files (capture, refile, personal, work) are addressed by their
    // bare name with no date prefix — e.g. `work.org` rather than
    // `2026-08-11--project-work.org`.
    let special = projects::db::is_special_file(project_name);
    let filename = if special {
        format!("{project_name}.org")
    } else {
        format!("{today}--project-{slug}.org")
    };
    Ok(PathBuf::from(format!("{notes_path}/projects/{filename}")))
}

/// Create a new project file on disk and register it in the database.
async fn create_project_file(db: &Connection, notes_path: &str, project_name: &str) -> Result<PathBuf> {
    let project_id = Uuid::new_v4().to_string();
    let slug = slugify(project_name)?;
    let today = Local::now().format("%Y-%m-%d");
    let special = projects::db::is_special_file(project_name);
    let full_path = project_file_path(notes_path, project_name)?;
    let filename = full_path
        .file_name()
        .and_then(|s| s.to_str())
        .ok_or_else(|| anyhow::anyhow!("invalid project file name"))?;
    let db_filename = format!("projects/{filename}");

    let content = org::Document::builder()
        .property("ID", &project_id)
        .title(project_name)
        .category(if special { project_name } else { &slug })
        .date(&today.to_string())
        .filetags(if special { "private inbox" } else { "private project" })
        .build()
        .to_string();
    orgmode::atomic_write(&full_path, &content)
        .await
        .context("Failed to create project file")?;

    // Register in DB so subsequent lookups find the project without
    // requiring a full re-index.
    let db_id = project_id;
    let db_title = project_name.to_string();
    db.call(move |conn| {
        conn.execute(
            "INSERT INTO note_meta (id, file_name, title, type, tags)
             VALUES (?1, ?2, ?3, 'note', 'project')",
            rusqlite::params![db_id, db_filename, db_title],
        )?;
        Ok(())
    })
    .await?;

    Ok(PathBuf::from(full_path))
}

fn slugify(s: &str) -> Result<String> {
    let slug: String = s
        .to_lowercase()
        .chars()
        .filter_map(|c| {
            if c.is_alphanumeric() || c == '-' || c == ' ' {
                Some(c)
            } else {
                None
            }
        })
        .collect::<String>()
        .split_whitespace()
        .collect::<Vec<&str>>()
        .join("-")
        .trim_matches('-')
        .to_string();
    if slug.is_empty() {
        anyhow::bail!("Cannot slugify empty string: input produced no valid characters");
    }
    Ok(slug)
}

pub async fn run_create(
    db: &Connection,
    notes_path: &str,
    index_path: &str,
    title: &str,
    body: Option<&str>,
    project: Option<&str>,
    status: &str,
) -> Result<()> {
    let id = Uuid::new_v4().to_string();
    let body = body.unwrap_or_default();
    let status_upper = status.to_uppercase();
    let headline = orgmode::build_headline(&id, title, body, &status_upper, 1);

    let file_path = if let Some(project_name) = project {
        // Look up existing project in DB, or create a new project file
        let project_path = match projects::db::find_project_file(db, notes_path, project_name).await? {
            Some(path) => path,
            None => create_project_file(db, notes_path, project_name).await?,
        };
        println!("Created project file: {}", project_path.display());
        orgmode::with_file_lock(&project_path, || async {
            let mut project_content = fs::read_to_string(&project_path).await?;
            if !project_content.ends_with('\n') {
                project_content.push('\n');
            }
            project_content.push_str(&headline);
            project_content.push('\n');
            orgmode::atomic_write(&project_path, &project_content)
                .await
                .context("Failed to write project file")
        })
        .await?;
        println!("Created task '{title}' in project '{project_name}' (id: {id})");
        project_path
    } else {
        // Standalone tasks go into the capture.org inbox so the default
        // `tasks list` (which scans capture.org) finds them.
        let capture_path = PathBuf::from(format!("{notes_path}/projects/capture.org"));
        orgmode::with_file_lock(&capture_path, || async {
            let mut content = if capture_path.exists() {
                fs::read_to_string(&capture_path).await?
            } else {
                let capture_id = Uuid::new_v4().to_string();
                let today = Local::now().format("%Y-%m-%d");
                org::Document::builder()
                    .property("ID", &capture_id)
                    .title("Capture")
                    .category("capture")
                    .date(&today.to_string())
                    .filetags("private inbox")
                    .build()
                    .to_string()
            };
            if !content.ends_with('\n') {
                content.push('\n');
            }
            content.push_str(&headline);
            content.push('\n');
            orgmode::atomic_write(&capture_path, &content)
                .await
                .context("Failed to write task file")
        })
        .await?;
        println!(
            "Created task '{title}' (id: {id}, file: {})",
            capture_path.display()
        );
        capture_path
    };

    // Re-index the affected file so the task is immediately queryable
    index_single_file(db, index_path, notes_path, file_path).await?;

    Ok(())
}

pub async fn run_update(
    db: &Connection,
    notes_path: &str,
    index_path: &str,
    id: &str,
    title: Option<&str>,
    body: Option<&str>,
    status: Option<&str>,
    project: Option<&str>,
    add_tags: &[String],
    remove_tags: &[String],
) -> Result<()> {
    if let Some(project_ref) = project {
        let path = projects::db::find_project_file(db, notes_path, project_ref).await?
            .ok_or_else(|| anyhow::anyhow!("Project '{project_ref}' not found"))?;
        let filename = path
            .strip_prefix(notes_path)
            .unwrap_or_else(|_| path.as_path())
            .to_str()
            .unwrap();
        orgmode::update_task(
            db,
            notes_path,
            id,
            Some(filename),
            title,
            body,
            status,
            add_tags,
            remove_tags,
        )
        .await?;
    } else {
        orgmode::update_task(db, notes_path, id, None, title, body, status, add_tags, remove_tags).await?;
    }
    println!("Task {id} updated");

    // Re-index the modified file so the task changes are immediately queryable
    let location = orgmode::find_task(db, notes_path, id).await?;
    index_single_file(db, index_path, notes_path, location.path).await?;

    Ok(())
}

/// Move a task from its current file into a project file.
///
/// Finds the task by UUID across all org files, removes the headline from its
/// source, and appends it to the target project file (creating the project
/// file if it doesn't exist yet).
pub async fn run_refile(
    db: &Connection,
    notes_path: &str,
    index_path: &str,
    id: &str,
    project: &str,
) -> Result<()> {
    // Resolve which file holds the task (via the index) so we know which
    // file to lock before doing the read-modify-write.
    let source_path = orgmode::find_task(db, notes_path, id).await?.path;
    if is_archive_path(&source_path) {
        anyhow::bail!("Cannot refile a task out of an archive file");
    }

    // Look up existing project in DB, or create a new project file. Creation is
    // serialized on the target file's lock: when several concurrent refiles
    // target a brand-new project, they can all miss the lookup (no DB row yet)
    // and race to create it. Creating under the lock — re-checking first so only
    // the first creator writes the header — prevents one creator's write from
    // clobbering a task another refile already appended.
    let target_path = projects::db::find_project_file(db, notes_path, project).await?;
    let target_path = match target_path {
        Some(path) => path,
        None => {
            let path = project_file_path(notes_path, project)?;
            orgmode::with_file_lock(&path, || async {
                if let Some(existing) =
                    projects::db::find_project_file(db, notes_path, project).await?
                {
                    return Ok(existing);
                }
                create_project_file(db, notes_path, project).await
            })
            .await?
        }
    };

    if is_archive_path(&target_path) {
        anyhow::bail!("Cannot refile a task to an archive file");
    }

    if source_path == target_path {
        anyhow::bail!("Task is already in project '{project}'");
    }

    orgmode::with_file_locks(
        &[source_path.clone(), target_path.clone()],
        || async {
            // Re-read the task under the lock so a concurrent refile can't be
            // overwritten (lost update) and the git sync never sees a partial file.
            let location = orgmode::find_task_in_file(&source_path, id).await?;

            // Extract the raw headline text (preserves all org-mode structure)
            let headline_text = &location.content[location.range.start..location.range.end];

            // Remove the headline from the source file
            let before = &location.content[..location.range.start];
            let after = &location.content[location.range.end..];
            let after = after.strip_prefix('\n').unwrap_or(after);
            let new_source = format!("{before}{after}");
            orgmode::atomic_write(&location.path, &new_source)
                .await
                .context("Failed to write source file after refile")?;

            // Append the raw headline verbatim to the target project file
            let mut target_content = fs::read_to_string(&target_path).await?;
            if !target_content.ends_with('\n') {
                target_content.push('\n');
            }
            target_content.push_str(headline_text);
            target_content.push('\n');
            orgmode::atomic_write(&target_path, &target_content)
                .await
                .context("Failed to write target project file")?;

            println!(
                "Refiled task {id} ('{}') from {} to {}",
                location.current_title,
                location.path.display(),
                target_path.display()
            );

            // Re-index both files so note_meta and full-text search reflect the
            // move: the task now lives in target, not source. Done under the
            // file locks so concurrent refiles sharing these files also
            // serialize their Tantivy index writes (an IndexWriter is exclusive
            // per directory, so parallel writers would panic on LockBusy).
            index_single_file(db, index_path, notes_path, source_path.clone()).await?;
            index_single_file(db, index_path, notes_path, target_path.clone()).await?;

            Ok(())
        },
    )
    .await?;

    Ok(())
}

pub async fn run_delete(
    db: &Connection,
    notes_path: &str,
    index_path: &str,
    id: &str,
) -> Result<()> {
    let location = orgmode::find_task(db, notes_path, id).await?;
    let path = location.path.clone();

    orgmode::with_file_lock(&path, || async {
        // Re-read under the lock so a concurrent writer can't be overwritten.
        let location = orgmode::find_task_in_file(&path, id).await?;

        let before = &location.content[..location.range.start];
        let after = &location.content[location.range.end..];
        // Remove one trailing newline if present to avoid blank-line gaps
        let after = after.strip_prefix('\n').unwrap_or(after);
        let new_content = format!("{before}{after}");
        orgmode::atomic_write(&location.path, &new_content)
            .await
            .context("Failed to write project file after deletion")?;
        println!("Deleted task {id} from {}", location.path.display());

        Ok(())
    })
    .await?;

    remove_task_from_indexes(db, index_path, id).await?;

    Ok(())
}

pub async fn run_list(
    db: &Connection,
    notes_path: &str,
    project: Option<&str>,
    status: Option<&str>,
) -> Result<()> {
    let tasks = if let Some(project_ref) = project {
        let (filenames, display_prefix) = if projects::db::is_special_file(project_ref) {
            let filename = format!("projects/{project_ref}.org");
            (vec![filename], None)
        } else {
            let path = projects::db::find_project_file(db, notes_path, project_ref).await?
                .ok_or_else(|| anyhow::anyhow!("Project '{project_ref}' not found"))?;
            let filename = path
                .strip_prefix(notes_path)
                .unwrap_or_else(|_| path.as_path())
                .to_str()
                .unwrap_or(project_ref)
                .to_string();
            (vec![filename], None)
        };
        list_tasks_from_files(db, &filenames, status, display_prefix).await?
    } else {
        list_tasks_from_files(
            db,
            &["projects/refile.org".into(), "projects/capture.org".into()],
            status,
            None,
        )
        .await?
    };

    if tasks.is_empty() {
        println!("No tasks found matching the given criteria.");
        return Ok(());
    }

    println!("{:<40} {:<10} {:<24} {}", "ID", "Status", "Project", "Title");
    println!("{}", "-".repeat(100));
    for (id, task_status, project_display, title) in &tasks {
        println!("{id:<40} {task_status:<10} {project_display:<24} {title}");
    }

    Ok(())
}

async fn list_tasks_from_files(
    db: &Connection,
    filenames: &[String],
    status_filter: Option<&str>,
    display_prefix: Option<&str>,
) -> Result<Vec<(String, String, String, String)>> {
    let status_lower = status_filter.map(|s| s.to_lowercase());
    let filenames_json = serde_json::to_string(filenames).unwrap();

    let tasks = db
        .call(move |conn| {
            let mut stmt = conn.prepare(
                "SELECT id, title, status, file_name
                 FROM note_meta
                 WHERE type = 'task'
                   AND file_name IN (SELECT value FROM json_each(?))
                   AND (? IS NULL OR status = ?)
                 ORDER BY status, title",
            )?;

            let status_val: &dyn rusqlite::types::ToSql = match &status_lower {
                Some(s) => s,
                None => &rusqlite::types::Null,
            };

            let rows = stmt
                .query_map(
                    rusqlite::params![filenames_json.as_bytes(), status_val, status_val],
                    |row| {
                        let id: String = row.get(0)?;
                        let title: String = row.get(1)?;
                        let status: String = row.get(2)?;
                        let file_name: String = row.get(3)?;
                        Ok((id, title, status, file_name))
                    },
                )?
                .filter_map(|r| r.ok())
                .collect::<Vec<_>>();

            Ok(rows)
        })
        .await?;

    let display_prefix = display_prefix.map(|s| s.to_string());
    let result: Vec<(String, String, String, String)> = tasks
        .into_iter()
        .map(|(id, title, status, file_name)| {
            let display = display_prefix.clone().unwrap_or_else(|| {
                file_name
                    .strip_suffix(".org")
                    .unwrap_or(&file_name)
                    .to_string()
            });
            (id, status.to_uppercase(), display, title)
        })
        .collect();

    Ok(result)
}

#[cfg(test)]
mod tests {
    use super::*;
    use orgize::ParseConfig;
    use rusqlite;
    use std::fs;
    use tempfile::TempDir;

    fn parsing_config() -> ParseConfig {
        ParseConfig {
            todo_keywords: (
                vec!["TODO".to_string(), "NEXT".to_string(), "WAITING".to_string()],
                vec!["DONE".to_string(), "CANCELED".to_string(), "SOMEDAY".to_string()],
            ),
            ..Default::default()
        }
    }

    /// Parse an org file at `path` and return the first headline's properties.
    fn parse_headline(path: &std::path::Path) -> (String, String, String) {
        let content = fs::read_to_string(path).unwrap();
        let config = parsing_config();
        let org = config.parse(&content);
        let h = org.document().headlines().next().unwrap();
        let status = h.todo_keyword().map(|k| k.to_string()).unwrap_or_default();
        let title = h.title_raw().trim().to_string();
        let props = h.properties().unwrap();
        let id = props.get("ID").unwrap().to_string();
        (id, status, title)
    }

    fn headline_count(path: &std::path::Path) -> usize {
        let content = fs::read_to_string(path).unwrap();
        let config = parsing_config();
        let org = config.parse(&content);
        org.document().headlines().count()
    }

    // -----------------------------------------------------------------------
    // slugify
    // -----------------------------------------------------------------------

    #[test]
    fn test_slugify_normal() {
        assert_eq!(slugify("Buy groceries").unwrap(), "buy-groceries");
    }

    #[test]
    fn test_slugify_multiple_spaces() {
        assert_eq!(slugify("  hello   world  ").unwrap(), "hello-world");
    }

    #[test]
    fn test_slugify_special_chars() {
        assert_eq!(slugify("Fix bug! (urgent) #42").unwrap(), "fix-bug-urgent-42");
    }

    #[test]
    fn test_slugify_only_special_chars() {
        assert!(slugify("!!! @@@ $$$").is_err());
    }

    #[test]
    fn test_slugify_empty_string() {
        assert!(slugify("").is_err());
    }

    #[test]
    fn test_slugify_already_slug() {
        assert_eq!(slugify("my-task-name").unwrap(), "my-task-name");
    }

    // -----------------------------------------------------------------------
    // run_create
    // -----------------------------------------------------------------------

    #[tokio::test]
    async fn test_create_standalone_task() {
        let (db, _dir, notes, index) = test_env().await;

        run_create(&db, &notes, &index, "Test Task", None, None, "TODO")
            .await
            .unwrap();

        // Should create a single .org file
        let entries: Vec<_> = fs::read_dir(projects_dir(&notes)).unwrap().collect();
        assert_eq!(entries.len(), 1);
        let path = entries[0].as_ref().unwrap().path();
        assert!(path.extension().unwrap() == "org");

        let (id, status, title) = parse_headline(&path);
        assert_eq!(status, "TODO");
        assert_eq!(title, "Test Task");
        assert!(!id.is_empty(), "should have a UUID");
    }

    #[tokio::test]
    async fn test_create_standalone_task_with_body() {
        let (db, _dir, notes, index) = test_env().await;

        run_create(&db, &notes, &index, "Buy milk", Some("Milk, eggs, bread"), None, "TODO")
            .await
            .unwrap();

        let entries: Vec<_> = fs::read_dir(projects_dir(&notes)).unwrap().collect();
        let path = entries[0].as_ref().unwrap().path();
        let content = fs::read_to_string(&path).unwrap();
        assert!(content.contains("Milk, eggs, bread"));
    }

    #[tokio::test]
    async fn test_create_standalone_task_custom_status() {
        let (db, _dir, notes, index) = test_env().await;

        run_create(&db, &notes, &index, "Urgent fix", None, None, "NEXT")
            .await
            .unwrap();

        let entries: Vec<_> = fs::read_dir(projects_dir(&notes)).unwrap().collect();
        let path = entries[0].as_ref().unwrap().path();
        let (_, status, _) = parse_headline(&path);
        assert_eq!(status, "NEXT");
    }

    // -----------------------------------------------------------------------
    // run_create — with named project
    // -----------------------------------------------------------------------

    #[tokio::test]
    async fn test_create_project_task_creates_project_file() {
        let (db, _dir, notes, index) = test_env().await;

        run_create(&db, &notes, &index, "Fix login", None, Some("sprint-12"), "TODO")
            .await
            .unwrap();

        // Should create project file with one headline
        let entries: Vec<_> = fs::read_dir(projects_dir(&notes)).unwrap().collect();
        assert_eq!(entries.len(), 1);
        let path = entries[0].as_ref().unwrap().path();
        assert!(path.to_str().unwrap().contains("--project-sprint-12"));
        assert_eq!(headline_count(&path), 1);

        let (id, status, title) = parse_headline(&path);
        assert_eq!(status, "TODO");
        assert_eq!(title, "Fix login");
        assert!(!id.is_empty());
    }

    #[tokio::test]
    async fn test_create_second_task_in_project() {
        let (db, _dir, notes, index) = test_env().await;

        run_create(&db, &notes, &index, "Task one", None, Some("sprint-12"), "TODO")
            .await
            .unwrap();

        // Second create reuses the same project file
        run_create(&db, &notes, &index, "Task two", None, Some("sprint-12"), "DONE")
            .await
            .unwrap();

        // Single project file with two headlines
        let entries: Vec<_> = fs::read_dir(projects_dir(&notes)).unwrap().collect();
        assert_eq!(entries.len(), 1);
        let path = entries[0].as_ref().unwrap().path();
        assert_eq!(headline_count(&path), 2);
    }

    /// Count task rows in note_meta for the given title.
    async fn count_tasks_by_title(db: &Connection, title: &str) -> usize {
        let title = title.to_owned();
        db.call(move |conn| {
            let count: usize = conn
                .query_row(
                    "SELECT COUNT(*) FROM note_meta WHERE type = 'task' AND title = ?",
                    [&title],
                    |row| row.get(0),
                )
                .unwrap();
            Ok(count)
        })
        .await
        .unwrap()
    }

    #[tokio::test]
    async fn test_create_indexes_task_in_note_meta() {
        let (db, _dir, notes, index) = test_env().await;

        run_create(&db, &notes, &index, "Visible task", None, None, "TODO")
            .await
            .unwrap();

        // Creating a task must register it in note_meta so tasks list finds it.
        assert_eq!(count_tasks_by_title(&db, "Visible task").await, 1);
    }

    #[tokio::test]
    async fn test_delete_removes_task_from_note_meta() {
        let (db, _dir, notes, index) = test_env().await;

        run_create(&db, &notes, &index, "Doomed task", None, None, "TODO")
            .await
            .unwrap();
        assert_eq!(count_tasks_by_title(&db, "Doomed task").await, 1);

        // Extract the task ID from the capture file.
        let path = fs::read_dir(projects_dir(&notes)).unwrap().next().unwrap().unwrap().path();
        let content = fs::read_to_string(&path).unwrap();
        let id_marker = ":ID:       ";
        let id_start = content.match_indices(id_marker).nth(1).unwrap().0 + id_marker.len();
        let id = content[id_start..].lines().next().unwrap().trim().to_string();

        run_delete(&db, &notes, &index, &id).await.unwrap();

        // Deleting a task must remove it from note_meta.
        assert_eq!(count_tasks_by_title(&db, "Doomed task").await, 0);
    }

    // -----------------------------------------------------------------------
    // run_update
    // -----------------------------------------------------------------------

    #[tokio::test]
    async fn test_update_standalone_status() {
        let (db, _dir, notes, index) = test_env().await;

        run_create(&db, &notes, &index, "My task", None, None, "TODO")
            .await
            .unwrap();

        // Find the created task's ID
        let path = fs::read_dir(projects_dir(&notes)).unwrap().next().unwrap().unwrap().path();
        let content = fs::read_to_string(&path).unwrap();
        let id_marker = ":ID:       ";
        let task_id_start = content.match_indices(id_marker).nth(1).unwrap().0 + id_marker.len();
        let id = content[task_id_start..].lines().next().unwrap().trim().to_string();

        run_update(&db, &notes, &index, &id, None, None, Some("DONE"), None, &[], &[])            .await
            .unwrap();

        let (_, status, _) = parse_headline(&path);
        assert_eq!(status, "DONE");
    }

    #[tokio::test]
    async fn test_update_standalone_title_and_body() {
        let (db, _dir, notes, index) = test_env().await;

        run_create(&db, &notes, &index, "Old title", Some("Old body"), None, "TODO")
            .await
            .unwrap();

        let path = fs::read_dir(projects_dir(&notes)).unwrap().next().unwrap().unwrap().path();
        let content = fs::read_to_string(&path).unwrap();
        let id_marker = ":ID:       ";
        let task_id_start = content.match_indices(id_marker).nth(1).unwrap().0 + id_marker.len();
        let id = content[task_id_start..].lines().next().unwrap().trim().to_string();

        run_update(&db, &notes, &index, &id, Some("New title"), Some("New body"), None, None, &[], &[])            .await
            .unwrap();

        let (_, status, title) = parse_headline(&path);
        assert_eq!(status, "TODO");
        assert_eq!(title, "New title");
        let new_content = fs::read_to_string(&path).unwrap();
        assert!(new_content.contains("New body"));
        assert!(!new_content.contains("Old body"));
    }

    #[tokio::test]
    async fn test_update_project_task_status() {
        let (db, _dir, notes, index) = test_env().await;

        run_create(&db, &notes, &index, "Fix bug", None, Some("sprint-12"), "TODO")
            .await
            .unwrap();

        // Find the task ID from the project file
        let path = fs::read_dir(projects_dir(&notes)).unwrap().next().unwrap().unwrap().path();
        let content = fs::read_to_string(&path).unwrap();
        // The headline ID is the second occurrence (after the project-level ID)
        let id_marker = ":ID:       ";
        let second_id_start = content.match_indices(id_marker).nth(1).unwrap().0 + id_marker.len();
        let id = content[second_id_start..].lines().next().unwrap().trim().to_string();

        run_update(&db, &notes, &index, &id, None, None, Some("DONE"), None, &[], &[])            .await
            .unwrap();

        // Re-parse and check the headline
        let config = parsing_config();
        let org = config.parse(&fs::read_to_string(&path).unwrap());
        let headlines: Vec<_> = org.document().headlines().collect();
        assert_eq!(headlines.len(), 1);
        assert_eq!(
            headlines[0].todo_keyword().unwrap().to_string(),
            "DONE"
        );
    }

    #[tokio::test]
    async fn test_update_nonexistent_task() {
        let (db, _dir, notes, index) = test_env().await;

        let result = run_update(&db, &notes, &index, "nonexistent-uuid", None, None, Some("DONE"), None, &[], &[]).await;
        assert!(result.is_err());
    }

    // -----------------------------------------------------------------------
    // run_update — state transition logging (CLOSED + LOGBOOK)
    // -----------------------------------------------------------------------

    /// Helper: create a TODO task and return (path, id) so tests can drive
    /// subsequent updates without re-deriving the ID.
    async fn create_todo_task(db: &Connection, notes: &str, index: &str) -> (PathBuf, String) {
        run_create(db, notes, &index, "Test task", None, None, "TODO")
            .await
            .unwrap();
        let path = fs::read_dir(projects_dir(notes)).unwrap().next().unwrap().unwrap().path();
        let content = fs::read_to_string(&path).unwrap();
        let id_marker = ":ID:       ";
        let task_id_start = content.match_indices(id_marker).nth(1).unwrap().0 + id_marker.len();
        let id = content[task_id_start..].lines().next().unwrap().trim().to_string();
        (path, id)
    }

    #[tokio::test]
    async fn test_update_logs_state_change_to_done() {
        let (db, _dir, notes, index) = test_env().await;
        let (path, id) = create_todo_task(&db, &notes, &index).await;

        run_update(&db, &notes, &index, &id, None, None, Some("DONE"), None, &[], &[])
            .await
            .unwrap();

        let content = fs::read_to_string(&path).unwrap();
        assert!(
            content.contains("CLOSED:"),
            "DONE transition should set CLOSED:, got:\n{content}"
        );
        assert!(
            content.contains(":LOGBOOK:"),
            "DONE transition should add :LOGBOOK: drawer, got:\n{content}"
        );
        assert!(
            content.contains("- State \"DONE\"       from \"TODO\""),
            "DONE transition should log state change, got:\n{content}"
        );
    }

    #[tokio::test]
    async fn test_update_logs_state_change_to_canceled() {
        let (db, _dir, notes, index) = test_env().await;
        let (path, id) = create_todo_task(&db, &notes, &index).await;

        run_update(&db, &notes, &index, &id, None, None, Some("CANCELED"), None, &[], &[])
            .await
            .unwrap();

        let content = fs::read_to_string(&path).unwrap();
        assert!(
            content.contains("CLOSED:"),
            "CANCELED transition should set CLOSED:, got:\n{content}"
        );
        assert!(
            content.contains("- State \"CANCELED\"       from \"TODO\""),
            "CANCELED transition should log state change, got:\n{content}"
        );
    }

    #[tokio::test]
    async fn test_update_someday_does_not_set_closed() {
        let (db, _dir, notes, index) = test_env().await;
        let (path, id) = create_todo_task(&db, &notes, &index).await;

        run_update(&db, &notes, &index, &id, None, None, Some("SOMEDAY"), None, &[], &[])
            .await
            .unwrap();

        let content = fs::read_to_string(&path).unwrap();
        assert!(
            !content.contains("CLOSED:"),
            "SOMEDAY is not a closed state — no CLOSED: line, got:\n{content}"
        );
        assert!(
            content.contains("- State \"SOMEDAY\"       from \"TODO\""),
            "SOMEDAY transition should still log state change, got:\n{content}"
        );
    }

    #[tokio::test]
    async fn test_update_reopen_clears_closed() {
        let (db, _dir, notes, index) = test_env().await;
        let (path, id) = create_todo_task(&db, &notes, &index).await;

        // Close the task first
        run_update(&db, &notes, &index, &id, None, None, Some("DONE"), None, &[], &[])
            .await
            .unwrap();
        let closed_content = fs::read_to_string(&path).unwrap();
        assert!(closed_content.contains("CLOSED:"));

        // Reopen it
        run_update(&db, &notes, &index, &id, None, None, Some("NEXT"), None, &[], &[])
            .await
            .unwrap();

        let reopened_content = fs::read_to_string(&path).unwrap();
        assert!(
            !reopened_content.contains("CLOSED:"),
            "Reopening should clear CLOSED:, got:\n{reopened_content}"
        );
        // Two logbook entries: one for closing, one for reopening
        let logbook_entries: Vec<_> = reopened_content
            .lines()
            .filter(|l| l.starts_with("- State "))
            .collect();
        assert_eq!(
            logbook_entries.len(),
            2,
            "Should have two state change entries (close + reopen), got:\n{reopened_content}"
        );
        assert!(
            reopened_content.contains("- State \"DONE\"       from \"TODO\""),
            "First entry should be the close transition, got:\n{reopened_content}"
        );
        assert!(
            reopened_content.contains("- State \"NEXT\"       from \"DONE\""),
            "Second entry should be the reopen transition, got:\n{reopened_content}"
        );
    }

    #[tokio::test]
    async fn test_update_title_only_preserves_logbook() {
        let (db, _dir, notes, index) = test_env().await;
        let (path, id) = create_todo_task(&db, &notes, &index).await;

        // Close the task to populate CLOSED + LOGBOOK
        run_update(&db, &notes, &index, &id, None, None, Some("DONE"), None, &[], &[])
            .await
            .unwrap();

        // Now update the title only, status unchanged (status=None means preserve)
        run_update(&db, &notes, &index, &id, Some("New title"), None, None, None, &[], &[])
            .await
            .unwrap();

        let after_title_update = fs::read_to_string(&path).unwrap();
        // CLOSED should still be present
        assert!(
            after_title_update.contains("CLOSED:"),
            "Title-only update should preserve CLOSED:, got:\n{after_title_update}"
        );
        // Logbook entry from the close should be preserved verbatim
        assert!(
            after_title_update.contains("- State \"DONE\"       from \"TODO\""),
            "Title-only update should preserve existing LOGBOOK entry, got:\n{after_title_update}"
        );
        // No new logbook entry should have been added (status unchanged)
        let entry_count = after_title_update
            .lines()
            .filter(|l| l.starts_with("- State "))
            .count();
        assert_eq!(
            entry_count, 1,
            "Title-only update should not add new state change entries, got:\n{after_title_update}"
        );
        // Title should be updated
        assert!(after_title_update.contains("New title"));
    }

    #[tokio::test]
    async fn test_update_status_noop_preserves_closed() {
        let (db, _dir, notes, index) = test_env().await;
        let (path, id) = create_todo_task(&db, &notes, &index).await;

        // Close the task
        run_update(&db, &notes, &index, &id, None, None, Some("DONE"), None, &[], &[])
            .await
            .unwrap();

        // "Update" to the same status (DONE -> DONE)
        run_update(&db, &notes, &index, &id, None, None, Some("DONE"), None, &[], &[])
            .await
            .unwrap();

        let content = fs::read_to_string(&path).unwrap();
        // No new logbook entry should be added when status doesn't change
        let entry_count = content
            .lines()
            .filter(|l| l.starts_with("- State "))
            .count();
        assert_eq!(
            entry_count, 1,
            "Status noop should not add a new state change entry, got:\n{content}"
        );
        // CLOSED should be preserved
        assert!(
            content.contains("CLOSED:"),
            "Status noop should preserve CLOSED:, got:\n{content}"
        );
    }

    #[tokio::test]
    async fn test_create_done_no_closed() {
        // New tasks created with a closed status should NOT get a CLOSED: line
        // or LOGBOOK entries — only state transitions (updates) record these.
        let (db, _dir, notes, index) = test_env().await;

        run_create(&db, &notes, &index, "Already done task", None, None, "DONE")
            .await
            .unwrap();

        let path = fs::read_dir(projects_dir(&notes)).unwrap().next().unwrap().unwrap().path();
        let content = fs::read_to_string(&path).unwrap();
        assert!(
            !content.contains("CLOSED:"),
            "New task created as DONE should not have CLOSED:, got:\n{content}"
        );
        assert!(
            !content.contains(":LOGBOOK:"),
            "New task created as DONE should not have a :LOGBOOK: drawer, got:\n{content}"
        );
    }

    // -----------------------------------------------------------------------
    // run_delete
    // -----------------------------------------------------------------------

    #[tokio::test]
    async fn test_delete_standalone_task() {
        let (db, _dir, notes, index) = test_env().await;

        run_create(&db, &notes, &index, "Temp task", None, None, "TODO")
            .await
            .unwrap();

        let path = fs::read_dir(projects_dir(&notes)).unwrap().next().unwrap().unwrap().path();
        let content = fs::read_to_string(&path).unwrap();
        let id_marker = ":ID:       ";
        let task_id_start = content.match_indices(id_marker).nth(1).unwrap().0 + id_marker.len();
        let id = content[task_id_start..].lines().next().unwrap().trim().to_string();

        run_delete(&db, &notes, &index, &id).await.unwrap();

        // File should still exist (as a note with no task headlines)
        assert!(path.exists());
        assert_eq!(headline_count(&path), 0);
    }

    #[tokio::test]
    async fn test_delete_project_task() {
        let (db, _dir, notes, index) = test_env().await;

        run_create(&db, &notes, &index, "Task one", None, Some("sprint-12"), "TODO")
            .await
            .unwrap();

        // Second create reuses the same project file
        run_create(&db, &notes, &index, "Task two", None, Some("sprint-12"), "TODO")
            .await
            .unwrap();

        let path = fs::read_dir(projects_dir(&notes)).unwrap().next().unwrap().unwrap().path();
        let content = fs::read_to_string(&path).unwrap();
        // Find the first headline's ID (second :ID: in file)
        let id_marker = ":ID:       ";
        let second_id_start = content.match_indices(id_marker).nth(1).unwrap().0 + id_marker.len();
        let id = content[second_id_start..].lines().next().unwrap().trim().to_string();

        run_delete(&db, &notes, &index, &id).await.unwrap();

        // One headline should remain
        assert_eq!(headline_count(&path), 1);
    }

    #[tokio::test]
    async fn test_delete_last_project_task_leaves_project_file() {
        let (db, _dir, notes, index) = test_env().await;

        run_create(&db, &notes, &index, "Only task", None, Some("sprint-12"), "TODO")
            .await
            .unwrap();

        let path = fs::read_dir(projects_dir(&notes)).unwrap().next().unwrap().unwrap().path();
        let content = fs::read_to_string(&path).unwrap();
        let id_marker = ":ID:       ";
        let second_id_start = content.match_indices(id_marker).nth(1).unwrap().0 + id_marker.len();
        let id = content[second_id_start..].lines().next().unwrap().trim().to_string();

        run_delete(&db, &notes, &index, &id).await.unwrap();

        // Project file should still exist (preamble remains)
        assert!(path.exists());
        assert_eq!(headline_count(&path), 0);
    }

    #[tokio::test]
    async fn test_delete_nonexistent_task() {
        let (db, _dir, notes, index) = test_env().await;

        let result = run_delete(&db, &notes, &index, "nonexistent-uuid").await;
        assert!(result.is_err());
    }

    // -----------------------------------------------------------------------
    // run_list
    // -----------------------------------------------------------------------

    /// Create a test database using the existing DB initialization functions.
    async fn test_db() -> (Connection, TempDir) {
        let dir = TempDir::new().unwrap();
        let vec_db_path = dir.path().to_str().unwrap().to_string();
        let db = crate::core::db::async_db(&vec_db_path).await.unwrap();
        db.call(|conn| {
            crate::core::db::initialize_db(conn).unwrap();
            Ok(())
        })
        .await
        .unwrap();
        (db, dir)
    }

    /// Create a test database + notes directory, returning the connection,
    /// temp dir guard, and notes path string.
    /// Notes are stored in a `notes/` subdirectory so DB files at the temp
    /// dir root don't interfere with file-count assertions.
    async fn test_env() -> (Connection, TempDir, String, String) {
        let dir = TempDir::new().unwrap();
        let notes = format!("{}/notes", dir.path().to_str().unwrap());
        let index = format!("{}/index", dir.path().to_str().unwrap());
        std::fs::create_dir_all(&notes).unwrap();
        std::fs::create_dir_all(format!("{notes}/projects")).unwrap();
        std::fs::create_dir_all(&index).unwrap();
        let db = crate::core::db::async_db(dir.path().to_str().unwrap()).await.unwrap();
        db.call(|conn| {
            crate::core::db::initialize_db(conn).unwrap();
            Ok(())
        })
        .await
        .unwrap();
        (db, dir, notes, index)
    }

    /// Task/project files are written under `notes/projects/`.
    fn projects_dir(notes: &str) -> String {
        format!("{notes}/projects")
    }

    /// Insert a task row into note_meta for testing.
    fn insert_task(
        conn: &rusqlite::Connection,
        id: &str,
        file_name: &str,
        title: &str,
        status: &str,
    ) {
        conn.execute(
            "INSERT INTO note_meta (id, file_name, title, type, status)
             VALUES (?1, ?2, ?3, 'task', ?4)",
            rusqlite::params![id, file_name, title, status],
        )
        .unwrap();
    }

    /// Insert a project row into note_meta for testing.
    fn insert_project(
        conn: &rusqlite::Connection,
        id: &str,
        file_name: &str,
        title: &str,
    ) {
        conn.execute(
            "INSERT INTO note_meta (id, file_name, title, type, tags)
             VALUES (?1, ?2, ?3, 'note', 'project')",
            rusqlite::params![id, file_name, title],
        )
        .unwrap();
    }

    #[tokio::test]
    async fn test_list_refile_and_capture_both_empty() {
        let (db, _dir) = test_db().await;

        // No tasks in the DB
        let result = run_list(&db, "", None, None).await;
        assert!(result.is_ok());
    }

    #[tokio::test]
    async fn test_list_refile_and_capture_with_tasks() {
        let (db, _dir) = test_db().await;
        db.call(|conn| {
            insert_task(conn, "task-1", "projects/refile.org", "Buy groceries", "todo");
            insert_task(conn, "task-2", "projects/refile.org", "Fix login", "done");
            insert_task(conn, "task-3", "projects/capture.org", "Quick idea", "todo");
            Ok(())
        })
        .await
        .unwrap();

        let result = run_list(&db, "", None, None).await;
        assert!(result.is_ok());
    }

    #[tokio::test]
    async fn test_list_refile_only() {
        let (db, _dir) = test_db().await;
        db.call(|conn| {
            insert_task(conn, "t1", "projects/refile.org", "Task one", "todo");
            insert_task(conn, "t2", "projects/refile.org", "Task two", "done");
            insert_task(conn, "t3", "projects/capture.org", "Capture task", "todo");
            Ok(())
        })
        .await
        .unwrap();

        let result = run_list(&db, "", Some("refile"), None).await;
        assert!(result.is_ok());
    }

    #[tokio::test]
    async fn test_list_capture_only() {
        let (db, _dir) = test_db().await;
        db.call(|conn| {
            insert_task(conn, "t1", "projects/refile.org", "Refile task", "todo");
            insert_task(conn, "t2", "projects/capture.org", "Capture task", "todo");
            Ok(())
        })
        .await
        .unwrap();

        let result = run_list(&db, "", Some("capture"), None).await;
        assert!(result.is_ok());
    }

    #[tokio::test]
    async fn test_list_special_file_personal() {
        let (db, _dir) = test_db().await;
        db.call(|conn| {
            insert_task(conn, "p1", "projects/personal.org", "Personal task", "todo");
            insert_task(conn, "w1", "projects/work.org", "Work task", "todo");
            insert_task(conn, "t1", "projects/capture.org", "Capture task", "todo");
            Ok(())
        })
        .await
        .unwrap();

        // personal and work are special files — listing them should target
        // their bare filenames, not require a dated --project-* registration.
        let result = run_list(&db, "", Some("personal"), None).await;
        assert!(result.is_ok());
        let result = run_list(&db, "", Some("work"), None).await;
        assert!(result.is_ok());
    }

    #[tokio::test]
    async fn test_list_refile_filter_by_status() {
        let (db, _dir) = test_db().await;
        db.call(|conn| {
            insert_task(conn, "t1", "projects/refile.org", "Task one", "todo");
            insert_task(conn, "t2", "projects/refile.org", "Task two", "done");
            insert_task(conn, "t3", "projects/refile.org", "Task three", "todo");
            Ok(())
        })
        .await
        .unwrap();

        let result = run_list(&db, "", None, Some("TODO")).await;
        assert!(result.is_ok());
    }

    #[tokio::test]
    async fn test_list_project_not_found() {
        let (db, _dir) = test_db().await;

        let result = run_list(&db, "", Some("nonexistent"), None).await;
        assert!(result.is_err(), "missing project should error");
    }

    #[tokio::test]
    async fn test_list_project_file() {
        let (db, dir) = test_db().await;
        let notes = dir.path().to_str().unwrap().to_string();

        // Create a project file on disk (find_project_file_by_id_or_name needs it)
        let project_path = dir.path().join("2026-05-31--project-my-project.org");
        tokio::fs::write(
            &project_path,
            "\
:PROPERTIES:
:ID:       proj-1
:END:
#+TITLE: my-project
#+CATEGORY: my-project
#+DATE: 2026-06-01
#+FILETAGS: private project

* TODO First task
:PROPERTIES:
:ID:       pt-1
:END:

* DONE Second task
:PROPERTIES:
:ID:       pt-2
:END:
",
        )
        .await
        .unwrap();

        // Insert project and tasks into the DB
        db.call(|conn| {
            insert_project(conn, "proj-1", "2026-05-31--project-my-project.org", "my-project");
            insert_task(conn, "pt-1", "2026-05-31--project-my-project.org", "First task", "todo");
            insert_task(conn, "pt-2", "2026-05-31--project-my-project.org", "Second task", "done");
            Ok(())
        })
        .await
        .unwrap();

        let result = run_list(&db, &notes, Some("my-project"), None).await;
        assert!(result.is_ok());
    }

    #[tokio::test]
    async fn test_list_project_filter_by_status() {
        let (db, dir) = test_db().await;
        let notes = dir.path().to_str().unwrap().to_string();

        let project_path = dir.path().join("2026-05-31--project-sprint-12.org");
        tokio::fs::write(
            &project_path,
            "\
:PROPERTIES:
:ID:       proj-1
:END:
#+TITLE: sprint-12
#+CATEGORY: sprint-12
#+DATE: 2026-06-01
#+FILETAGS: private project

* TODO Task A
:PROPERTIES:
:ID:       a
:END:

* DONE Task B
:PROPERTIES:
:ID:       b
:END:

* TODO Task C
:PROPERTIES:
:ID:       c
:END:
",
        )
        .await
        .unwrap();

        db.call(|conn| {
            insert_project(conn, "proj-1", "2026-05-31--project-sprint-12.org", "sprint-12");
            insert_task(conn, "a", "2026-05-31--project-sprint-12.org", "Task A", "todo");
            insert_task(conn, "b", "2026-05-31--project-sprint-12.org", "Task B", "done");
            insert_task(conn, "c", "2026-05-31--project-sprint-12.org", "Task C", "todo");
            Ok(())
        })
        .await
        .unwrap();

        let result = run_list(&db, &notes, Some("sprint-12"), Some("DONE")).await;
        assert!(result.is_ok());
    }

    // -----------------------------------------------------------------------
    // find_task_in_file — now delegated to core::orgmode
    // -----------------------------------------------------------------------

    #[tokio::test]
    async fn test_find_task_in_file_found() {
        let dir = TempDir::new().unwrap();
        let path = dir.path().join("project.org");
        fs::write(
            &path,
            "\
:PROPERTIES:
:ID:       proj-1
:END:
#+TITLE: My Project
#+FILETAGS: private project

* TODO Fix login
:PROPERTIES:
:ID:       task-1
:END:
Investigate the redirect

* DONE Setup CI
:PROPERTIES:
:ID:       task-2
:END:
",
        )
        .unwrap();

        let location = orgmode::find_task_in_file(&path, "task-1").await.unwrap();
        assert_eq!(location.current_title, "Fix login");
        assert_eq!(location.current_status, "TODO");
        assert!(location.current_body.contains("Investigate the redirect"));
        assert_eq!(location.current_level, 1);
    }

    #[tokio::test]
    async fn test_find_task_in_file_not_found() {
        let dir = TempDir::new().unwrap();
        let path = dir.path().join("project.org");
        fs::write(
            &path,
            "\
:PROPERTIES:
:ID:       proj-1
:END:
",
        )
        .unwrap();

        let result = orgmode::find_task_in_file(&path, "nonexistent").await;
        assert!(result.is_err());
    }

    #[tokio::test]
    async fn test_find_task_in_file_empty_file() {
        let dir = TempDir::new().unwrap();
        let path = dir.path().join("empty.org");
        fs::write(&path, "").unwrap();

        let result = orgmode::find_task_in_file(&path, "task-1").await;
        assert!(result.is_err());
    }

    // -----------------------------------------------------------------------
    // run_update — with --project flag
    // -----------------------------------------------------------------------

    #[tokio::test]
    async fn test_update_project_task_by_filename() {
        let (db, _dir, notes, index) = test_env().await;

        run_create(&db, &notes, &index, "Fix bug", None, Some("sprint-12"), "TODO")
            .await
            .unwrap();

        // Find the task ID from the project file
        let path = fs::read_dir(projects_dir(&notes)).unwrap().next().unwrap().unwrap().path();
        let content = fs::read_to_string(&path).unwrap();
        let id_marker = ":ID:       ";
        let second_id_start = content.match_indices(id_marker).nth(1).unwrap().0 + id_marker.len();
        let id = content[second_id_start..].lines().next().unwrap().trim().to_string();

        let filename = path
            .strip_prefix(&notes)
            .unwrap_or_else(|_| path.as_path())
            .to_str()
            .unwrap()
            .to_string();

        // Update using --project with filename
        run_update(&db, &notes, &index, &id, Some("Fixed bug"), None, Some("DONE"), Some(&filename), &[], &[])            .await
            .unwrap();

        let config = parsing_config();
        let org = config.parse(&fs::read_to_string(&path).unwrap());
        let headlines: Vec<_> = org.document().headlines().collect();
        assert_eq!(headlines.len(), 1);
        assert_eq!(headlines[0].todo_keyword().unwrap().to_string(), "DONE");
        assert_eq!(headlines[0].title_raw().trim(), "Fixed bug");
    }

    #[tokio::test]
    async fn test_update_project_task_by_id() {
        let (db, _dir, notes, index) = test_env().await;

        run_create(&db, &notes, &index, "Add tests", None, Some("sprint-12"), "TODO")
            .await
            .unwrap();

        // Find the project file's ID (first :ID: in the file)
        let path = fs::read_dir(projects_dir(&notes)).unwrap().next().unwrap().unwrap().path();
        let content = fs::read_to_string(&path).unwrap();
        let id_marker = ":ID:       ";
        let first_id_start = content.find(id_marker).unwrap() + id_marker.len();
        let project_id = content[first_id_start..].lines().next().unwrap().trim().to_string();

        // Find the task ID (second :ID:)
        let second_id_start = content.match_indices(id_marker).nth(1).unwrap().0 + id_marker.len();
        let task_id = content[second_id_start..].lines().next().unwrap().trim().to_string();

        // Update using --project with project ID
        run_update(&db, &notes, &index, &task_id, None, None, Some("DONE"), Some(&project_id), &[], &[])            .await
            .unwrap();

        let config = parsing_config();
        let org = config.parse(&fs::read_to_string(&path).unwrap());
        let headlines: Vec<_> = org.document().headlines().collect();
        assert_eq!(headlines.len(), 1);
        assert_eq!(headlines[0].todo_keyword().unwrap().to_string(), "DONE");
    }

    #[tokio::test]
    async fn test_update_project_task_not_found_in_project() {
        let (db, _dir, notes, index) = test_env().await;

        // Create a standalone task (not in any project)
        run_create(&db, &notes, &index, "Standalone", None, None, "TODO")
            .await
            .unwrap();

        // Create a project with a different task
        run_create(&db, &notes, &index, "Project task", None, Some("my-project"), "TODO")
            .await
            .unwrap();

        // Find the standalone task's ID
        let standalone_path = fs::read_dir(projects_dir(&notes))
            .unwrap()
            .filter_map(|e| {
                let p = e.unwrap().path();
                let n = p.file_name().unwrap().to_str().unwrap().to_string();
                if n.contains("--project-") { None } else { Some(p) }
            })
            .next()
            .unwrap();
        let content = fs::read_to_string(&standalone_path).unwrap();
        let id_start = content.match_indices(":ID:       ").nth(1).unwrap().0 + ":ID:       ".len();
        let task_id = content[id_start..].lines().next().unwrap().trim().to_string();

        // Updating scoped to a project where the task doesn't exist should error
        let result = run_update(&db, &notes, &index, &task_id, None, None, Some("DONE"), Some("my-project"), &[], &[]).await;
        assert!(result.is_err(), "should error when task not found in scoped project file");
    }

    #[tokio::test]
    async fn test_update_project_task_nonexistent_project() {
        let (db, _dir, notes, index) = test_env().await;

        let result = run_update(&db, &notes, &index, "some-id", None, None, Some("DONE"), Some("nonexistent"), &[], &[]).await;
        assert!(result.is_err(), "should error for nonexistent project");
    }

    // -----------------------------------------------------------------------
    // run_refile
    // -----------------------------------------------------------------------

    #[tokio::test]
    async fn test_refile_standalone_to_project() {
        let (db, _dir, notes, index) = test_env().await;

        run_create(&db, &notes, &index, "Buy groceries", Some("Milk, eggs, bread"), None, "TODO")
            .await
            .unwrap();

        // Find the standalone task's ID
        let standalone_path = fs::read_dir(projects_dir(&notes))
            .unwrap()
            .filter_map(|e| {
                let p = e.unwrap().path();
                let n = p.file_name().unwrap().to_str().unwrap().to_string();
                if n.contains("--project-") { None } else { Some(p) }
            })
            .next()
            .unwrap();
        let content = fs::read_to_string(&standalone_path).unwrap();
        let id_start = content.match_indices(":ID:       ").nth(1).unwrap().0 + ":ID:       ".len();
        let task_id = content[id_start..].lines().next().unwrap().trim().to_string();

        // Refile to a project
        run_refile(&db, &notes, &index, &task_id, "errands").await.unwrap();

        // Task headline should be removed from the standalone file
        // (document-level preamble with #+TITLE: may retain the title)
        let standalone_content = fs::read_to_string(&standalone_path).unwrap();
        assert!(!standalone_content.contains("Milk, eggs, bread"),
            "headline body should not remain in source");
        assert_eq!(headline_count(&standalone_path), 0);

        // Task should now be in the project file
        let project_path = fs::read_dir(projects_dir(&notes))
            .unwrap()
            .filter_map(|e| {
                let p = e.unwrap().path();
                let n = p.file_name().unwrap().to_str().unwrap().to_string();
                if n.contains("--project-errands") { Some(p) } else { None }
            })
            .next()
            .unwrap();
        let project_content = fs::read_to_string(&project_path).unwrap();
        assert!(project_content.contains(&task_id));
        assert!(project_content.contains("Buy groceries"));
        assert!(project_content.contains("Milk, eggs, bread"));
        assert_eq!(headline_count(&project_path), 1);
    }

    #[tokio::test]
    async fn test_refile_from_one_project_to_another() {
        let (db, _dir, notes, index) = test_env().await;

        run_create(&db, &notes, &index, "Fix login bug", None, Some("sprint-12"), "TODO")
            .await
            .unwrap();

        // Find the task ID from the project file
        let project_path = fs::read_dir(projects_dir(&notes))
            .unwrap()
            .filter_map(|e| {
                let p = e.unwrap().path();
                let n = p.file_name().unwrap().to_str().unwrap().to_string();
                if n.contains("--project-sprint-12") { Some(p) } else { None }
            })
            .next()
            .unwrap();
        let content = fs::read_to_string(&project_path).unwrap();
        let id_marker = ":ID:       ";
        let second_id_start = content.match_indices(id_marker).nth(1).unwrap().0 + id_marker.len();
        let task_id = content[second_id_start..].lines().next().unwrap().trim().to_string();

        // Refile to a different project
        run_refile(&db, &notes, &index, &task_id, "security").await.unwrap();

        // Task should no longer be in the original project
        let sprint_content = fs::read_to_string(&project_path).unwrap();
        assert!(!sprint_content.contains(&task_id));
        assert_eq!(headline_count(&project_path), 0);

        // Task should now be in the new project
        let security_path = fs::read_dir(projects_dir(&notes))
            .unwrap()
            .filter_map(|e| {
                let p = e.unwrap().path();
                let n = p.file_name().unwrap().to_str().unwrap().to_string();
                if n.contains("--project-security") { Some(p) } else { None }
            })
            .next()
            .unwrap();
        let security_content = fs::read_to_string(&security_path).unwrap();
        assert!(security_content.contains(&task_id));
        assert!(security_content.contains("Fix login bug"));
        assert_eq!(headline_count(&security_path), 1);
    }

    /// Refiling a task out of an archive file must be blocked (archives are
    /// read-only for refile purposes).
    #[tokio::test]
    async fn test_refile_from_archive_blocked() {
        let (db, _dir, notes, index) = test_env().await;

        let archive_path = format!("{}/projects/work.org_archive", notes);
        std::fs::write(
            &archive_path,
            "Archived entries from file work.org\n\n* DONE Old archived task\n:PROPERTIES:\n:ARCHIVE_TIME: 2022-10-16 Sun 09:37\n:ID: archived-task\n:END:\n",
        )
        .unwrap();
        db.call(|conn| {
            insert_task(conn, "archived-task", "projects/work.org_archive", "Old archived task", "done");
            Ok(())
        })
        .await
        .unwrap();

        let err = run_refile(&db, &notes, &index, "archived-task", "errands")
            .await
            .unwrap_err();
        assert!(
            err.to_string().contains("archive"),
            "expected archive guard error, got: {err}"
        );
    }

    #[tokio::test]
    async fn test_refile_twice_keeps_index_in_sync() {
        let (db, _dir, notes, index) = test_env().await;

        run_create(&db, &notes, &index, "Move me", None, None, "TODO")
            .await
            .unwrap();

        // Find the standalone task's ID (document-level :ID: is first, task is second)
        let capture_path = std::path::Path::new(&notes).join("projects").join("capture.org");
        let capture = fs::read_to_string(&capture_path).unwrap();
        let id_start = capture.match_indices(":ID:       ").nth(1).unwrap().0 + ":ID:       ".len();
        let task_id = capture[id_start..].lines().next().unwrap().trim().to_string();

        // Refile to project alpha, then refile again to project beta. The second
        // refile depends on find_task locating the task in alpha — i.e. the index
        // must have been updated by the first refile.
        run_refile(&db, &notes, &index, &task_id, "alpha").await.unwrap();
        run_refile(&db, &notes, &index, &task_id, "beta").await.unwrap();

        // find_task (index-driven, no fallback) must point at the beta file.
        let loc = orgmode::find_task(&db, &notes, &task_id).await.unwrap();
        let file_name = loc.path.file_name().unwrap().to_str().unwrap().to_string();
        assert!(
            file_name.contains("--project-beta"),
            "task should now live in beta, find_task resolved to {file_name}"
        );

        // Task appears exactly once, only in beta.
        let beta = fs::read_dir(projects_dir(&notes))
            .unwrap()
            .filter_map(|e| {
                let p = e.unwrap().path();
                let n = p.file_name().unwrap().to_str().unwrap().to_string();
                if n.contains("--project-beta") { Some(p) } else { None }
            })
            .next()
            .unwrap();
        let beta_content = fs::read_to_string(&beta).unwrap();
        assert_eq!(beta_content.matches(&task_id).count(), 1);
        assert_eq!(headline_count(&beta), 1);
    }

    #[tokio::test]
    async fn test_refile_preserves_org_structure() {
        let (db, _dir, notes, index) = test_env().await;

        // Create a task with body text via refile.org
        let refile_path = std::path::Path::new(&notes).join("projects").join("refile.org");
        fs::write(
            &refile_path,
            "\
:PROPERTIES:
:ID:       refile
:END:
#+TITLE: Refile

* TODO Research API design
:PROPERTIES:
:ID:       task-with-body
:END:
Look into REST vs GraphQL options.
Consider authentication requirements.
",
        )
        .unwrap();
        index_single_file(&db, &index, &notes, refile_path.clone()).await.unwrap();

        // Refile to a project
        run_refile(&db, &notes, &index, "task-with-body", "research").await.unwrap();

        // Read the project file
        let project_path = fs::read_dir(projects_dir(&notes))
            .unwrap()
            .filter_map(|e| {
                let p = e.unwrap().path();
                let n = p.file_name().unwrap().to_str().unwrap().to_string();
                if n.contains("--project-research") { Some(p) } else { None }
            })
            .next()
            .unwrap();
        let project_content = fs::read_to_string(&project_path).unwrap();

        // All org structure should be preserved verbatim
        assert!(project_content.contains("TODO Research API design"));
        assert!(project_content.contains(":ID:       task-with-body"));
        assert!(project_content.contains("Look into REST vs GraphQL options."));
        assert!(project_content.contains("Consider authentication requirements."));

        // Source file should no longer have the task
        let refile_content = fs::read_to_string(&refile_path).unwrap();
        assert!(!refile_content.contains("task-with-body"));
    }

    #[tokio::test]
    async fn test_refile_to_same_project_errors() {
        let (db, _dir, notes, index) = test_env().await;

        run_create(&db, &notes, &index, "Task in project", None, Some("my-project"), "TODO")
            .await
            .unwrap();

        // Find the task ID
        let project_path = fs::read_dir(projects_dir(&notes))
            .unwrap()
            .filter_map(|e| {
                let p = e.unwrap().path();
                let n = p.file_name().unwrap().to_str().unwrap().to_string();
                if n.contains("--project-my-project") { Some(p) } else { None }
            })
            .next()
            .unwrap();
        let content = fs::read_to_string(&project_path).unwrap();
        let id_marker = ":ID:       ";
        let second_id_start = content.match_indices(id_marker).nth(1).unwrap().0 + id_marker.len();
        let task_id = content[second_id_start..].lines().next().unwrap().trim().to_string();

        // Refiling to the same project should fail
        let result = run_refile(&db, &notes, &index, &task_id, "my-project").await;
        assert!(result.is_err(), "should error when refiling to same project");
        assert!(
            result.unwrap_err().to_string().contains("already in project"),
            "error should mention already-in-project"
        );
    }

    #[tokio::test]
    async fn test_refile_nonexistent_task() {
        let (db, _dir, notes, index) = test_env().await;

        let result = run_refile(&db, &notes, &index, "nonexistent-uuid", "some-project").await;
        assert!(result.is_err());
    }

    #[tokio::test]
    async fn test_refile_from_refile_org_to_project() {
        let (db, _dir, notes, index) = test_env().await;

        // Simulate a task sitting in refile.org
        let refile_path = std::path::Path::new(&notes).join("projects").join("refile.org");
        fs::write(
            &refile_path,
            "\
:PROPERTIES:
:ID:       inbox
:END:
#+TITLE: Refile

* TODO Review PR
:PROPERTIES:
:ID:       review-pr-42
:END:
Need to check the middleware changes.

* DONE Setup CI
:PROPERTIES:
:ID:       setup-ci
:END:
",
        )
        .unwrap();
        index_single_file(&db, &index, &notes, refile_path.clone()).await.unwrap();

        // Refile one task to a project
        run_refile(&db, &notes, &index, "review-pr-42", "ops").await.unwrap();

        // The refiled task should be gone from refile.org
        let refile_content = fs::read_to_string(&refile_path).unwrap();
        assert!(!refile_content.contains("review-pr-42"));
        assert!(!refile_content.contains("Review PR"));

        // The other task should remain in refile.org
        assert!(refile_content.contains("setup-ci"));
        assert!(refile_content.contains("Setup CI"));

        // The refiled task should be in the project file
        let project_path = fs::read_dir(projects_dir(&notes))
            .unwrap()
            .filter_map(|e| {
                let p = e.unwrap().path();
                let n = p.file_name().unwrap().to_str().unwrap().to_string();
                if n.contains("--project-ops") { Some(p) } else { None }
            })
            .next()
            .unwrap();
        let project_content = fs::read_to_string(&project_path).unwrap();
        assert!(project_content.contains("review-pr-42"));
        assert!(project_content.contains("Review PR"));
        assert!(project_content.contains("middleware changes"));
        assert_eq!(headline_count(&project_path), 1);
    }

    /// Concurrent refiles of several tasks from the same capture.org must not
    /// lose updates: every task ends up in the project exactly once, and none
    /// remain behind in the source (regression test for the lost-update / task
    /// duplication seen when refiles raced the git sync).
    #[tokio::test(flavor = "multi_thread", worker_threads = 8)]
    async fn test_refile_concurrent_from_same_file_no_duplication() {
        let (db, _dir, notes, index) = test_env().await;

        // Create several standalone tasks in capture.org.
        for i in 0..8 {
            run_create(&db, &notes, &index, &format!("Task {i}"), Some("body"), None, "TODO")
                .await
                .unwrap();
        }
        let capture_path = std::path::Path::new(&notes).join("projects").join("capture.org");
        let capture = fs::read_to_string(&capture_path).unwrap();
        // Skip the document-level :ID: (first) and collect each task's ID.
        let ids: Vec<String> = capture
            .match_indices(":ID:")
            .skip(1)
            .filter_map(|(i, _)| {
                let rest = &capture[i + ":ID:".len()..];
                let line = rest.lines().next()?.trim().to_string();
                Some(line)
            })
            .collect();
        assert_eq!(ids.len(), 8, "expected 8 task IDs, got: {ids:?}");

        // Refile all of them concurrently to the same project.
        let mut set = tokio::task::JoinSet::new();
        for id in ids.clone() {
            let db = db.clone();
            let notes = notes.clone();
            let index = index.clone();
            set.spawn(async move {
                run_refile(&db, &notes, &index, &id, "errands").await.unwrap();
            });
        }
        while let Some(_) = set.join_next().await {}

        // Every task must appear in the project file exactly once.
        let project_path = fs::read_dir(projects_dir(&notes))
            .unwrap()
            .filter_map(|e| {
                let p = e.unwrap().path();
                let n = p.file_name().unwrap().to_str().unwrap().to_string();
                if n.contains("--project-errands") { Some(p) } else { None }
            })
            .next()
            .expect("errands project file should exist");
        let project = fs::read_to_string(&project_path).unwrap();
        for id in &ids {
            assert_eq!(
                project.matches(id).count(),
                1,
                "task {id} should appear exactly once in project, got:\n{project}"
            );
        }
        assert_eq!(headline_count(&project_path), 8);

        // None should remain in capture.org (no lost updates).
        let capture = fs::read_to_string(&capture_path).unwrap();
        for id in &ids {
            assert!(
                !capture.contains(id.as_str()),
                "task {id} should have been removed from capture.org, got:\n{capture}"
            );
        }
        assert_eq!(headline_count(&capture_path), 0);
    }

    // -----------------------------------------------------------------------
    // Special files (capture, refile, personal, work) — bare filenames with
    // no date. Creating/refiling into these must write to `{name}.org`, not
    // create a new dated `--project-{slug}.org` file.
    // -----------------------------------------------------------------------

    #[tokio::test]
    async fn test_create_into_existing_special_file() {
        let (db, _dir, notes, index) = test_env().await;

        // Pre-existing special file with no date in its filename
        let work_path = std::path::Path::new(&notes).join("projects").join("work.org");
        fs::write(
            &work_path,
            "\
:PROPERTIES:
:ID:       work-file
:END:
#+TITLE: work

* TODO Existing task
:PROPERTIES:
:ID:       existing-1
:END:
",
        )
        .unwrap();

        run_create(&db, &notes, &index, "New work task", None, Some("work"), "TODO")
            .await
            .unwrap();

        // No new dated project file — only work.org exists, with both tasks
        let entries: Vec<_> = fs::read_dir(projects_dir(&notes)).unwrap().collect();
        assert_eq!(entries.len(), 1);
        let path = entries[0].as_ref().unwrap().path();
        assert_eq!(path.file_name().unwrap().to_str().unwrap(), "work.org");
        let content = fs::read_to_string(&path).unwrap();
        assert!(content.contains("New work task"));
        assert!(content.contains("Existing task"));
        assert_eq!(headline_count(&path), 2);
    }

    #[tokio::test]
    async fn test_create_special_file_creates_bare_name() {
        let (db, _dir, notes, index) = test_env().await;

        run_create(&db, &notes, &index, "Personal task", None, Some("personal"), "TODO")
            .await
            .unwrap();

        // Special file is created with its bare name, not a dated --project-* name
        let entries: Vec<_> = fs::read_dir(projects_dir(&notes)).unwrap().collect();
        assert_eq!(entries.len(), 1);
        let path = entries[0].as_ref().unwrap().path();
        assert_eq!(path.file_name().unwrap().to_str().unwrap(), "personal.org");
        assert!(!path.to_str().unwrap().contains("--project-"));
    }

    #[tokio::test]
    async fn test_refile_to_existing_special_file() {
        let (db, _dir, notes, index) = test_env().await;

        // Task in capture.org
        let capture_path = std::path::Path::new(&notes).join("projects").join("capture.org");
        fs::write(
            &capture_path,
            "\
:PROPERTIES:
:ID:       capture
:END:
#+TITLE: capture

* TODO Refile me
:PROPERTIES:
:ID:       special-task
:END:
",
        )
        .unwrap();
        index_single_file(&db, &index, &notes, capture_path.clone()).await.unwrap();

        // Pre-existing work.org
        let work_path = std::path::Path::new(&notes).join("projects").join("work.org");
        fs::write(
            &work_path,
            "\
:PROPERTIES:
:ID:       work-file
:END:
#+TITLE: work

* TODO Work task
:PROPERTIES:
:ID:       work-task
:END:
",
        )
        .unwrap();

        run_refile(&db, &notes, &index, "special-task", "work").await.unwrap();

        // Task removed from capture.org, appended to work.org
        let capture_content = fs::read_to_string(&capture_path).unwrap();
        assert!(!capture_content.contains("special-task"));
        let work_content = fs::read_to_string(&work_path).unwrap();
        assert!(work_content.contains("special-task"));
        assert!(work_content.contains("Refile me"));

        // No new dated project file created
        let entries: Vec<_> = fs::read_dir(projects_dir(&notes)).unwrap().collect();
        assert_eq!(entries.len(), 2);
    }

    #[tokio::test]
    async fn test_refile_creates_special_file_when_missing() {
        let (db, _dir, notes, index) = test_env().await;

        let capture_path = std::path::Path::new(&notes).join("projects").join("capture.org");
        fs::write(
            &capture_path,
            "\
:PROPERTIES:
:ID:       capture
:END:
#+TITLE: capture

* TODO Refile me
:PROPERTIES:
:ID:       special-task
:END:
",
        )
        .unwrap();
        index_single_file(&db, &index, &notes, capture_path.clone()).await.unwrap();

        run_refile(&db, &notes, &index, "special-task", "personal").await.unwrap();

        // personal.org created with bare name (no date prefix)
        let personal_path = std::path::Path::new(&notes).join("projects").join("personal.org");
        assert!(personal_path.exists());
        let content = fs::read_to_string(&personal_path).unwrap();
        assert!(content.contains("special-task"));
        assert!(content.contains("Refile me"));
        assert!(!personal_path.to_str().unwrap().contains("--project-"));
    }

    // -----------------------------------------------------------------------
    // Tag operations (run_update with add_tags / remove_tags)
    // -----------------------------------------------------------------------

    /// Helper: create a TODO task and return (path, id) so tag tests can drive
    /// subsequent updates without re-deriving the ID. Mirrors `create_todo_task` above
    /// but kept separate so existing tests don't depend on a helper defined further down.
    async fn create_task_for_tags(db: &Connection, notes: &str, index: &str) -> (PathBuf, String) {
        run_create(db, notes, &index, "Tag test task", None, None, "TODO")
            .await
            .unwrap();
        let path = fs::read_dir(projects_dir(notes)).unwrap().next().unwrap().unwrap().path();
        let content = fs::read_to_string(&path).unwrap();
        let id_marker = ":ID:       ";
        let task_id_start = content.match_indices(id_marker).nth(1).unwrap().0 + id_marker.len();
        let id = content[task_id_start..].lines().next().unwrap().trim().to_string();
        (path, id)
    }

    /// Read a task file and return the headline's tags as a Vec<String> (in order).
    fn read_headline_tags(path: &std::path::Path) -> Vec<String> {
        let content = fs::read_to_string(path).unwrap();
        let config = parsing_config();
        let org = config.parse(&content);
        org.document()
            .headlines()
            .next()
            .expect("task file should have at least one headline")
            .tags()
            .map(|t| t.to_string())
            .collect()
    }

    #[tokio::test]
    async fn test_update_add_tag_to_task_without_tags() {
        let (db, _dir, notes, index) = test_env().await;
        let (path, id) = create_task_for_tags(&db, &notes, &index).await;

        // Task starts with no tags
        assert!(read_headline_tags(&path).is_empty());

        run_update(&db, &notes, &index, &id, None, None, None, None, &["urgent".to_string()], &[])
        .await
        .unwrap();

        let tags = read_headline_tags(&path);
        assert_eq!(tags, vec!["urgent".to_string()], "added tag should appear, got: {tags:?}");
    }

    #[tokio::test]
    async fn test_update_remove_tag() {
        let (db, _dir, notes, index) = test_env().await;
        let (path, id) = create_task_for_tags(&db, &notes, &index).await;

        // Add a tag first
        run_update(&db, &notes, &index, &id, None, None, None, None, &["urgent".to_string()], &[])
        .await
        .unwrap();

        // Then remove it
        run_update(&db, &notes, &index, &id, None, None, None, None, &[], &["urgent".to_string()])
        .await
        .unwrap();

        let tags = read_headline_tags(&path);
        assert!(tags.is_empty(), "after removing the only tag, headline should have no tags, got: {tags:?}");
    }

    #[tokio::test]
    async fn test_update_add_and_remove_in_one_call() {
        let (db, _dir, notes, index) = test_env().await;
        let (path, id) = create_task_for_tags(&db, &notes, &index).await;

        // Start with tags a and b
        run_update(&db, &notes, &index, &id, None, None, None, None, &["a".to_string(), "b".to_string()], &[])
        .await
        .unwrap();

        // In one call: add c, remove a — result should be b and c.
        run_update(&db, &notes, &index, &id, None, None, None, None, &["c".to_string()], &["a".to_string()])
        .await
        .unwrap();

        let tags = read_headline_tags(&path);
        assert_eq!(tags, vec!["b".to_string(), "c".to_string()]);
    }

    #[tokio::test]
    async fn test_update_remove_nonexistent_tag_silent() {
        let (db, _dir, notes, index) = test_env().await;
        let (path, id) = create_task_for_tags(&db, &notes, &index).await;

        // Add one tag so we have something to verify
        run_update(&db, &notes, &index, &id, None, None, None, None, &["urgent".to_string()], &[])
        .await
        .unwrap();

        // Removing a tag that isn't set should be a silent no-op (no error).
        run_update(&db, &notes, &index, &id, None, None, None, None, &[], &["nonexistent".to_string()])
        .await
        .unwrap();

        let tags = read_headline_tags(&path);
        assert_eq!(tags, vec!["urgent".to_string()], "nonexistent removal should leave existing tags unchanged, got: {tags:?}");
    }

    #[tokio::test]
    async fn test_update_preserves_tags_when_updating_title() {
        let (db, _dir, notes, index) = test_env().await;
        let (path, id) = create_task_for_tags(&db, &notes, &index).await;

        // Add a tag
        run_update(&db, &notes, &index, &id, None, None, None, None, &["urgent".to_string()], &[])
        .await
        .unwrap();

        // Update the title only (no add/remove tag flags) — existing tags should be preserved.
        run_update(&db, &notes, &index, &id, Some("Renamed task"), None, None, None, &[], &[])
        .await
        .unwrap();

        // Title should be updated AND tags should still be present.
        let content = fs::read_to_string(&path).unwrap();
        assert!(content.contains("Renamed task"), "title should be updated");
        let tags = read_headline_tags(&path);
        assert_eq!(
            tags,
            vec!["urgent".to_string()],
            "title-only update should preserve existing tags, got: {tags:?}"
        );
    }

    #[tokio::test]
    async fn test_update_add_duplicate_tag_dedup() {
        let (db, _dir, notes, index) = test_env().await;
        let (path, id) = create_task_for_tags(&db, &notes, &index).await;

        // Add the same tag that's already present — should dedupe to one.
        run_update(&db, &notes, &index, &id, None, None, None, None, &["urgent".to_string()], &[])
        .await
        .unwrap();

        // Add the same tag again — should NOT result in two "urgent" entries.
        run_update(&db, &notes, &index, &id, None, None, None, None, &["urgent".to_string()], &[])
        .await
        .unwrap();

        let tags = read_headline_tags(&path);
        assert_eq!(tags, vec!["urgent".to_string()], "duplicate add should be deduped to a single entry, got: {tags:?}");
    }

    #[tokio::test]
    async fn test_update_add_tag_with_spaces_errors() {
        let (db, _dir, notes, index) = test_env().await;
        let (_path, id) = create_task_for_tags(&db, &notes, &index).await;

        // Adding a tag with spaces should error (validation runs in compute_new_tags).
        let result = run_update(&db, &notes, &index, &id, None, None, None, None, &["ur gent".to_string()], &[])
        .await;
        let err = result.unwrap_err();
        assert!(
            err.to_string().contains("spaces"),
            "error should mention spaces, got: {err}"
        );
    }

    #[tokio::test]
    async fn test_update_add_tag_with_special_chars_errors() {
        let (db, _dir, notes, index) = test_env().await;
        let (_path, id) = create_task_for_tags(&db, &notes, &index).await;

        // Adding a tag with special chars should error.
        let result = run_update(&db, &notes, &index, &id, None, None, None, None, &["urgent!".to_string()], &[])
        .await;
        let err = result.unwrap_err();
        assert!(
            err.to_string().contains("special character"),
            "error should mention special character, got: {err}"
        );
    }

    #[tokio::test]
    async fn test_update_add_tag_uppercase_auto_lowercased() {
        let (db, _dir, notes, index) = test_env().await;
        let (path, id) = create_task_for_tags(&db, &notes, &index).await;

        // Uppercase tags should be auto-lowercased (not rejected).
        run_update(&db, &notes, &index, &id, None, None, None, None, &["URGENT".to_string()], &[])
        .await
        .unwrap();

        let tags = read_headline_tags(&path);
        assert_eq!(tags, vec!["urgent".to_string()], "uppercase should be auto-lowercased, got: {tags:?}");
    }

    #[tokio::test]
    async fn test_update_add_tag_with_underscore_allowed() {
        let (db, _dir, notes, index) = test_env().await;
        let (path, id) = create_task_for_tags(&db, &notes, &index).await;

        // Underscores are allowed (per user's custom choice).
        run_update(&db, &notes, &index, &id, None, None, None, None, &["work_project".to_string()], &[])
        .await
        .unwrap();

        let tags = read_headline_tags(&path);
        assert_eq!(tags, vec!["work_project".to_string()], "underscores should be allowed, got: {tags:?}");
    }

    #[test]
    fn test_parse_tag_list_basic() {
        assert_eq!(parse_tag_list("urgent,errands"), vec!["urgent", "errands"]);
    }

    #[test]
    fn test_parse_tag_list_trims_whitespace() {
        assert_eq!(
            parse_tag_list(" urgent , errands "),
            vec!["urgent", "errands"],
            "each entry should be trimmed of surrounding whitespace"
        );
    }

    #[test]
    fn test_parse_tag_list_drops_empty_entries() {
        assert_eq!(parse_tag_list("a,,b"), vec!["a", "b"]);
        assert_eq!(parse_tag_list(""), Vec::<String>::new());
        assert_eq!(parse_tag_list(",,,"), Vec::<String>::new());
    }
}
