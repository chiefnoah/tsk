use crate::{
    config::get_database_file,
    error::{Error, Result},
};
use rusqlite::{Connection, Error as SQLiteError, Result as SQLiteResult};

impl From<SQLiteError> for Error {
    fn from(value: SQLiteError) -> Self {
        Error::Database(format!("There was a database error: {value:?}"))
    }
}

const INITIALIZE: &'static str = "
";

pub(super) struct Db {
    conn: Connection,
}

impl Db {
    pub(super) fn new() -> Result<Db> {
        let db_path = get_database_file()?;
        let conn = Connection::open(db_path)?;
        conn.execute_batch(INITIALIZE)?;
        Ok(Db { conn })
    }
}
