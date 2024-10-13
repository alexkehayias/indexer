use rusqlite::{ffi::sqlite3_auto_extension, Connection, Result};
use sqlite_vec::sqlite3_vec_init;


pub fn migrate_db(db: &Connection) -> Result<()> {
    // Create a metadata table that has a foreign key to the
    // embeddings virtual table. This will be used to coordinate
    // upserts and hydrating the notes
    db.execute(
        r"CREATE TABLE IF NOT EXISTS note_meta (
    id TEXT PRIMARY KEY
);",
        [],
    )?;

    // Create vector virtual table for similarity search
    db.execute(
        "CREATE VIRTUAL TABLE IF NOT EXISTS vec_items USING vec0(
note_meta_id TEXT PRIMARY KEY,
embedding float[384]
)",
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
