use rusqlite::{Connection, Result};
use sqlite_vec::sqlite3_vec_init;

pub fn vector_db(path_to_db_file: &str) -> Connection {
    let db = Connection::open(path_to_db_file).expect("Failed to connect to the vector DB");
    db
}
