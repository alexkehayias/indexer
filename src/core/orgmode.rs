use std::collections::HashMap;
use std::ops::Range;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex, OnceLock};

use anyhow::{Context, Result};
use chrono::Local;
use once_cell::sync::Lazy;
use orgize::ast::Drawer;
use orgize::export::{Container, Event, TraversalContext, Traverser};
use orgize::rowan::ast::AstNode;
use orgize::SyntaxElement;
use regex::Regex;
use tokio::fs;
use tokio_rusqlite::Connection;

use crate::org;

/// Suffix Org mode uses for archive files (e.g. `work.org_archive`).
pub const ORG_ARCHIVE_SUFFIX: &str = ".org_archive";

/// Whether a file name/path points at an Org archive file.
pub fn is_archive_file(file_name: &str) -> bool {
    file_name.ends_with(ORG_ARCHIVE_SUFFIX)
}

/// Done-state keywords that count as "closed" for setting CLOSED:.
///
/// SOMEDAY is intentionally excluded — only DONE and CANCELED mark a task as
/// closed (per user requirement).
const CLOSED_STATUSES: &[&str] = &["DONE", "CANCELED"];

/// Write `contents` to `path` atomically: write a temp file in the same
/// directory, then `rename` it over the target. Because rename is atomic on
/// the same filesystem, a concurrent reader — most importantly the git sync's
/// `git add -A`, which runs every few minutes — can never observe a
/// half-written note file. This is what caused the tail corruption seen when a
/// plain truncate+write raced with the git sync.
pub async fn atomic_write(path: &Path, contents: &str) -> Result<()> {
    let dir = path.parent().unwrap_or_else(|| Path::new("."));
    // Notes can live in subdirectories (e.g. projects/), so ensure the
    // parent directory exists before writing the temp file.
    fs::create_dir_all(dir).await?;
    let file_name = path
        .file_name()
        .and_then(|s| s.to_str())
        .unwrap_or("note.org");
    let tmp_path = dir.join(format!(
        ".{file_name}.{}.{}.tmp",
        std::process::id(),
        TMP_COUNTER.fetch_add(1, Ordering::Relaxed)
    ));
    fs::write(&tmp_path, contents).await?;
    fs::rename(&tmp_path, path).await?;
    Ok(())
}

/// Counter to keep atomic-write temp names unique within this process.
static TMP_COUNTER: AtomicU64 = AtomicU64::new(0);

/// Process-global registry of per-path async mutexes, used to serialize
/// read-modify-write mutations to the same note file so concurrent operations
/// (e.g. several refiles from the same file, or a refile racing an update)
/// don't lose each other's changes.
static FILE_LOCKS: OnceLock<Mutex<HashMap<PathBuf, Arc<tokio::sync::Mutex<()>>>>> =
    OnceLock::new();

/// Run `f` while holding exclusive locks for each path in `paths`. Locks are
/// acquired in sorted order so concurrent multi-file operations can't deadlock.
pub async fn with_file_locks<F, Fut, T>(paths: &[PathBuf], f: F) -> Result<T>
where
    F: FnOnce() -> Fut + Send,
    Fut: std::future::Future<Output = Result<T>> + Send,
{
    let map = FILE_LOCKS.get_or_init(|| Mutex::new(HashMap::new()));
    let mut sorted = paths.to_vec();
    sorted.sort();
    sorted.dedup();

    // Collect the Arc handles first, then acquire guards. `locks` is declared
    // before `guards` so the guards (which borrow it) drop first.
    let mut locks: Vec<Arc<tokio::sync::Mutex<()>>> = Vec::with_capacity(sorted.len());
    for path in &sorted {
        let lock = {
            let mut m = map.lock().unwrap();
            m.entry(path.clone())
                .or_insert_with(|| Arc::new(tokio::sync::Mutex::new(())))
                .clone()
        };
        locks.push(lock);
    }
    let mut guards = Vec::with_capacity(locks.len());
    for lock in &locks {
        guards.push(lock.lock().await);
    }
    f().await
}

/// Run `f` while holding an exclusive lock for `path`.
pub async fn with_file_lock<F, Fut, T>(path: &Path, f: F) -> Result<T>
where
    F: FnOnce() -> Fut + Send,
    Fut: std::future::Future<Output = Result<T>> + Send,
{
    with_file_locks(&[path.to_path_buf()], f).await
}

/// Format the current local time as an inactive org timestamp:
/// `[YYYY-MM-DD Ddd HH:MM]`.
fn now_timestamp() -> String {
    Local::now().format("[%Y-%m-%d %a %H:%M]").to_string()
}

/// Format a state-change logbook entry matching emacs' `org-log-into-drawer`:
///
/// ```text
/// - State "DONE"       from "TODO"       [2026-05-23 Sat 10:59]
/// ```
///
/// The seven-space alignment between the quoted state keyword and `from`, and
/// again before the timestamp, matches emacs' default formatting so logs
/// produced by hq round-trip cleanly with logs added by emacs.
fn format_state_change(from: &str, to: &str) -> String {
    format!("- State \"{to}\"       from \"{from}\"       {}", now_timestamp())
}

/// Regex for individual state-change lines inside a LOGBOOK drawer. Captures
/// the full line so it can be preserved verbatim.
///
/// This regex operates on `Drawer::content_raw()` (the text between
/// `:LOGBOOK:` and `:END:`) rather than the raw headline text, so it
/// naturally scopes to the drawer contents. Trailing whitespace is restricted
/// to horizontal (`[ \t]`) so the match does not consume the newline at end
/// of line — that would otherwise cause `writeln!` to emit a spurious blank
/// line between consecutive entries.
static RE_STATE_CHANGE: Lazy<Regex> = Lazy::new(|| {
    Regex::new(r#"(?m)^- State "(\w+)"\s+from "(\w+)"\s+(\[[^\]]+\])[ \t]*$"#).unwrap()
});

/// Extract the CLOSED: timestamp from a parsed orgize `Headline`, if set.
///
/// Returns the bracketed inactive-timestamp string (e.g.
/// `"[2026-05-23 Sat 10:59]"`) so it can be preserved verbatim across
/// headline rebuilds.
fn extract_closed(headline: &orgize::ast::Headline) -> Option<String> {
    // Headline::closed() returns the parsed Timestamp node; `.syntax()` gives
    // us the underlying SyntaxNode whose Display impl renders it back to the
    // original text (e.g. `[2026-05-23 Sat 10:59]`).
    headline.closed().map(|ts| ts.syntax().to_string())
}

/// Extract state-change logbook entries from a parsed orgize `Headline`.
///
/// Walks the headline's `Section` for `Drawer` nodes named `LOGBOOK`,
/// then parses each drawer's content text for `- State "X"       from "Y"
/// [TS]` lines using `RE_STATE_CHANGE`. Entries are returned in document
/// order. Drawers that are not named LOGBOOK (e.g., `:PROPERTIES:`) are
/// ignored. State-change text appearing outside a drawer is also ignored.
fn extract_logbook(headline: &orgize::ast::Headline) -> Vec<String> {
    let mut entries = Vec::new();
    // Headline's body content (Section) holds any LOGBOOK drawers. Walk it
    // the same way Headline::clocks() does: iterate Section's children,
    // filter by Drawer, then check the drawer name.
    if let Some(section) = headline.section() {
        for child in section.syntax().children() {
            if let Some(drawer) = Drawer::cast(child) {
                if drawer.name().eq_ignore_ascii_case("LOGBOOK") {
                    let content = drawer.content_raw();
                    for caps in RE_STATE_CHANGE.captures_iter(&content) {
                        entries.push(caps.get(0).unwrap().as_str().to_string());
                    }
                }
            }
        }
    }
    entries
}

pub struct TaskLocation {
    pub path: PathBuf,
    pub range: Range<usize>,
    pub content: String,
    pub current_title: String,
    pub current_body: String,
    pub current_status: String,
    pub current_level: usize,
    /// Existing `CLOSED:` timestamp bracket string (e.g.
    /// `"[2026-05-23 Sat 10:59]"`) parsed from the headline, if any.
    pub current_closed: Option<String>,
    /// Existing state-change lines from the headline's `:LOGBOOK:` drawer,
    /// preserved verbatim. Empty if no LOGBOOK drawer is present.
    pub current_logbook: Vec<String>,
    /// Existing headline-level tags parsed from the headline (e.g. `["errands", "urgent"]`
    /// for a headline written as `* TODO Buy groceries :errands:urgent:`).
    pub current_tags: Vec<String>,
}

pub fn build_headline(id: &str, title: &str, body: &str, status: &str, level: usize) -> String {
    let mut h = org::Headline::builder()
        .level(level)
        .status(status)
        .title(title)
        .property("ID", id);
    if !body.is_empty() {
        h = h.body(body);
    }
    h.build().to_string()
}

/// Build an updated headline string from current and new field values,
/// including the `CLOSED:` planning line and `:LOGBOOK:` drawer entries.
///
/// This is used by `update_task` / `update_task_in_file` to rebuild a
/// headline in place. New headlines (created via `run_create`) use
/// `build_headline` instead, since they have no prior state to preserve.
fn build_updated_headline(
    id: &str,
    title: &str,
    body: &str,
    status: &str,
    level: usize,
    closed: Option<&str>,
    logbook: &[String],
    tags: &[String],
) -> String {
    let mut h = org::Headline::builder()
        .level(level)
        .status(status)
        .title(title)
        .property("ID", id);
    if !body.is_empty() {
        h = h.body(body);
    }
    if let Some(ts) = closed {
        h = h.closed(ts);
    }
    for entry in logbook {
        h = h.logbook_entry(entry);
    }
    if !tags.is_empty() {
        h = h.tags(tags.to_vec());
    }
    h.build().to_string()
}

/// Validate and normalize a tag being added to an org-mode headline.
///
/// Tags must be lowercase, with no spaces or special characters. Allowed
/// characters: `a-z`, `0-9`, hyphens (`-`), and underscores (`_`). The input
/// is lowercased before validation so `Urgent` becomes `urgent`. Tags with
/// spaces or other special characters (e.g. `!`, `@`, `.`) are rejected with
/// a clear error message.
///
/// Existing tags read from files are NOT passed through this function — only
/// tags being newly added via `--add-tag` (or programmatic equivalents) are
/// validated. This lets users clean up messy existing data without being blocked.
pub(crate) fn validate_tag(tag: &str) -> Result<String> {
    let normalized = tag.trim().to_lowercase();
    if normalized.is_empty() {
        anyhow::bail!("tag cannot be empty");
    }
    for c in normalized.chars() {
        if c.is_ascii_whitespace() {
            anyhow::bail!(
                "tag '{tag}' contains spaces; tags must be lowercase with no spaces (allowed: a-z, 0-9, hyphens, underscores)"
            );
        }
        if !c.is_ascii_lowercase() && !c.is_ascii_digit() && c != '-' && c != '_' {
            anyhow::bail!(
                "tag '{tag}' contains special character '{}'; tags must be lowercase with no spaces or special characters (allowed: a-z, 0-9, hyphens, underscores)",
                c
            );
        }
    }
    Ok(normalized)
}

/// Compute the new tag list for a headline update.
///
/// Starts from `current_tags` (lowercased for output consistency), appends any
/// `add_tags` that aren't already present (deduped, first-seen order preserved),
/// then removes any in `remove_tags`. Removing a tag not currently set is a
/// silent no-op (idempotent).
///
/// `add_tags` are validated via `validate_tag` — tags with spaces or special
/// characters cause an error. `current_tags` and `remove_tags` are lowercased
/// but NOT validated, so existing messy data can be preserved/removed without
/// blocking updates.
pub(crate) fn compute_new_tags(
    current: &[String],
    add: &[String],
    remove: &[String],
) -> Result<Vec<String>> {
    // Lowercase existing tags (preserve spaces/special chars if present —
    // we don't validate existing data, only normalize case for output consistency).
    let mut result: Vec<String> = current.iter().map(|t| t.to_lowercase()).collect();

    // Validate and lowercase add tags (strict: no spaces or special chars).
    let validated_add: Vec<String> = add
        .iter()
        .map(|t| validate_tag(t))
        .collect::<Result<Vec<_>>>()?;

    // Add new tags (deduped, order-preserving)
    for tag in &validated_add {
        if !result.iter().any(|t| t == tag) {
            result.push(tag.clone());
        }
    }

    // Lowercase remove tags for case-insensitive matching against current tags
    let remove_lower: std::collections::HashSet<String> =
        remove.iter().map(|t| t.to_lowercase()).collect();
    result.retain(|t| !remove_lower.contains(t));

    Ok(result)
}

/// Given a task's current state (`location`) and the new status being applied,
/// compute the resulting `CLOSED:` timestamp and logbook entries.
///
/// Rules:
/// - If status unchanged: preserve existing CLOSED/logbook as-is (no new entry).
/// - If status changed: append a `- State "NEW"       from "OLD"       [TS]`
///   entry to the logbook.
/// - If transitioning into DONE/CANCELED (from a non-closed state): set
///   CLOSED to the current time.
/// - If transitioning out of DONE/CANCELED (reopening): clear CLOSED.
/// - SOMEDAY is not treated as closed — transitions to SOMEDAY log a state
///   change but do not set CLOSED.
fn compute_state_transition(
    location: &TaskLocation,
    new_status: &str,
) -> (Option<String>, Vec<String>) {
    let old_status = &location.current_status;
    let status_changed = old_status != new_status;

    let new_is_closed = CLOSED_STATUSES.contains(&new_status);
    let old_was_closed = CLOSED_STATUSES.contains(&old_status.as_str());

    let mut new_logbook = location.current_logbook.clone();
    let mut new_closed = location.current_closed.clone();

    if status_changed {
        new_logbook.push(format_state_change(old_status, new_status));

        if new_is_closed && !old_was_closed {
            // Closing: set CLOSED to now.
            new_closed = Some(now_timestamp());
        } else if !new_is_closed && old_was_closed {
            // Reopening: clear CLOSED.
            new_closed = None;
        }
    }

    (new_closed, new_logbook)
}

/// Build a regex pattern that matches `:ID:` followed by the given value,
/// regardless of the amount of whitespace between the key and value.
fn id_pattern(id: &str) -> Regex {
    Regex::new(&format!(":ID:\\s+{}", regex::escape(id))).unwrap()
}

/// Find a task by UUID within a specific file.
pub async fn find_task_in_file(path: &PathBuf, id: &str) -> Result<TaskLocation> {
    let pattern = id_pattern(id);
    let content = fs::read_to_string(path)
        .await
        .with_context(|| format!("Cannot read file: {}", path.display()))?;

    if !pattern.is_match(&content) {
        anyhow::bail!("Task with ID {id} not found in {}", path.display());
    }

    let config = org::todo_keywords_config();
    let org = config.parse(&content);
    for headline in org.document().headlines() {
        if let Some(props) = headline.properties() {
            if props.get("ID").is_some_and(|v| v == id) {
                let range = headline.syntax().text_range();
                let usize_range = u32::from(range.start()) as usize..u32::from(range.end()) as usize;
                let current_status = headline
                    .todo_keyword()
                    .map(|k| k.to_string())
                    .unwrap_or_else(|| "TODO".to_string());
                let current_title = headline.title_raw().trim().to_string();
                let current_level = headline.level();
                let current_body = body_from_headline(&headline);
                let current_closed = extract_closed(&headline);
                let current_logbook = extract_logbook(&headline);
                let current_tags = headline
                    .tags()
                    .map(|t| t.to_string())
                    .collect::<Vec<String>>();

                return Ok(TaskLocation {
                    path: path.clone(),
                    range: usize_range,
                    content,
                    current_title,
                    current_body,
                    current_status,
                    current_level,
                    current_closed,
                    current_logbook,
                    current_tags,
                });
            }
        }
    }

    anyhow::bail!(
        "Found ID {id} in {} but could not locate its headline",
        path.display()
    );
}

/// Find a task by UUID, using the index to locate its file.
///
/// Looks up the task's `file_name` in `note_meta`, then reads just that one
/// file. Tasks are only findable once indexed (created via the CLI or picked
/// up by the periodic index job) — this matches `run_list`, which also queries
/// `note_meta`. Callers that move a task between files (e.g. refile) must
/// re-index so the entry stays in sync.
pub async fn find_task(db: &Connection, notes_path: &str, id: &str) -> Result<TaskLocation> {
    let id_owned = id.to_string();
    let db_file: Option<String> = db
        .call(move |conn| {
            let mut stmt = conn.prepare(
                "SELECT file_name FROM note_meta WHERE id = ?1 AND type = 'task'",
            )?;
            let mut rows = stmt.query_map([id_owned.as_str()], |row| row.get::<_, String>(0))?;
            Ok(rows.next().transpose()?)
        })
        .await?;

    let Some(file_name) = db_file else {
        anyhow::bail!("Task with ID {id} not found in {notes_path}");
    };
    let path = std::path::Path::new(notes_path).join(&file_name);
    find_task_in_file(&path, id).await
}

/// Traverses a headline's syntax subtree and extracts body text,
/// skipping the headline title and property drawer.
#[derive(Default)]
struct BodyExtractor {
    output: String,
    in_headline_title: bool,
}

impl BodyExtractor {
    fn finish(self) -> String {
        self.output.trim().to_string()
    }
}

impl Traverser for BodyExtractor {
    fn event(&mut self, event: Event, ctx: &mut TraversalContext) {
        match event {
            Event::Enter(Container::Headline(_)) => {
                self.in_headline_title = true;
            }
            Event::Leave(Container::Headline(_)) => {
                self.in_headline_title = false;
            }
            // Entering a Section means we've passed the headline title
            // and are now in the body area.
            Event::Enter(Container::Section(_)) => {
                self.in_headline_title = false;
            }
            // Skip property drawers entirely
            Event::Enter(Container::PropertyDrawer(_)) => {
                ctx.skip();
            }
            Event::Leave(Container::PropertyDrawer(_)) => {}
            // Skip generic drawers (e.g. :LOGBOOK:) so their content doesn't
            // leak into the extracted body. State-change entries are read
            // separately via `extract_logbook` on the raw headline text.
            Event::Enter(Container::Drawer(_)) => {
                ctx.skip();
            }
            Event::Leave(Container::Drawer(_)) => {}
            // Add newline between paragraphs
            Event::Leave(Container::Paragraph(_)) => {
                if !self.in_headline_title {
                    self.output.push('\n');
                }
            }
            Event::Text(text) => {
                if !self.in_headline_title {
                    self.output.push_str(&text);
                }
            }
            _ => {}
        }
    }
}

/// Extract body text from a headline's syntax node using the orgize Traverser.
pub fn body_from_headline(headline: &orgize::ast::Headline) -> String {
    let mut extractor = BodyExtractor::default();
    let mut ctx = TraversalContext::default();
    extractor.element(SyntaxElement::Node(headline.syntax().clone()), &mut ctx);
    extractor.finish()
}

/// Update a task's fields in a specific file.
///
/// Finds the task by ID in the given file, rebuilds the headline with the
/// provided field updates, and writes the modified file back to disk.
///
/// When `status` changes, a state-change entry is appended to the headline's
/// `:LOGBOOK:` drawer. When transitioning into DONE or CANCELED, a `CLOSED:`
/// planning timestamp is set; when reopening (DONE/CANCELED → non-closed),
/// `CLOSED:` is cleared. Existing logbook entries are preserved verbatim.
///
/// Tag updates (`add_tags` / `remove_tags`) are applied to the headline's
/// existing tag list (deduped, order-preserving) and round-tripped through the
/// rebuild. Removing a tag not currently set is a silent no-op.
pub async fn update_task_in_file(
    file_path: &PathBuf,
    id: &str,
    title: Option<&str>,
    body: Option<&str>,
    status: Option<&str>,
    add_tags: &[String],
    remove_tags: &[String],
) -> Result<()> {
    with_file_lock(file_path, || async {
        let location = find_task_in_file(file_path, id).await?;
        apply_update(&location, id, title, body, status, add_tags, remove_tags).await
    })
    .await
}

/// Rebuild the headline for a located task and write the file back atomically.
/// Callers must hold the file lock for `location.path`.
async fn apply_update(
    location: &TaskLocation,
    id: &str,
    title: Option<&str>,
    body: Option<&str>,
    status: Option<&str>,
    add_tags: &[String],
    remove_tags: &[String],
) -> Result<()> {
    let new_title = title.unwrap_or(&location.current_title);
    let new_body = body.unwrap_or(&location.current_body);
    let status = status.map(|s| s.to_uppercase());
    let new_status = status.as_deref().unwrap_or(&location.current_status);

    let (new_closed, new_logbook) =
        compute_state_transition(location, new_status);
    let new_tags = compute_new_tags(&location.current_tags, add_tags, remove_tags)?;

    let new_headline = build_updated_headline(
        id,
        new_title,
        new_body,
        new_status,
        location.current_level,
        new_closed.as_deref(),
        &new_logbook,
        &new_tags,
    );
    let new_content = format!(
        "{before}{new_headline}{after}",
        before = &location.content[..location.range.start],
        after = &location.content[location.range.end..]
    );
    atomic_write(&location.path, &new_content)
        .await
        .context("Failed to write updated task file")
}

/// Update a task by UUID, optionally scoped to a specific file.
///
/// If `file_name` is provided, looks only in that file. If not found there,
/// returns an error (no filesystem fallback). If `file_name` is `None`,
/// searches all org files in the notes directory.
///
/// When `status` changes, a state-change entry is appended to the headline's
/// `:LOGBOOK:` drawer. When transitioning into DONE or CANCELED, a `CLOSED:`
/// planning timestamp is set; when reopening (DONE/CANCELED → non-closed),
/// `CLOSED:` is cleared. Existing logbook entries are preserved verbatim.
///
/// Tag updates (`add_tags` / `remove_tags`) apply to the headline's existing tag
/// list (deduped, order-preserving). Removing a tag not currently set is silent.
pub async fn update_task(
    db: &Connection,
    notes_path: &str,
    id: &str,
    file_name: Option<&str>,
    title: Option<&str>,
    body: Option<&str>,
    status: Option<&str>,
    add_tags: &[String],
    remove_tags: &[String],
) -> Result<()> {
    let location = if let Some(fname) = file_name {
        let path = std::path::Path::new(notes_path).join(fname);
        find_task_in_file(&path, id).await.with_context(|| {
            format!("Task {id} not found in scoped file {fname}")
        })?
    } else {
        find_task(db, notes_path, id).await?
    };
    let path = location.path.clone();
    with_file_lock(&path, || async {
        // Re-read under the lock so a concurrent writer can't be overwritten.
        let location = find_task_in_file(&path, id).await?;
        apply_update(&location, id, title, body, status, add_tags, remove_tags).await
    })
    .await
}

/// Build a full org-mode document string for a standalone task.
pub fn build_document(id: &str, title: &str, body: &str, status: &str) -> String {
    let headline = org::Headline::builder()
        .level(1)
        .status(status)
        .title(title)
        .property("ID", id);
    let headline = if !body.is_empty() {
        headline.body(body)
    } else {
        headline
    };
    org::Document::builder()
        .property("ID", id)
        .title(title)
        .filetags("task")
        .headline(headline.build())
        .build()
        .to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Parse an org-mode document string and return the first headline.
    fn first_headline(content: &str) -> orgize::ast::Headline {
        let config = org::todo_keywords_config();
        let org = config.parse(content);
        org.document()
            .headlines()
            .next()
            .expect("test content must contain a headline")
    }

    #[test]
    fn test_extract_closed_present() {
        let h = first_headline(
            "* DONE Task\n\
             CLOSED: [2026-05-23 Sat 10:59]\n\
             :PROPERTIES:\n\
             :ID:       abc\n\
             :END:",
        );
        assert_eq!(extract_closed(&h), Some("[2026-05-23 Sat 10:59]".to_string()));
    }

    #[test]
    fn test_extract_closed_absent() {
        let h = first_headline("* TODO Task\n:PROPERTIES:\n:END:");
        assert_eq!(extract_closed(&h), None);
    }

    #[test]
    fn test_extract_closed_active_timestamp_still_returned() {
        // Even active timestamps are returned verbatim via syntax().to_string().
        // Org-mode typically uses inactive for CLOSED:, but the extractor
        // preserves whatever is there.
        let h = first_headline("* DONE Task\nCLOSED: <2026-05-23 Sat 10:59>");
        assert_eq!(
            extract_closed(&h),
            Some("<2026-05-23 Sat 10:59>".to_string())
        );
    }

    #[test]
    fn test_extract_logbook_empty_when_no_drawer() {
        let h = first_headline("* TODO Task\n:PROPERTIES:\n:END:");
        assert!(extract_logbook(&h).is_empty());
    }

    #[test]
    fn test_extract_logbook_single_entry() {
        let h = first_headline(
            "* DONE Task\n\
             CLOSED: [2026-05-23 Sat 10:59]\n\
             :PROPERTIES:\n\
             :END:\n\
             :LOGBOOK:\n\
             - State \"DONE\"       from \"TODO\"       [2026-05-23 Sat 10:59]\n\
             :END:",
        );
        let entries = extract_logbook(&h);
        assert_eq!(entries.len(), 1);
        assert!(
            entries[0].contains("- State \"DONE\"       from \"TODO\""),
            "entry should preserve text verbatim, got: {}",
            entries[0]
        );
    }

    #[test]
    fn test_extract_logbook_multiple_entries() {
        // Reopened task: two state-change entries in the LOGBOOK.
        let h = first_headline(
            "* TODO Reopened\n\
             :PROPERTIES:\n\
             :END:\n\
             :LOGBOOK:\n\
             - State \"DONE\"       from \"TODO\"       [2026-05-23 Sat 10:59]\n\
             - State \"TODO\"       from \"DONE\"       [2026-05-24 Sun 09:00]\n\
             :END:",
        );
        let entries = extract_logbook(&h);
        assert_eq!(entries.len(), 2);
        assert!(entries[0].contains("\"DONE\""));
        assert!(entries[1].contains("\"TODO\""));
    }

    #[test]
    fn test_extract_logbook_ignores_non_logbook_drawers() {
        // A non-LOGBOOK drawer containing state-change-like text should be
        // ignored — only :LOGBOOK: drawers contribute entries.
        let h = first_headline(
            "* TODO Task\n\
             :PROPERTIES:\n\
             :END:\n\
             :NOTES:\n\
             - State \"DONE\"       from \"TODO\"       [2026-05-23 Sat 10:59]\n\
             :END:",
        );
        assert!(extract_logbook(&h).is_empty());
    }

    #[test]
    fn test_extract_logbook_multiple_drawers() {
        // Unusual but valid: two LOGBOOK drawers in one headline.
        // All entries should be returned in document order.
        let h = first_headline(
            "* TODO Task\n\
             :PROPERTIES:\n\
             :END:\n\
             :LOGBOOK:\n\
             - State \"DONE\"       from \"TODO\"       [2026-05-23 Sat 10:59]\n\
             :END:\n\
             :LOGBOOK:\n\
             - State \"TODO\"       from \"DONE\"       [2026-05-24 Sun 09:00]\n\
             :END:",
        );
        let entries = extract_logbook(&h);
        assert_eq!(entries.len(), 2);
    }

    #[test]
    fn test_extract_logbook_preserves_trailing_whitespace_alignment() {
        // The regex should preserve the exact spacing (7 spaces) between
        // state keywords and the timestamp, so round-tripping is verbatim.
        let original = "- State \"DONE\"       from \"TODO\"       [2026-05-23 Sat 10:59]";
        let h = first_headline(&format!(
            "* DONE Task\n:PROPERTIES:\n:END:\n:LOGBOOK:\n{original}\n:END:"
        ));
        let entries = extract_logbook(&h);
        assert_eq!(entries.len(), 1);
        assert_eq!(
            entries[0], original,
            "entry should be preserved verbatim including alignment"
        );
    }

    #[test]
    fn test_extract_closed_and_logbook_together() {
        // Realistic DONE task with both CLOSED: and a LOGBOOK entry.
        let h = first_headline(
            "* DONE Fix emacs init\n\
             CLOSED: [2026-05-23 Sat 10:59]\n\
             :PROPERTIES:\n\
             :ID:       10038616-55C7-4710-BF4D-53E68D5B14F0\n\
             :END:\n\
             :LOGBOOK:\n\
             - State \"DONE\"       from \"TODO\"       [2026-05-23 Sat 10:59]\n\
             :END:",
        );
        assert_eq!(
            extract_closed(&h),
            Some("[2026-05-23 Sat 10:59]".to_string())
        );
        let entries = extract_logbook(&h);
        assert_eq!(entries.len(), 1);
        assert!(entries[0].contains("\"DONE\""));
    }

    // -----------------------------------------------------------------------
    // Headline-level tags: extraction and compute_new_tags
    // -----------------------------------------------------------------------

    #[test]
    fn test_extract_tags_present() {
        let h = first_headline("* TODO Buy groceries :errands:urgent:\n:PROPERTIES:\n:END:");
        let tags: Vec<String> = h.tags().map(|t| t.to_string()).collect();
        assert_eq!(tags, vec!["errands".to_string(), "urgent".to_string()]);
    }

    #[test]
    fn test_extract_tags_empty() {
        let h = first_headline("* TODO Buy groceries\n:PROPERTIES:\n:END:");
        let tags: Vec<String> = h.tags().map(|t| t.to_string()).collect();
        assert!(tags.is_empty(), "headline without tags should yield empty Vec, got: {tags:?}");
    }

    #[test]
    fn test_compute_new_tags_add_appends() {
        let current = vec!["existing".to_string()];
        let add = vec!["urgent".to_string()];
        let result = compute_new_tags(&current, &add, &[]).unwrap();
        assert_eq!(result, vec!["existing", "urgent"]);
    }

    #[test]
    fn test_compute_new_tags_add_dedupes_existing() {
        let current = vec!["urgent".to_string()];
        let add = vec!["urgent".to_string()];
        let result = compute_new_tags(&current, &add, &[]).unwrap();
        assert_eq!(result, vec!["urgent"], "duplicate add should not appear twice");
    }

    #[test]
    fn test_compute_new_tags_add_dedupes_within_add() {
        let current: Vec<String> = vec![];
        let add = vec!["urgent".to_string(), "urgent".to_string()];
        let result = compute_new_tags(&current, &add, &[]).unwrap();
        assert_eq!(result, vec!["urgent"], "duplicates within add should be deduped");
    }

    #[test]
    fn test_compute_new_tags_remove_existing() {
        let current = vec!["a".to_string(), "b".to_string(), "c".to_string()];
        let result = compute_new_tags(&current, &[], &["b".to_string()]).unwrap();
        assert_eq!(result, vec!["a", "c"]);
    }

    #[test]
    fn test_compute_new_tags_remove_nonexistent_silent() {
        let current = vec!["a".to_string()];
        let result = compute_new_tags(&current, &[], &["nonexistent".to_string()]).unwrap();
        assert_eq!(result, vec!["a"], "removing a tag not present should be a silent no-op");
    }

    #[test]
    fn test_compute_new_tags_combined_add_and_remove() {
        let current = vec!["a".to_string(), "b".to_string()];
        let add = vec!["c".to_string()];
        let remove = vec!["a".to_string()];
        let result = compute_new_tags(&current, &add, &remove).unwrap();
        assert_eq!(result, vec!["b", "c"]);
    }

    #[test]
    fn test_compute_new_tags_remove_all() {
        let current = vec!["a".to_string(), "b".to_string()];
        let remove = vec!["a".to_string(), "b".to_string()];
        let result = compute_new_tags(&current, &[], &remove).unwrap();
        assert!(result.is_empty(), "removing all tags should yield empty Vec, got: {result:?}");
    }

    // -----------------------------------------------------------------------
    // Tag validation (validate_tag + compute_new_tags error cases)
    // -----------------------------------------------------------------------

    #[test]
    fn test_validate_tag_lowercases() {
        assert_eq!(validate_tag("Urgent").unwrap(), "urgent");
        assert_eq!(validate_tag("URGENT").unwrap(), "urgent");
        assert_eq!(validate_tag("UrGeNt").unwrap(), "urgent");
    }

    #[test]
    fn test_validate_tag_allows_hyphens_and_underscores() {
        assert_eq!(validate_tag("low-priority").unwrap(), "low-priority");
        assert_eq!(validate_tag("work_project").unwrap(), "work_project");
        assert_eq!(validate_tag("tag-1_v2").unwrap(), "tag-1_v2");
    }

    #[test]
    fn test_validate_tag_rejects_spaces() {
        let err = validate_tag("ur gent").unwrap_err();
        assert!(
            err.to_string().contains("spaces"),
            "error should mention spaces, got: {err}"
        );
    }

    #[test]
    fn test_validate_tag_rejects_special_chars() {
        for bad in &["urgent!", "tag@host", "a.b.c", "tag#1", "u/r"] {
            let err = validate_tag(bad).unwrap_err();
            assert!(
                err.to_string().contains("special character"),
                "error for '{bad}' should mention special character, got: {err}"
            );
        }
    }

    #[test]
    fn test_validate_tag_rejects_empty() {
        let err = validate_tag("").unwrap_err();
        assert!(err.to_string().contains("empty"), "error should mention empty, got: {err}");
    }

    #[test]
    fn test_validate_tag_trims_whitespace() {
        // Leading/trailing whitespace is trimmed; the tag itself is valid.
        assert_eq!(validate_tag("  urgent  ").unwrap(), "urgent");
    }

    #[test]
    fn test_compute_new_tags_add_invalid_tag_errors() {
        // Add tags with spaces/special chars should error.
        let err = compute_new_tags(&[], &["ur gent".to_string()], &[]).unwrap_err();
        assert!(err.to_string().contains("spaces"), "should reject spaces, got: {err}");

        let err = compute_new_tags(&[], &["urgent!".to_string()], &[]).unwrap_err();
        assert!(err.to_string().contains("special character"), "should reject special chars, got: {err}");
    }

    #[test]
    fn test_compute_new_tags_preserves_invalid_current_tags() {
        // Existing tags with spaces (messy data) should be preserved, not error.
        // Only normalize case — don't block the update.
        let current = vec!["UR GENT".to_string()];
        let result = compute_new_tags(&current, &[], &[]).unwrap();
        assert_eq!(result, vec!["ur gent".to_string()],
            "existing tags should be lowercased but preserved (spaces kept), got: {result:?}");
    }

    #[test]
    fn test_compute_new_tags_removes_invalid_current_tag_case_insensitive() {
        // If existing tag has spaces, user can still remove it by specifying
        // the same (lowercased) value.
        let current = vec!["ur gent".to_string()];
        let result = compute_new_tags(&current, &[], &["UR GENT".to_string()]).unwrap();
        assert!(result.is_empty(), "should remove the tag case-insensitively, got: {result:?}");
    }

    /// Build a Headline with tags via the builder, render it to a string,
    /// parse the string back, and confirm `tags()` returns the same set in order.
    #[test]
    fn test_headline_tags_round_trip() {
        let h = org::Headline::builder()
            .level(1)
            .status("TODO")
            .title("Buy groceries")
            .property("ID", "task-1")
            .tags(vec!["errands".to_string(), "urgent".to_string()])
            .build();
        let rendered = h.to_string();

        // Rendered form should contain the trailing tag suffix on the title line.
        assert!(
            rendered.contains("Buy groceries :errands:urgent:"),
            "rendered headline should include tag suffix, got:\n{rendered}"
        );

        // Parse back via orgize and verify tags round-trip.
        let config = org::todo_keywords_config();
        let org = config.parse(&rendered);
        let parsed = org.document().headlines().next().expect("should parse one headline");
        let tags: Vec<String> = parsed.tags().map(|t| t.to_string()).collect();
        assert_eq!(tags, vec!["errands".to_string(), "urgent".to_string()]);
    }

    #[test]
    fn test_headline_no_tags_renders_without_suffix() {
        let h = org::Headline::builder()
            .level(1)
            .status("TODO")
            .title("Plain task")
            .property("ID", "task-2")
            .build();
        let rendered = h.to_string();
        assert!(
            !rendered.contains(" :"),
            "headline without tags should not emit a trailing tag suffix, got:\n{rendered}"
        );
    }
}

