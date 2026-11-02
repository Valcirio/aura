use crate::{
    calendar::EventDraft,
    error::{AppError, Result},
    memory::{Memory, Message},
};
use rusqlite::{Connection, OptionalExtension, params};
use serde::{Deserialize, Serialize};
use serde_json::Value;

pub(crate) fn migrate(db: &mut Connection) -> rusqlite::Result<()> {
    let version: u32 = db.query_row("PRAGMA user_version", [], |r| r.get(0))?;
    if version > 1 {
        return Err(rusqlite::Error::InvalidQuery);
    }
    if version == 1 {
        return Ok(());
    }
    let tx = db.transaction()?;
    tx.execute_batch("ALTER TABLE sessions ADD COLUMN service_id TEXT;
      ALTER TABLE sessions ADD COLUMN account_id TEXT;
      CREATE TABLE settings(key TEXT PRIMARY KEY, value TEXT NOT NULL);
      CREATE TABLE connections(service_id TEXT PRIMARY KEY, account_id TEXT NOT NULL, label TEXT NOT NULL);
      CREATE TABLE documents(id INTEGER PRIMARY KEY, service_id TEXT NOT NULL, url TEXT NOT NULL,
        title TEXT NOT NULL, text TEXT NOT NULL, updated_at TEXT NOT NULL, UNIQUE(service_id,url));
      CREATE VIRTUAL TABLE documents_fts USING fts5(text,title,content='documents',content_rowid='id', tokenize='unicode61 remove_diacritics 2');
      CREATE TRIGGER docs_insert AFTER INSERT ON documents BEGIN
        INSERT INTO documents_fts(rowid,text,title) VALUES(new.id,new.text,new.title); END;
      CREATE TRIGGER docs_delete AFTER DELETE ON documents BEGIN
        INSERT INTO documents_fts(documents_fts,rowid,text,title) VALUES('delete',old.id,old.text,old.title); END;
      CREATE TRIGGER docs_update AFTER UPDATE ON documents BEGIN
        INSERT INTO documents_fts(documents_fts,rowid,text,title) VALUES('delete',old.id,old.text,old.title);
        INSERT INTO documents_fts(rowid,text,title) VALUES(new.id,new.text,new.title); END;
      CREATE TABLE turns(id TEXT PRIMARY KEY, session_id INTEGER NOT NULL REFERENCES sessions(id) ON DELETE CASCADE,
        generation INTEGER NOT NULL, status TEXT NOT NULL, generated_text TEXT NOT NULL DEFAULT '',
        played_text TEXT NOT NULL DEFAULT '', created_at INTEGER NOT NULL);
      CREATE TABLE operations(id TEXT PRIMARY KEY, session_id INTEGER REFERENCES sessions(id) ON DELETE SET NULL,
        service_id TEXT NOT NULL, account_id TEXT NOT NULL, event TEXT NOT NULL, status TEXT NOT NULL,
        created_at INTEGER NOT NULL, expires_at INTEGER NOT NULL, result TEXT);
      CREATE INDEX operations_session ON operations(session_id,status);
      PRAGMA user_version=1;")?;
    tx.commit()
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Document {
    pub id: i64,
    pub service_id: String,
    pub url: String,
    pub title: String,
    pub text: String,
    pub updated_at: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Operation {
    pub id: String,
    pub session_id: Option<i64>,
    pub service_id: String,
    pub account_id: String,
    pub event: EventDraft,
    pub status: String,
    pub expires_at: i64,
    pub result: Option<Value>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ConnectionInfo {
    pub service_id: String,
    pub account_id: String,
    pub label: String,
}

fn fts(query: &str) -> String {
    query
        .split_whitespace()
        .take(24)
        .map(|v| format!("\"{}\"", v.replace('"', "\"\"")))
        .collect::<Vec<_>>()
        .join(" AND ")
}

impl Memory {
    pub fn set_json(&self, key: &str, value: &Value) -> Result<()> {
        self.connection.execute("INSERT INTO settings VALUES(?1,?2) ON CONFLICT(key) DO UPDATE SET value=excluded.value", params![key,serde_json::to_string(value)?])?;
        Ok(())
    }
    /// Writes several settings entries atomically.
    pub fn set_json_many(&self, entries: &[(&str, &Value)]) -> Result<()> {
        let tx = self.connection.unchecked_transaction()?;
        for (key, value) in entries {
            tx.execute("INSERT INTO settings VALUES(?1,?2) ON CONFLICT(key) DO UPDATE SET value=excluded.value", params![key,serde_json::to_string(value)?])?;
        }
        tx.commit()?;
        Ok(())
    }
    pub fn get_json(&self, key: &str) -> Result<Option<Value>> {
        let value: Option<String> = self
            .connection
            .query_row("SELECT value FROM settings WHERE key=?1", [key], |r| {
                r.get(0)
            })
            .optional()?;
        value
            .map(|v| serde_json::from_str(&v).map_err(AppError::from))
            .transpose()
    }
    pub fn attach_service(
        &self,
        session_id: i64,
        service: &str,
        account: Option<&str>,
    ) -> Result<()> {
        self.connection.execute(
            "UPDATE sessions SET service_id=?1,account_id=?2 WHERE id=?3",
            params![service, account, session_id],
        )?;
        Ok(())
    }
    pub fn session_exists(&self, session_id: i64) -> Result<bool> {
        Ok(self.connection.query_row(
            "SELECT EXISTS(SELECT 1 FROM sessions WHERE id=?1)",
            [session_id],
            |r| r.get(0),
        )?)
    }
    pub fn sessions_page(
        &self,
        before: Option<i64>,
        limit: u32,
    ) -> Result<Vec<crate::memory::Session>> {
        Ok(self.connection.prepare("SELECT id,title,created_at FROM sessions WHERE (?1 IS NULL OR id<?1) ORDER BY id DESC LIMIT ?2")?
            .query_map(params![before,limit.clamp(1,100)],|r|Ok(crate::memory::Session{id:r.get(0)?,title:r.get(1)?,created_at:r.get(2)?}))?.collect::<rusqlite::Result<Vec<_>>>()?)
    }
    pub fn messages_page(
        &self,
        session_id: i64,
        after: Option<i64>,
        limit: u32,
    ) -> Result<Vec<Message>> {
        Ok(self.connection.prepare("SELECT id,session_id,role,text,created_at FROM messages WHERE session_id=?1 AND id>?2 ORDER BY id LIMIT ?3")?
            .query_map(params![session_id,after.unwrap_or(0),limit.clamp(1,100)],|r|Ok(Message{id:r.get(0)?,session_id:r.get(1)?,role:r.get(2)?,text:r.get(3)?,created_at:r.get(4)?}))?.collect::<rusqlite::Result<Vec<_>>>()?)
    }
    pub fn recent_messages(&self, session_id: i64, limit: u32) -> Result<Vec<Message>> {
        let mut values=self.connection.prepare("SELECT id,session_id,role,text,created_at FROM messages WHERE session_id=?1 ORDER BY id DESC LIMIT ?2")?
            .query_map(params![session_id,limit.clamp(1,40)],|r|Ok(Message{id:r.get(0)?,session_id:r.get(1)?,role:r.get(2)?,text:r.get(3)?,created_at:r.get(4)?}))?.collect::<rusqlite::Result<Vec<_>>>()?;
        values.reverse();
        Ok(values)
    }
    pub fn search_history(
        &self,
        query: &str,
        service: Option<&str>,
        exclude_session: Option<i64>,
        since: Option<&str>,
        until: Option<&str>,
    ) -> Result<Vec<Message>> {
        let expression = fts(query);
        if expression.is_empty() {
            if since.is_none() && until.is_none() {
                return Ok(Vec::new());
            }
            return Ok(self.connection.prepare("SELECT m.id,m.session_id,m.role,m.text,m.created_at FROM messages m JOIN sessions s ON s.id=m.session_id
                WHERE m.role IN ('user','assistant') AND (?1 IS NULL OR s.service_id=?1 OR s.service_id IS NULL)
                AND (?2 IS NULL OR m.session_id!=?2) AND (?3 IS NULL OR julianday(m.created_at)>=julianday(?3))
                AND (?4 IS NULL OR julianday(m.created_at)<julianday(?4)) ORDER BY m.id DESC LIMIT 8")?
                .query_map(params![service,exclude_session,since,until],|r|Ok(Message{id:r.get(0)?,session_id:r.get(1)?,role:r.get(2)?,text:r.get(3)?,created_at:r.get(4)?}))?.collect::<rusqlite::Result<Vec<_>>>()?);
        }
        Ok(self
            .connection
            .prepare(
                "SELECT m.id,m.session_id,m.role,m.text,m.created_at FROM messages_fts f
            JOIN messages m ON m.id=f.rowid JOIN sessions s ON s.id=m.session_id
            WHERE messages_fts MATCH ?1 AND m.role IN ('user','assistant') AND (?2 IS NULL OR s.service_id=?2 OR s.service_id IS NULL)
            AND (?3 IS NULL OR m.session_id!=?3) AND (?4 IS NULL OR julianday(m.created_at)>=julianday(?4))
            AND (?5 IS NULL OR julianday(m.created_at)<julianday(?5)) ORDER BY rank LIMIT 8",
            )?
            .query_map(
                params![expression, service, exclude_session, since, until],
                |r| {
                    Ok(Message {
                        id: r.get(0)?,
                        session_id: r.get(1)?,
                        role: r.get(2)?,
                        text: r.get(3)?,
                        created_at: r.get(4)?,
                    })
                },
            )?
            .collect::<rusqlite::Result<Vec<_>>>()?)
    }
    pub fn connection_info(&self) -> Result<Option<ConnectionInfo>> {
        Ok(self.connection.query_row("SELECT service_id,account_id,label FROM connections WHERE service_id='google_calendar'",[],|r|Ok(ConnectionInfo{service_id:r.get(0)?,account_id:r.get(1)?,label:r.get(2)?})).optional()?)
    }
    pub fn save_connection(&self, account_id: &str, label: &str) -> Result<()> {
        self.connection.execute("INSERT INTO connections VALUES('google_calendar',?1,?2) ON CONFLICT(service_id) DO UPDATE SET account_id=excluded.account_id,label=excluded.label",params![account_id,label])?;
        Ok(())
    }
    pub fn remove_connection(&self) -> Result<()> {
        self.connection.execute(
            "DELETE FROM connections WHERE service_id='google_calendar'",
            [],
        )?;
        self.connection.execute(
            "UPDATE operations SET status='cancelled' WHERE status='awaiting_confirmation'",
            [],
        )?;
        Ok(())
    }
    pub fn replace_document(
        &self,
        service: &str,
        url: &str,
        title: &str,
        text: &str,
        updated: &str,
    ) -> Result<()> {
        if text.trim().len() < 40 || text.len() > 250_000 {
            return Err(AppError::invalid("Documento vazio ou muito grande"));
        }
        self.connection.execute("INSERT INTO documents(service_id,url,title,text,updated_at) VALUES(?1,?2,?3,?4,?5)
            ON CONFLICT(service_id,url) DO UPDATE SET title=excluded.title,text=excluded.text,updated_at=excluded.updated_at",params![service,url,title,text,updated])?;
        Ok(())
    }
    pub fn knowledge_search(&self, service: &str, query: &str) -> Result<Vec<Document>> {
        let expression = fts(query);
        if expression.is_empty() {
            return Ok(Vec::new());
        }
        Ok(self.connection.prepare("SELECT d.id,d.service_id,d.url,d.title,snippet(documents_fts,0,'','', ' … ',48),d.updated_at
            FROM documents_fts f JOIN documents d ON d.id=f.rowid WHERE documents_fts MATCH ?1 AND d.service_id=?2 ORDER BY rank LIMIT 6")?
            .query_map(params![expression,service],|r|Ok(Document{id:r.get(0)?,service_id:r.get(1)?,url:r.get(2)?,title:r.get(3)?,text:r.get(4)?,updated_at:r.get(5)?}))?.collect::<rusqlite::Result<Vec<_>>>()?)
    }
    pub fn document_count(&self, service: &str) -> Result<i64> {
        Ok(self.connection.query_row(
            "SELECT count(*) FROM documents WHERE service_id=?1",
            [service],
            |r| r.get(0),
        )?)
    }
    pub fn prepare_operation(
        &mut self,
        session: i64,
        account: &str,
        event: &EventDraft,
        now: i64,
    ) -> Result<Operation> {
        if !self.session_exists(session)? {
            return Err(AppError::invalid("Sessão não encontrada"));
        }
        let id = uuid::Uuid::new_v4().simple().to_string();
        let tx = self.connection.transaction()?;
        tx.execute("UPDATE operations SET status='cancelled' WHERE session_id=?1 AND status='awaiting_confirmation'",[session])?;
        tx.execute("INSERT INTO operations(id,session_id,service_id,account_id,event,status,created_at,expires_at) VALUES(?1,?2,'google_calendar',?3,?4,'awaiting_confirmation',?5,?6)",params![id,session,account,serde_json::to_string(event)?,now,now+600])?;
        tx.commit()?;
        self.operation(&id)
    }
    pub fn operation(&self, id: &str) -> Result<Operation> {
        let row=self.connection.query_row("SELECT id,session_id,service_id,account_id,event,status,expires_at,result FROM operations WHERE id=?1",[id],|r|Ok((r.get::<_,String>(0)?,r.get::<_,Option<i64>>(1)?,r.get::<_,String>(2)?,r.get::<_,String>(3)?,r.get::<_,String>(4)?,r.get::<_,String>(5)?,r.get::<_,i64>(6)?,r.get::<_,Option<String>>(7)?))).optional()?
            .ok_or_else(||AppError::new("ACTION_NOT_FOUND","Proposta não encontrada"))?;
        Ok(Operation {
            id: row.0,
            session_id: row.1,
            service_id: row.2,
            account_id: row.3,
            event: serde_json::from_str(&row.4)?,
            status: row.5,
            expires_at: row.6,
            result: row.7.map(|s| serde_json::from_str(&s)).transpose()?,
        })
    }
    pub fn pending_operation(&self, session: i64) -> Result<Option<Operation>> {
        let id:Option<String>=self.connection.query_row("SELECT id FROM operations WHERE session_id=?1 AND status IN ('awaiting_confirmation','executing','unknown') ORDER BY created_at DESC LIMIT 1",[session],|r|r.get(0)).optional()?;
        id.map(|id| self.operation(&id)).transpose()
    }
    pub fn operation_context(&self, id: &str, result: &Value) -> Result<Operation> {
        self.connection.execute(
            "UPDATE operations SET result=?1 WHERE id=?2 AND status='awaiting_confirmation'",
            params![serde_json::to_string(result)?, id],
        )?;
        self.operation(id)
    }
    pub fn latest_operation(&self, session: i64) -> Result<Option<Operation>> {
        let id:Option<String>=self.connection.query_row("SELECT id FROM operations WHERE session_id=?1 ORDER BY created_at DESC,rowid DESC LIMIT 1",[session],|r|r.get(0)).optional()?;
        id.map(|id| self.operation(&id)).transpose()
    }
    pub fn expire_operations(&self, now: i64) -> Result<()> {
        self.connection.execute("UPDATE operations SET status='expired' WHERE status='awaiting_confirmation' AND expires_at<=?1",[now])?;
        Ok(())
    }
    pub fn claim_operation(
        &mut self,
        id: &str,
        session: i64,
        account: &str,
        now: i64,
    ) -> Result<Operation> {
        let tx = self.connection.transaction()?;
        let changed=tx.execute("UPDATE operations SET status='executing' WHERE id=?1 AND session_id=?2 AND account_id=?3 AND status='awaiting_confirmation' AND expires_at>?4",params![id,session,account,now])?;
        if changed != 1 {
            return Err(AppError::new(
                "ACTION_NOT_CONFIRMABLE",
                "Proposta expirada, modificada, já confirmada ou de outra conta",
            ));
        }
        tx.commit()?;
        self.operation(id)
    }
    pub fn cancel_operation(&self, id: &str, session: i64) -> Result<()> {
        if self.connection.execute("UPDATE operations SET status='cancelled' WHERE id=?1 AND session_id=?2 AND status='awaiting_confirmation'",params![id,session])? !=1 {
            return Err(AppError::new("ACTION_NOT_CANCELLABLE","A proposta não pode mais ser cancelada. Uma criação em andamento precisa ser verificada."));
        }
        Ok(())
    }
    pub fn retry_operation(
        &self,
        id: &str,
        session: i64,
        account: &str,
        context: &Value,
        now: i64,
    ) -> Result<Operation> {
        let changed=self.connection.execute("UPDATE operations SET status='awaiting_confirmation',result=?1,expires_at=?2 WHERE id=?3 AND session_id=?4 AND account_id=?5 AND status='unknown'",params![serde_json::to_string(context)?,now+600,id,session,account])?;
        if changed != 1 {
            return Err(AppError::new(
                "ACTION_NOT_CONFIRMABLE",
                "Não é possível preparar esta tentativa",
            ));
        }
        self.operation(id)
    }
    pub fn finish_operation(
        &self,
        id: &str,
        status: &str,
        result: Option<&Value>,
    ) -> Result<Operation> {
        if !["completed", "unknown", "failed"].contains(&status) {
            return Err(AppError::invalid("Estado de operação inválido"));
        }
        self.connection.execute("UPDATE operations SET status=?1,result=?2 WHERE id=?3 AND status IN ('executing','unknown')",params![status,result.map(serde_json::to_string).transpose()?,id])?;
        self.operation(id)
    }
    pub fn recover_operations(&self) -> Result<()> {
        self.connection.execute(
            "UPDATE operations SET status='unknown' WHERE status='executing'",
            [],
        )?;
        Ok(())
    }
    pub fn create_turn(&self, id: &str, session: i64, generation: u64, now: i64) -> Result<()> {
        let generation =
            i64::try_from(generation).map_err(|_| AppError::invalid("Geração inválida"))?;
        self.connection.execute("INSERT INTO turns(id,session_id,generation,status,created_at) VALUES(?1,?2,?3,'running',?4)",params![id,session,generation,now])?;
        Ok(())
    }
    pub fn finish_turn(&self, id: &str, status: &str, text: &str, played: &str) -> Result<()> {
        self.connection.execute(
            "UPDATE turns SET status=?1,generated_text=?2,played_text=?3 WHERE id=?4",
            params![status, text, played, id],
        )?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::calendar::EventInput;
    fn draft() -> EventDraft {
        EventDraft::validate(EventInput {
            calendar_id: "primary".into(),
            summary: "Reunião".into(),
            start: "2026-10-30T14:00:00-03:00".into(),
            end: "2026-10-30T15:00:00-03:00".into(),
            time_zone: "America/Bahia".into(),
            all_day: false,
            description: String::new(),
        })
        .unwrap()
    }
    #[test]
    fn replaces_pending_draft_and_only_claims_once() {
        let mut m = Memory::ephemeral().unwrap();
        let s = m.create_session("s").unwrap();
        let first = m
            .prepare_operation(s.id, "account-a", &draft(), 100)
            .unwrap();
        let second = m
            .prepare_operation(s.id, "account-a", &draft(), 101)
            .unwrap();
        assert!(
            m.claim_operation(&first.id, s.id, "account-a", 102)
                .is_err()
        );
        assert!(
            m.claim_operation(&second.id, s.id, "account-b", 102)
                .is_err()
        );
        assert!(
            m.claim_operation(&second.id, s.id, "account-a", 102)
                .is_ok()
        );
        assert!(
            m.claim_operation(&second.id, s.id, "account-a", 102)
                .is_err()
        );
    }
    #[test]
    fn expired_proposal_cannot_be_confirmed() {
        let mut m = Memory::ephemeral().unwrap();
        let s = m.create_session("s").unwrap();
        let op = m.prepare_operation(s.id, "a", &draft(), 100).unwrap();
        assert!(m.claim_operation(&op.id, s.id, "a", 701).is_err());
    }
    #[test]
    fn knowledge_is_separate_and_replace_updates_fts() {
        let m = Memory::ephemeral().unwrap();
        m.replace_document(
            "google_calendar",
            "https://example.test/a",
            "Agenda",
            "Como criar um compromisso no calendário usando a ferramenta de eventos.",
            "2026",
        )
        .unwrap();
        assert_eq!(
            m.knowledge_search("google_calendar", "compromisso")
                .unwrap()
                .len(),
            1
        );
        assert!(
            m.knowledge_search("other_service", "compromisso")
                .unwrap()
                .is_empty()
        );
        assert!(m.search("compromisso", None, 8).unwrap().is_empty());
        assert!(
            m.replace_document(
                "google_calendar",
                "https://example.test/a",
                "Agenda",
                "",
                "2026"
            )
            .is_err()
        );
        assert_eq!(
            m.knowledge_search("google_calendar", "compromisso")
                .unwrap()
                .len(),
            1
        );
    }
    #[test]
    fn migration_preserves_legacy_sessions_without_product_assignment() {
        let mut connection = Connection::open_in_memory().unwrap();
        connection
            .execute_batch(include_str!("schema.sql"))
            .unwrap();
        connection
            .execute("INSERT INTO sessions(title) VALUES('Legado')", [])
            .unwrap();
        migrate(&mut connection).unwrap();
        migrate(&mut connection).unwrap();
        let (title, service): (String, Option<String>) = connection
            .query_row("SELECT title,service_id FROM sessions", [], |r| {
                Ok((r.get(0)?, r.get(1)?))
            })
            .unwrap();
        assert_eq!(title, "Legado");
        assert!(service.is_none());
    }
    #[test]
    fn crash_recovery_preserves_id_without_repeating_write() {
        let mut m = Memory::ephemeral().unwrap();
        let s = m.create_session("s").unwrap();
        let op = m.prepare_operation(s.id, "a", &draft(), 100).unwrap();
        m.claim_operation(&op.id, s.id, "a", 101).unwrap();
        m.recover_operations().unwrap();
        assert_eq!(m.operation(&op.id).unwrap().status, "unknown");
        assert!(m.claim_operation(&op.id, s.id, "a", 102).is_err());
    }
    #[test]
    fn date_only_recall_respects_offsets_and_exclusive_upper_bound() {
        let m = Memory::ephemeral().unwrap();
        let session = m.create_session("Ontem").unwrap();
        let id = m
            .append(session.id, "user", "Uma dúvida sem palavra-chave conhecida")
            .unwrap();
        m.connection
            .execute(
                "UPDATE messages SET created_at='2026-10-29T02:30:00.000Z' WHERE id=?1",
                [id],
            )
            .unwrap();
        let hits = m
            .search_history(
                "",
                None,
                None,
                Some("2026-10-28T00:00:00-03:00"),
                Some("2026-10-29T00:00:00-03:00"),
            )
            .unwrap();
        assert_eq!(hits.len(), 1);
        let excluded = m
            .search_history("", None, None, Some("2026-10-29T00:00:00-03:00"), None)
            .unwrap();
        assert!(excluded.is_empty());
    }
}
