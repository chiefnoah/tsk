#![allow(dead_code)]
use crate::{
    config::get_database_file,
    error::{Error, Result},
    types::{Task, TaskContent, TaskId, TaskStatus},
};
use chrono::DateTime;
use log::debug;
use rusqlite::{Connection, Error as SQLiteError, OptionalExtension, Transaction};
use uris::Uri;

impl From<SQLiteError> for Error {
    fn from(value: SQLiteError) -> Self {
        Error::Database(format!("There was a database error: {value:?}"))
    }
}

const INITIALIZE: &'static str = "
CREATE TABLE IF NOT EXISTS TAG (
    NAME TEXT NOT NULL UNIQUE,
    PRIMARY KEY(NAME)
) STRICT;
CREATE TABLE IF NOT EXISTS TASK (
    ID INTEGER NOT NULL UNIQUE,
    TITLE TEXT NOT NULL,
    CREATED INTEGER NOT NULL DEFAULT (CAST(strftime('%s', 'now') as INT)),
    NEXT INTEGER DEFAULT NULL,
    FOREIGN KEY (NEXT) REFERENCES TASK(ID) ON DELETE SET NULL,
    PRIMARY KEY('ID' AUTOINCREMENT)
) STRICT;
INSERT INTO TASK(ID, TITLE, CREATED) VALUES(0, 'ROOT', 0) ON CONFLICT(ID) DO NOTHING;
CREATE TABLE IF NOT EXISTS TASK_CONTENT (
    TASK_ID INTEGER NOT NULL,
    BODY INTEGER NOT NULL,
    LINK TEXT,
    UPDATED INTEGER NOT NULL DEFAULT (CAST(strftime('%s', 'now') as INT)),
    FOREIGN KEY(TASK_ID) REFERENCES TASK(ID) ON DELETE CASCADE,
    FOREIGN KEY(BODY) REFERENCES task_body(rowid) ON DELETE CASCADE,
    PRIMARY KEY(UPDATED, TASK_ID)
) STRICT;
CREATE TABLE IF NOT EXISTS 'TASK_STATUS' (
    STATUS INTEGER NOT NULL DEFAULT 0,
    UPDATED INTEGER NOT NULL DEFAULT (CAST(strftime('%s', 'now') as INT)),
    TASK_ID INTEGER NOT NULL,
    FOREIGN KEY(TASK_ID) REFERENCES TASK(ID) ON DELETE CASCADE,
    PRIMARY KEY(UPDATED,TASK_ID)
) STRICT;
CREATE TABLE IF NOT EXISTS RELATIONSHIP (
    LEFT INTEGER NOT NULL,
    TAG INTEGER NOT NULL,
    RIGHT INTEGER NOT NULL,
    FOREIGN KEY(LEFT) REFERENCES TASK(ID) ON DELETE CASCADE,
    PRIMARY KEY(LEFT, TAG, RIGHT),
    FOREIGN KEY(TAG) REFERENCES TAG(NAME) ON DELETE CASCADE,
    FOREIGN KEY(RIGHT) REFERENCES TASK(ID) ON DELETE CASCADE
) STRICT;

CREATE VIEW IF NOT EXISTS priority_task(id, title, created, next, ordering) AS
WITH RECURSIVE
priority_task(id, title, created, next, ordering) AS (
    SELECT id, title, created, next, 0
    FROM TASK
    WHERE ID = 0
    UNION
    SELECT task.id, task.title, task.created, task.NEXT, priority_task.ordering + 1
    FROM TASK, priority_task
    WHERE task.id = priority_task.next
    LIMIT 20
)
SELECT * FROM priority_task
WHERE priority_task.ID > 0
ORDER BY priority_task.ordering;

CREATE VIEW IF NOT EXISTS top_tasks(id, title, status, next) AS
  SELECT priority_task.id, priority_task.title, task_status.status, priority_task.next
  FROM PRIORITY_TASK
  JOIN TASK_STATUS ON priority_task.ID = task_status.TASK_ID
  GROUP BY task_status.TASK_ID
  HAVING MAX(task_status.UPDATED);

PRAGMA user_version = 1;
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

    pub(super) fn create_task(&mut self, title: String) -> Result<TaskId> {
        let tx = self.conn.transaction()?;
        tx.execute("INSERT INTO TASK(TITLE) VALUES(?)", (title,))?;
        let row_id = tx.last_insert_rowid();
        let task_id = tx.query_row("SELECT ID FROM TASK WHERE ROWID = ?", (row_id,), |row| {
            row.get(0)
        })?;
        update_status(&tx, task_id, TaskStatus::Todo)?;
        tx.commit()?;
        Ok(task_id)
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

    pub(super) fn update_status(&mut self, task_id: u64, state: TaskStatus) -> Result<()> {
        let tx = self.conn.transaction()?;
        update_status(&tx, task_id, state)?;
        tx.commit()?;
        Ok(())
    }

    pub(super) fn get_task(&self, task_id: u64) -> Result<Task> {
        let status_int: u8 = self.conn.query_row(
            "SELECT STATE FROM TASK_STATUS WHERE TASK_ID = ? HAVING MAX(UPDATED)",
            (task_id,),
            |row| row.get(0),
        )?;
        let task_status: TaskStatus = status_int.try_into()?;
        let mut task = self.conn.query_row(
            "SELECT TASK(TITLE, CREATED) WHERE TASK_ID = ?",
            (task_id,),
            |row| {
                Ok(Task::new(
                    task_id,
                    task_status,
                    row.get(0)?,
                    DateTime::from_timestamp(row.get(1)?, 0)
                        .or(DateTime::from_timestamp(0, 0))
                        .unwrap(),
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

    pub(super) fn deprioritize(&self, task_id: TaskId) -> Result<()> {
        let prev: Option<TaskId> = self
            .conn
            .query_row("SELECT ID FROM TASK WHERE NEXT = ?", (task_id,), |row| {
                row.get(0)
            })
            .optional()?;
        if let Some(prev) = prev {
            self.conn.execute(
                "UPDATE TASK SET NEXT = (SELECT NEXT FROM TASK WHERE ID = ?) WHERE ID = ?",
                (task_id, prev),
            )?;
        }
        Ok(())
    }
    pub(super) fn prioritize(&mut self, task_id: TaskId) -> Result<()> {
        let tx = self.conn.transaction()?;
        let parent: Option<TaskId> = tx
            .query_row("SELECT ID FROM TASK WHERE NEXT = ?", (task_id,), |row| {
                row.get(0)
            })
            .optional()?;
        // Remove the task from continuum
        if let Some(parent) = parent {
            tx.execute(
                "UPDATE TASK SET NEXT = (SELECT NEXT FROM TASK WHERE ID = ?) WHERE ID = ?",
                (task_id, parent),
            )?;
        }
        tx.execute("UPDATE TASK SET NEXT = (SELECT NEXT FROM TASK WHERE ID = 0) WHERE ID = ?", (task_id,))?;
        tx.execute("UPDATE TASK SET NEXT = ? WHERE ID = 0", (task_id,))?;
        tx.commit()?;
        Ok(())
    }

    /// `get_top_n_tasks` retrieves the top tasks per the linked-list priority semantics of tasks.
    pub(super) fn get_top_n_tasks(&self, n: u16) -> Result<Vec<Task>> {
        let mut out = Vec::with_capacity(n.into());
        let mut stmt = self.conn.prepare(
            "SELECT ID, STATUS, TITLE, PRIORITY_TASK.CREATED
                      FROM PRIORITY_TASK
                      JOIN TASK_STATUS ON TASK_STATUS.TASK_ID = priority_task.ID
                      GROUP BY TASK_STATUS.TASK_ID
                      HAVING MAX(TASK_STATUS.UPDATED)
                      ORDER BY PRIORITY_TASK.ORDERING
                      LIMIT ?;",
        )?;
        let mut rows = stmt.query((n,))?;
        while let Some(row) = rows.next()? {
            let status_int: u8 = row.get(1)?;
            let status: TaskStatus = status_int.try_into()?;
            out.push(Task::new(
                row.get(0)?,
                status,
                row.get(2)?,
                DateTime::from_timestamp(row.get(3)?, 0)
                    .or(DateTime::from_timestamp(0, 0))
                    .unwrap(),
            ));
        }
        Ok(out)
    }
}
pub(super) fn update_status(tx: &Transaction, task_id: u64, state: TaskStatus) -> Result<()> {
    tx.execute(
        "INSERT INTO TASK_STATUS(TASK_ID, STATUS) VALUES(?, ?)",
        (task_id, state as u8),
    )?;
    Ok(())
}
