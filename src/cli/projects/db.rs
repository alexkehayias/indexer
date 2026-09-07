use std::path::PathBuf;

use anyhow::Result;
use tokio_rusqlite::Connection;

pub struct ProjectRow {
    pub id: String,
    pub title: String,
    pub file_name: String,
    pub total_tasks: usize,
    pub done_tasks: usize,
    pub todo_tasks: usize,
    pub is_done: bool,
}

fn slugify(s: &str) -> String {
    s.to_lowercase()
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
        .to_string()
}

/// Special inbox/project files addressed by their bare name rather than by
/// the dated `--project-{slug}.org` scheme. Their filenames carry no date
/// (e.g. `capture.org`, `work.org`, `personal.org`).
pub const SPECIAL_FILES: &[&str] = &["capture", "refile", "personal", "work"];

/// Whether `name` refers to a special inbox/project file.
pub fn is_special_file(name: &str) -> bool {
    SPECIAL_FILES.contains(&name)
}

/// Find a project file by ID, title, or name slug using the database.
///
/// Queries `note_meta` for project-type entries and matches the given reference
/// against the project's UUID, title, or filename (via slug). Returns the full
/// path to the project file, or `None` if no match is found.
///
/// Special files (capture, refile, personal, work) are resolved directly by
/// their bare filename (`{name}.org`) when present on disk, since they carry
/// no date in their filename and are not registered under the
/// `--project-{slug}.org` scheme.
pub async fn find_project_file(db: &Connection, notes_path: &str, project_ref: &str) -> Result<Option<PathBuf>> {
    if is_special_file(project_ref) {
        let path = PathBuf::from(format!("{notes_path}/projects/{project_ref}.org"));
        if path.exists() {
            return Ok(Some(path));
        }
        // Not on disk yet — caller should create it as a special file.
        return Ok(None);
    }

    let projects = db
        .call(|conn| {
            let mut stmt = conn.prepare(
                "SELECT file_name, id, title FROM note_meta WHERE type = 'note' AND tags LIKE '%project%' AND file_name NOT LIKE '%_archive'",
            )?;

            let rows = stmt
                .query_map([], |row| {
                    let file_name: String = row.get(0)?;
                    let id: String = row.get(1)?;
                    let title: String = row.get(2)?;
                    Ok((file_name, id, title))
                })?
                .filter_map(|r| r.ok())
                .collect::<Vec<_>>();

            Ok(rows)
        })
        .await?;

    let slug = slugify(project_ref);

    for (file_name, id, title) in &projects {
        // Match by UUID
        if id == project_ref {
            return Ok(Some(PathBuf::from(format!("{notes_path}/{file_name}"))));
        }
        // Match by exact title
        if title == project_ref {
            return Ok(Some(PathBuf::from(format!("{notes_path}/{file_name}"))));
        }
        // Match by exact filename
        if file_name == project_ref {
            return Ok(Some(PathBuf::from(format!("{notes_path}/{file_name}"))));
        }
        // Match by slug against the --project-{slug}.org suffix
        if file_name.ends_with(&format!("--project-{slug}.org")) {
            return Ok(Some(PathBuf::from(format!("{notes_path}/{file_name}"))));
        }
        // Also try underscore variant for backwards compat
        let underscore_slug = slug.replace('-', "_");
        if underscore_slug != slug && file_name.ends_with(&format!("--project-{underscore_slug}.org")) {
            return Ok(Some(PathBuf::from(format!("{notes_path}/{file_name}"))));
        }
    }

    Ok(None)
}

pub async fn list_projects(db: &Connection) -> Result<Vec<ProjectRow>> {
    let projects = db
        .call(|conn| {
            let mut stmt = conn.prepare(
                "SELECT
                   n.id,
                   n.title,
                   n.file_name,
                   COUNT(t.id) as total,
                   COALESCE(SUM(CASE WHEN t.status IN ('done', 'canceled', 'someday') THEN 1 ELSE 0 END), 0) as done,
                   COALESCE(SUM(CASE WHEN t.status NOT IN ('done', 'canceled', 'someday') THEN 1 ELSE 0 END), 0) as todo,
                   CASE WHEN n.tags LIKE '%project_done%' THEN 1 ELSE 0 END as is_done
                 FROM note_meta n
                 LEFT JOIN note_meta t ON t.file_name = n.file_name AND t.type = 'task'
                 WHERE n.type = 'note' AND n.tags LIKE '%project%' AND n.file_name NOT LIKE '%_archive'
                 GROUP BY n.id
                 ORDER BY n.title",
            )?;

            let rows = stmt
                .query_map([], |row| {
                    let id: String = row.get(0)?;
                    let title: String = row.get(1)?;
                    let file_name: String = row.get(2)?;
                    let total_tasks: usize = row.get(3)?;
                    let done_tasks: usize = row.get(4)?;
                    let todo_tasks: usize = row.get(5)?;
                    let is_done_int: i32 = row.get(6)?;
                    Ok(ProjectRow {
                        id,
                        title,
                        file_name,
                        total_tasks,
                        done_tasks,
                        todo_tasks,
                        is_done: is_done_int != 0,
                    })
                })?
                .filter_map(|r| r.ok())
                .collect::<Vec<_>>();

            Ok(rows)
        })
        .await?;

    Ok(projects)
}
