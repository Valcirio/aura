use rusqlite::{Connection, Result, params};
use serde::{Deserialize, Serialize};
use std::path::Path;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Session {
    pub id: i64,
    pub title: String,
    pub created_at: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Message {
    pub id: i64,
    pub session_id: i64,
    pub role: String,
    pub text: String,
    pub created_at: String,
}

pub struct Memory {
    pub(crate) connection: Connection,
}

impl Memory {
    pub fn open(path: impl AsRef<Path>) -> Result<Self> {
        Self::init(Connection::open(path)?)
    }
    pub fn ephemeral() -> Result<Self> {
        Self::init(Connection::open_in_memory()?)
    }

    fn init(mut connection: Connection) -> Result<Self> {
        connection.busy_timeout(std::time::Duration::from_secs(5))?;
        connection.execute_batch(include_str!("schema.sql"))?;
        crate::store::migrate(&mut connection)?;
        Ok(Self { connection })
    }

    pub fn create_session(&self, title: &str) -> Result<Session> {
        self.connection
            .execute("INSERT INTO sessions(title) VALUES (?1)", [title])?;
        self.connection.query_row(
            "SELECT id,title,created_at FROM sessions WHERE id=?1",
            [self.connection.last_insert_rowid()],
            session_row,
        )
    }

    pub fn sessions(&self) -> Result<Vec<Session>> {
        self.connection
            .prepare("SELECT id,title,created_at FROM sessions ORDER BY id DESC")?
            .query_map([], session_row)?
            .collect()
    }

    pub fn append(&self, session_id: i64, role: &str, text: &str) -> Result<i64> {
        self.connection.execute(
            "INSERT INTO messages(session_id,role,text) VALUES (?1,?2,?3)",
            params![session_id, role, text],
        )?;
        Ok(self.connection.last_insert_rowid())
    }

    pub fn messages(&self, session_id: i64) -> Result<Vec<Message>> {
        self.connection.prepare("SELECT id,session_id,role,text,created_at FROM messages WHERE session_id=?1 ORDER BY id")?
            .query_map([session_id], message_row)?.collect()
    }

    /// Entrada tratada como texto, nunca como uma expressão FTS fornecida pelo usuário.
    pub fn search(&self, query: &str, session_id: Option<i64>, limit: u32) -> Result<Vec<Message>> {
        let expression = query
            .split_whitespace()
            .take(32)
            .map(|word| format!("\"{}\"", word.replace('"', "\"\"")))
            .collect::<Vec<_>>()
            .join(" AND ");
        if expression.is_empty() {
            return Ok(Vec::new());
        }
        self.connection
            .prepare(
                "SELECT m.id,m.session_id,m.role,m.text,m.created_at FROM messages_fts f
             JOIN messages m ON m.id=f.rowid WHERE messages_fts MATCH ?1
             AND (?2 IS NULL OR m.session_id=?2) ORDER BY rank LIMIT ?3",
            )?
            .query_map(
                params![expression, session_id, limit.clamp(1, 50)],
                message_row,
            )?
            .collect()
    }

    pub fn delete_session(&self, id: i64) -> Result<bool> {
        Ok(self
            .connection
            .execute("DELETE FROM sessions WHERE id=?1", [id])?
            > 0)
    }
}

fn session_row(row: &rusqlite::Row<'_>) -> Result<Session> {
    Ok(Session {
        id: row.get(0)?,
        title: row.get(1)?,
        created_at: row.get(2)?,
    })
}
fn message_row(row: &rusqlite::Row<'_>) -> Result<Message> {
    Ok(Message {
        id: row.get(0)?,
        session_id: row.get(1)?,
        role: row.get(2)?,
        text: row.get(3)?,
        created_at: row.get(4)?,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn recalls_previous_sessions_with_accent_insensitive_search() {
        let m = Memory::ephemeral().unwrap();
        let old = m.create_session("Conexão").unwrap();
        let current = m.create_session("Hoje").unwrap();
        m.append(old.id, "user", "Problema de conexão no roteador")
            .unwrap();
        m.append(current.id, "user", "Outra conversa").unwrap();
        assert_eq!(m.search("conexao", None, 5).unwrap()[0].session_id, old.id);
        assert!(
            m.search("roteador", Some(current.id), 5)
                .unwrap()
                .is_empty()
        );
    }

    #[test]
    fn deleting_session_removes_search_index() {
        let m = Memory::ephemeral().unwrap();
        let s = m.create_session("Suporte").unwrap();
        m.append(s.id, "assistant", "Ajuste o roteador").unwrap();
        assert!(m.delete_session(s.id).unwrap());
        assert!(m.search("roteador", None, 10).unwrap().is_empty());
        assert!(m.messages(s.id).unwrap().is_empty());
    }

    #[test]
    fn user_input_cannot_become_fts_syntax() {
        let m = Memory::ephemeral().unwrap();
        for query in ["", "\"", "OR NOT", "NEAR(x)", "*", "' OR 1=1 --"] {
            assert!(m.search(query, None, 10).is_ok(), "{query}");
        }
    }

    #[test]
    fn foreign_keys_and_roles_are_enforced() {
        let m = Memory::ephemeral().unwrap();
        assert!(m.append(999, "user", "texto").is_err());
        let s = m.create_session("Teste").unwrap();
        assert!(m.append(s.id, "invalid", "texto").is_err());
    }

    #[test]
    fn sessions_survive_reopening_database() {
        let path = std::env::temp_dir().join(format!("aura-memory-{}.db", std::process::id()));
        {
            let m = Memory::open(&path).unwrap();
            let s = m.create_session("Persistente").unwrap();
            m.append(s.id, "user", "Lembre do monitor").unwrap();
        }
        let m = Memory::open(&path).unwrap();
        assert!(!m.search("monitor", None, 10).unwrap().is_empty());
        drop(m);
        std::fs::remove_file(path).unwrap();
    }
}
