PRAGMA foreign_keys = ON;
PRAGMA journal_mode = WAL;
CREATE TABLE IF NOT EXISTS sessions (
  id INTEGER PRIMARY KEY,
  title TEXT NOT NULL,
  created_at TEXT NOT NULL DEFAULT (strftime('%Y-%m-%dT%H:%M:%fZ','now'))
);
CREATE TABLE IF NOT EXISTS messages (
  id INTEGER PRIMARY KEY,
  session_id INTEGER NOT NULL REFERENCES sessions(id) ON DELETE CASCADE,
  role TEXT NOT NULL CHECK(role IN ('user','assistant','tool')),
  text TEXT NOT NULL,
  created_at TEXT NOT NULL DEFAULT (strftime('%Y-%m-%dT%H:%M:%fZ','now'))
);
CREATE INDEX IF NOT EXISTS messages_session ON messages(session_id, id);
CREATE VIRTUAL TABLE IF NOT EXISTS messages_fts USING fts5(
  text, content='messages', content_rowid='id', tokenize='unicode61 remove_diacritics 2'
);
CREATE TRIGGER IF NOT EXISTS messages_insert AFTER INSERT ON messages BEGIN
  INSERT INTO messages_fts(rowid,text) VALUES (new.id,new.text);
END;
CREATE TRIGGER IF NOT EXISTS messages_delete AFTER DELETE ON messages BEGIN
  INSERT INTO messages_fts(messages_fts,rowid,text) VALUES ('delete',old.id,old.text);
END;
CREATE TRIGGER IF NOT EXISTS messages_update AFTER UPDATE ON messages BEGIN
  INSERT INTO messages_fts(messages_fts,rowid,text) VALUES ('delete',old.id,old.text);
  INSERT INTO messages_fts(rowid,text) VALUES (new.id,new.text);
END;
