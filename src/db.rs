use rusqlite::{ffi::sqlite3_auto_extension, Connection, Result};
use sqlite_vec::sqlite3_vec_init;


pub fn migrate_db(db: &Connection) -> Result<()> {
    // TODO: Move this to an init function
    // Create a metadata table that has a foreign key to the
    // embeddings virtual table. This will be used to coordinate
    // upserts and hydrating the notes
    db.execute(
        r"CREATE TABLE note_meta (
    id INTEGER PRIMARY KEY AUTOINCREMENT,
    org_id TEXT NOT NULL,
    file_name TEXT NOT NULL,
    vec_id INTEGER,
    FOREIGN KEY (vec_id) REFERENCES vec_items(rowid)
);",
        [],
    )?;
    db.execute(
        "CREATE VIRTUAL TABLE vec_items USING vec0(embedding float[384])",
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
