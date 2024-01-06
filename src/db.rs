use crate::{
    config::get_database_file,
    error::{Error, Result},
};
use rusqlite::{Connection, Error as SQLiteError, Result as SQLiteResult};
use log::debug;

impl From<SQLiteError> for Error {
    fn from(value: SQLiteError) -> Self {
        Error::Database(format!("There was a database error: {value:?}"))
    }
}

const INITIALIZE: &'static str = "
CREATE TABLE IF NOT EXISTS 'TAG' (
	'NAME'	TEXT NOT NULL UNIQUE,
	PRIMARY KEY('NAME')
);
CREATE TABLE IF NOT EXISTS 'TASK_CONTENT' (
	'TASK_ID'	INTEGER NOT NULL,
	'BODY'	TEXT,
	'LINK'	TEXT,
	'UPDATED'	INTEGER NOT NULL DEFAULT (datetime('now', 'localtime')),
	'STATE'	INTEGER NOT NULL DEFAULT 0,
	PRIMARY KEY('UPDATED','TASK_ID'),
	FOREIGN KEY('TASK_ID') REFERENCES 'TASK'('ID') ON DELETE CASCADE
);
CREATE TABLE IF NOT EXISTS 'TAG' (
	'NAME'	TEXT NOT NULL UNIQUE,
	PRIMARY KEY('NAME')
);
CREATE TABLE IF NOT EXISTS 'RELATIONSHIP' (
	'LEFT'	INTEGER NOT NULL,
	'TAG'	INTEGER NOT NULL,
	'RIGHT'	INTEGER NOT NULL,
	FOREIGN KEY('LEFT') REFERENCES 'TASK'('ID') ON DELETE CASCADE,
	PRIMARY KEY('LEFT','TAG','RIGHT'),
	FOREIGN KEY('TAG') REFERENCES 'TAG'('NAME') ON DELETE CASCADE,
	FOREIGN KEY('RIGHT') REFERENCES 'TASK'('ID') ON DELETE CASCADE
);
CREATE TABLE IF NOT EXISTS '_META' (
	'VERSION'	INTEGER NOT NULL,
	'CREATED'	INTEGER NOT NULL,
	PRIMARY KEY('VERSION')
);
INSERT OR REPLACE INTO _META(VERSION, CREATED) VALUES(0, (datetime('now', 'localtime')));
";

pub(super) struct Db {
    conn: Connection,
}

impl Db {
    pub(super) fn new() -> Result<Db> {
        let db_path = get_database_file()?;
        debug!("Opening databases at {db_path:?}");
        let conn = Connection::open(db_path)?;
        debug!("Database connection opened, initializing...");
        Self::initialize(&conn)?;
        debug!("Database initialized.");
        Ok(Db { conn })
    }

    fn initialize(conn: &Connection) -> Result<()> {
        conn.execute_batch(INITIALIZE)?;
        Ok(())
    }
}
