use crate::{
    config::get_database_file,
    error::{Error, Result},
    types::{Task, TaskContent, TaskStatus},
};
use chrono::{DateTime, Utc};
use log::debug;
use rusqlite::{Connection, Error as SQLiteError, Result as SQLiteResult};
use uris::Uri;

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
	FOREIGN KEY('TASK_ID') REFERENCES 'TASK'('ID') ON DELETE CASCADE,
	PRIMARY KEY('UPDATED','TASK_ID')
);
CREATE TABLE IF NOT EXISTS 'TASK_STATUS' (
	'STATUS'	INTEGER NOT NULL DEFAULT 0,
	'UPDATED'	INTEGER NOT NULL DEFAULT (datetime('now', 'localtime')),
	'TASK_ID'	INTEGER NOT NULL,
	FOREIGN KEY('TASK_ID') REFERENCES 'TASK'('ID') ON DELETE CASCADE,
	PRIMARY KEY('UPDATED','TASK_ID')
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

    pub(super) fn create_task(&self, title: String, priority: Option<u64>) -> Result<u64> {
        if let Some(priority) = priority {
            self.conn.execute(
                "INSERT INTO TASK(TITLE, PRIORITY) VALUES(?, ?)",
                (title, priority),
            )?;
        } else {
            self.conn
                .execute("INSERT INTO TASK(TITLE) VALUES(?)", (title,))?;
        }
        let row_id = self.conn.last_insert_rowid();
        let task_id =
            self.conn
                .query_row("SELECT ID FROM TASK WHERE ROWID = ?", (row_id,), |row| {
                    row.get(0)
                })?;
        self.conn
            .execute("INSERT INTO TASK_STATUS(TASK_ID)", (task_id,))?;
        // TODO: create status
        Ok(task_id)
    }

    pub(super) fn reprioritize_task(&self, task_id: u64, priority: u64) -> Result<()> {
        self.conn
            .execute("UPDATE TASK SET PRIORITY = ?", (priority,))?;
        Ok(())
    }

    pub(super) fn update_content(
        &self,
        task_id: u64,
        body: Option<String>,
        link: Option<String>,
    ) -> Result<()> {
        self.conn.execute(
            "INSERT INTO TASK_CONTENT(TASK_ID, BODY, LINK) VALUES(?, ?, ?)",
            (task_id, body, link),
        )?;
        Ok(())
    }

    pub(super) fn update_status(&self, task_id: u64, state: TaskStatus) -> Result<()> {
        self.conn.execute(
            "INSERT INTO TASK_STATUS(TASK_ID, STATUS) VALUES(?, ?)",
            (task_id, state as u8),
        )?;
        Ok(())
    }

    pub(super) fn get_task(&self, task_id: u64) -> Result<Task> {
        let task_int: u8 = self.conn.query_row(
            "SELECT STATE FROM TASK_STATUS WHERE TASK_ID = ? HAVING MAX(UPDATED)",
            (task_id,),
            |row| row.get(0),
        )?;
        let task_status: TaskStatus = task_int.try_into()?;
        let mut task = self.conn.query_row(
            "SELECT TASK(TITLE, CREATED, PRIORITY) WHERE TASK_ID = ?",
            (task_id,),
            |row| {
                Ok(Task::new(
                    task_id,
                    task_status,
                    row.get(0)?,
                    DateTime::from_timestamp(row.get(1)?, 0)
                        .or(DateTime::from_timestamp(0, 0))
                        .unwrap(),
                    row.get(2)?,
                ))
            },
        )?;
        let content: TaskContent = self.conn.query_row(
            "SELECT BODY, LINK FROM TASK_CONTENT
            WHERE TASK_ID = ?
            GROUP BY TASK_ID HAVING MAX(UPDATED)",
            (task_id,),
            |row| {
                let link = if let Some(link) = row.get(1)? {
                    // if the string fails to parse, we just drop it
                    Uri::parse::<String>(link).ok()
                } else {
                    None
                };
                Ok(TaskContent {
                    body: row.get(0)?,
                    link,
                })
            },
        )?;
        if content.body.is_some() || content.link.is_some() {
            task.content = Some(content);
        }
        Ok(task)
    }
}
