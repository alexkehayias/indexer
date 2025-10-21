use rusqlite::{ffi::sqlite3_auto_extension, Connection, Result};
use sqlite_vec::sqlite3_vec_init;

pub fn migrate_db(db: &Connection) -> Result<()> {
    // Create a metadata table that has a foreign key to the
    // embeddings virtual table. This will be used to coordinate
    // upserts and hydrating the notes
    db.execute(
        r"CREATE TABLE IF NOT EXISTS note_meta (
    id TEXT PRIMARY KEY,
    file_name TEXT,
    title TEXT,
    tags TEXT NULLABLE,
    body TEXT,
    type TEXT,
    status TEXT
);",
        [],
    )?;

    // Create vector virtual table for similarity search
    db.execute(
        "CREATE VIRTUAL TABLE IF NOT EXISTS vec_items USING vec0(
note_meta_id TEXT PRIMARY KEY,
embedding float[384]
);",
        [],
    )?;

    // 2024-12-29 Add colums for type and status
    db.execute(
        r"BEGIN;

CREATE TABLE IF NOT EXISTS note_meta_new (
    id TEXT PRIMARY KEY,
    file_name TEXT,
    title TEXT,
    tags TEXT NULLABLE,
    body TEXT,
    type TEXT DEFAULT 'note',
    status TEXT
);

INSERT INTO note_meta_new (id, file_name, title, tags, body)
SELECT id, file_name, title, tags, body FROM note_meta;

DROP TABLE note_meta;

ALTER TABLE note_meta_new RENAME TO note_meta;

COMMIT;",
        [],
    )?;

    Ok(())
}

pub fn vector_db(path_to_db_file: &str) -> Result<Connection> {
    unsafe {
        sqlite3_auto_extension(Some(std::mem::transmute(sqlite3_vec_init as *const ())));
    }
    let db = Connection::open(format!("{}/vector.db", path_to_db_file))?;

    Ok(db)
}
