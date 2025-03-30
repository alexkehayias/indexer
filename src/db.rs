use rusqlite::{ffi::sqlite3_auto_extension, Connection, Result};
use sqlite_vec::sqlite3_vec_init;

/// Initialize the db by creating all tables. This function should
/// always succeed and is safe to run multiple times.
pub fn initialize_db(db: &Connection) -> Result<()> {
    // Create a metadata table that has a foreign key to the
    // embeddings virtual table. This will be used to coordinate
    // upserts and hydrating the notes
    let create_note_meta = db.execute(
        r"CREATE TABLE IF NOT EXISTS note_meta (
    id TEXT PRIMARY KEY,
    file_name TEXT,
    title TEXT,
    category TEXT,
    tags TEXT NULLABLE,
    body TEXT,
    type TEXT,
    status TEXT
);",
        [],
    );
    match create_note_meta {
        Ok(_) => (),
        Err(e) => println!("Create note meta table failed: {}", e)
    }

    // Create vector virtual table for similarity search
    let create_note_vec_table = db.execute(
        "CREATE VIRTUAL TABLE IF NOT EXISTS vec_items USING vec0(
note_meta_id TEXT PRIMARY KEY,
embedding float[384]
);",
        [],
    );

    match create_note_vec_table {
        Ok(_) => (),
        Err(e) => println!("Create note vec table failed: {}", e)
    };

    Ok(())
}

/// Migrate the db from a previous schema to a new one. This is NOT
/// safe to run more than once.
pub fn migrate_db(db: &Connection) -> Result<()> {
    // 2024-12-29 Add columns for type and status
    // 2025-03-30 Add column for category
    let migrated_note_meta_table = db.execute(
        r"BEGIN;

CREATE TABLE IF NOT EXISTS note_meta_new (
    id TEXT PRIMARY KEY,
    file_name TEXT,
    title TEXT,
    category TEXT,
    tags TEXT NULLABLE,
    body TEXT,
    type TEXT DEFAULT 'note',
    status TEXT
);

INSERT INTO note_meta_new (id, file_name, title, category, tags, body)
SELECT id, file_name, title, 'placeholder', tags, body FROM note_meta;

DROP TABLE note_meta;

ALTER TABLE note_meta_new RENAME TO note_meta;

COMMIT;",
        [],
    );

    match migrated_note_meta_table {
        Ok(_) => (),
        Err(e) => println!("Create updated note meta table failed: {}", e)
    }

    Ok(())
}

pub fn vector_db(path_to_db_file: &str) -> Result<Connection> {
    unsafe {
        sqlite3_auto_extension(Some(std::mem::transmute(sqlite3_vec_init as *const ())));
    }
    let db = Connection::open(format!("{}/vector.db", path_to_db_file))?;

    Ok(db)
}
