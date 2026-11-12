//! SQLite storage: topics, subskills (the concepts), sessions, messages, cards.

use crate::srs::{self, DAY, Grade, Sched};
use anyhow::Result;
use rusqlite::{Connection, OptionalExtension, Row, params};
use serde_json::Value;
use std::path::Path;

/// A failed card comes back after ten minutes; a failed concept the next day.
const CARD_AGAIN_DELAY: i64 = 600;

const SCHEMA: &str = "
PRAGMA foreign_keys = ON;
CREATE TABLE IF NOT EXISTS topics(
    id INTEGER PRIMARY KEY,
    title TEXT NOT NULL,
    target_level INTEGER NOT NULL DEFAULT 4,
    status TEXT NOT NULL DEFAULT 'active',
    notes TEXT NOT NULL DEFAULT '',
    created_at INTEGER NOT NULL
);
CREATE TABLE IF NOT EXISTS subskills(
    id INTEGER PRIMARY KEY,
    topic_id INTEGER NOT NULL REFERENCES topics(id) ON DELETE CASCADE,
    name TEXT NOT NULL,
    position INTEGER NOT NULL,
    level INTEGER NOT NULL DEFAULT 0,
    due_at INTEGER,
    interval_days REAL NOT NULL DEFAULT 0,
    ease REAL NOT NULL DEFAULT 2.5,
    reps INTEGER NOT NULL DEFAULT 0,
    last_reviewed_at INTEGER
);
CREATE TABLE IF NOT EXISTS level_events(
    id INTEGER PRIMARY KEY,
    subskill_id INTEGER NOT NULL REFERENCES subskills(id) ON DELETE CASCADE,
    old_level INTEGER NOT NULL,
    new_level INTEGER NOT NULL,
    reason TEXT NOT NULL,
    at INTEGER NOT NULL
);
CREATE TABLE IF NOT EXISTS sessions(
    id INTEGER PRIMARY KEY,
    topic_id INTEGER NOT NULL REFERENCES topics(id) ON DELETE CASCADE,
    kind TEXT NOT NULL,
    started_at INTEGER NOT NULL,
    input_tokens INTEGER NOT NULL DEFAULT 0,
    output_tokens INTEGER NOT NULL DEFAULT 0
);
CREATE TABLE IF NOT EXISTS messages(
    id INTEGER PRIMARY KEY,
    session_id INTEGER NOT NULL REFERENCES sessions(id) ON DELETE CASCADE,
    role TEXT NOT NULL,
    content TEXT NOT NULL,
    hidden INTEGER NOT NULL DEFAULT 0,
    at INTEGER NOT NULL
);
CREATE TABLE IF NOT EXISTS cards(
    id INTEGER PRIMARY KEY,
    topic_id INTEGER NOT NULL REFERENCES topics(id) ON DELETE CASCADE,
    subskill_id INTEGER REFERENCES subskills(id) ON DELETE SET NULL,
    front TEXT NOT NULL,
    back TEXT NOT NULL,
    created_at INTEGER NOT NULL,
    due_at INTEGER NOT NULL,
    interval_days REAL NOT NULL DEFAULT 0,
    ease REAL NOT NULL DEFAULT 2.5,
    reps INTEGER NOT NULL DEFAULT 0,
    lapses INTEGER NOT NULL DEFAULT 0
);
CREATE TABLE IF NOT EXISTS card_reviews(
    id INTEGER PRIMARY KEY,
    card_id INTEGER NOT NULL REFERENCES cards(id) ON DELETE CASCADE,
    grade TEXT NOT NULL,
    at INTEGER NOT NULL
);
";

#[derive(Clone, Debug)]
pub struct Topic {
    pub id: i64,
    pub title: String,
    pub target_level: i64,
    pub status: String,
    pub notes: String,
}

#[derive(Clone, Debug)]
pub struct Subskill {
    pub id: i64,
    pub name: String,
    pub level: i64,
    pub due_at: Option<i64>,
    pub sched: Sched,
}

impl Subskill {
    pub fn is_due(&self, now: i64) -> bool {
        self.level >= 1 && self.due_at.is_some_and(|d| d <= now)
    }
}

#[derive(Clone, Debug)]
pub struct Card {
    pub id: i64,
    pub topic_title: String,
    pub subskill: Option<String>,
    pub front: String,
    pub back: String,
    pub sched: Sched,
}

#[derive(Clone, Debug)]
pub struct SessionRow {
    pub id: i64,
    pub topic_id: i64,
    pub topic_title: String,
    pub kind: String,
    pub started_at: i64,
    pub input_tokens: i64,
    pub output_tokens: i64,
    pub turns: i64,
}

#[derive(Clone, Debug)]
pub struct StoredMessage {
    pub role: String,
    pub content: Value,
    pub hidden: bool,
}

#[derive(Clone, Debug)]
pub struct LevelEvent {
    pub subskill: String,
    pub old_level: i64,
    pub new_level: i64,
    pub reason: String,
    pub at: i64,
}

pub struct Db {
    conn: Connection,
}

fn topic_from(row: &Row) -> rusqlite::Result<Topic> {
    Ok(Topic {
        id: row.get(0)?,
        title: row.get(1)?,
        target_level: row.get(2)?,
        status: row.get(3)?,
        notes: row.get(4)?,
    })
}

const TOPIC_COLS: &str = "id, title, target_level, status, notes";

fn subskill_from(row: &Row) -> rusqlite::Result<Subskill> {
    Ok(Subskill {
        id: row.get(0)?,
        name: row.get(1)?,
        level: row.get(2)?,
        due_at: row.get(3)?,
        sched: Sched {
            interval_days: row.get(4)?,
            ease: row.get(5)?,
            reps: row.get(6)?,
        },
    })
}

const SUBSKILL_COLS: &str = "id, name, level, due_at, interval_days, ease, reps";

impl Db {
    pub fn open(path: &Path) -> Result<Self> {
        Self::init(Connection::open(path)?)
    }

    #[cfg(test)]
    pub fn in_memory() -> Result<Self> {
        Self::init(Connection::open_in_memory()?)
    }

    fn init(conn: Connection) -> Result<Self> {
        conn.execute_batch(SCHEMA)?;
        // Sessions the learner never answered in (the app closed mid-session).
        conn.execute(
            "DELETE FROM sessions WHERE id NOT IN
             (SELECT session_id FROM messages WHERE role = 'user' AND hidden = 0)",
            [],
        )?;
        Ok(Self { conn })
    }

    // ---- topics ----

    pub fn create_topic(&self, title: &str, target_level: i64, now: i64) -> Result<i64> {
        self.conn.execute(
            "INSERT INTO topics(title, target_level, created_at) VALUES (?1, ?2, ?3)",
            params![title, target_level, now],
        )?;
        Ok(self.conn.last_insert_rowid())
    }

    pub fn topics(&self) -> Result<Vec<Topic>> {
        let mut stmt = self.conn.prepare(&format!(
            "SELECT {TOPIC_COLS} FROM topics ORDER BY created_at DESC, id DESC"
        ))?;
        let rows = stmt.query_map([], topic_from)?;
        Ok(rows.collect::<rusqlite::Result<_>>()?)
    }

    pub fn topic(&self, id: i64) -> Result<Topic> {
        Ok(self.conn.query_row(
            &format!("SELECT {TOPIC_COLS} FROM topics WHERE id = ?1"),
            [id],
            topic_from,
        )?)
    }

    pub fn delete_topic(&self, id: i64) -> Result<()> {
        self.conn
            .execute("DELETE FROM topics WHERE id = ?1", [id])?;
        Ok(())
    }

    pub fn set_notes(&self, topic_id: i64, notes: &str) -> Result<()> {
        self.conn.execute(
            "UPDATE topics SET notes = ?2 WHERE id = ?1",
            params![topic_id, notes],
        )?;
        Ok(())
    }

    pub fn set_status(&self, topic_id: i64, status: &str) -> Result<()> {
        self.conn.execute(
            "UPDATE topics SET status = ?2 WHERE id = ?1",
            params![topic_id, status],
        )?;
        Ok(())
    }

    // ---- subskills ----

    pub fn subskills(&self, topic_id: i64) -> Result<Vec<Subskill>> {
        let mut stmt = self.conn.prepare(&format!(
            "SELECT {SUBSKILL_COLS} FROM subskills WHERE topic_id = ?1 ORDER BY position, id"
        ))?;
        let rows = stmt.query_map([topic_id], subskill_from)?;
        Ok(rows.collect::<rusqlite::Result<_>>()?)
    }

    pub fn find_subskill(&self, topic_id: i64, name: &str) -> Result<Option<Subskill>> {
        Ok(self
            .conn
            .query_row(
                &format!(
                    "SELECT {SUBSKILL_COLS} FROM subskills
                     WHERE topic_id = ?1 AND lower(name) = lower(?2)"
                ),
                params![topic_id, name.trim()],
                subskill_from,
            )
            .optional()?)
    }

    /// Adds the names that the topic does not have yet. Returns how many were added.
    pub fn add_subskills(&self, topic_id: i64, names: &[String]) -> Result<usize> {
        let mut added = 0;
        for name in names {
            let name = name.trim();
            if name.is_empty() || self.find_subskill(topic_id, name)?.is_some() {
                continue;
            }
            self.conn.execute(
                "INSERT INTO subskills(topic_id, name, position)
                 VALUES (?1, ?2, (SELECT COALESCE(MAX(position), 0) + 1 FROM subskills WHERE topic_id = ?1))",
                params![topic_id, name],
            )?;
            added += 1;
        }
        Ok(added)
    }

    /// Records a level change. A drop puts the concept back at the start of its
    /// review schedule; a concept that was never scheduled is due the next day.
    pub fn set_level(&self, s: &Subskill, level: i64, reason: &str, now: i64) -> Result<()> {
        let level = level.clamp(0, 5);
        if level == s.level {
            return Ok(());
        }
        self.conn.execute(
            "INSERT INTO level_events(subskill_id, old_level, new_level, reason, at)
             VALUES (?1, ?2, ?3, ?4, ?5)",
            params![s.id, s.level, level, reason, now],
        )?;
        if level < s.level {
            self.conn.execute(
                "UPDATE subskills SET level = ?2, due_at = ?3, interval_days = 0, reps = 0 WHERE id = ?1",
                params![s.id, level, now + DAY],
            )?;
        } else {
            self.conn.execute(
                "UPDATE subskills SET level = ?2, due_at = COALESCE(due_at, ?3) WHERE id = ?1",
                params![s.id, level, now + DAY],
            )?;
        }
        Ok(())
    }

    /// Reschedules a concept after the tutor graded a review of it.
    pub fn review_subskill(&self, s: &Subskill, grade: Grade, now: i64) -> Result<Sched> {
        let sched = srs::next(s.sched, grade);
        self.conn.execute(
            "UPDATE subskills SET due_at = ?2, interval_days = ?3, ease = ?4, reps = ?5,
             last_reviewed_at = ?6 WHERE id = ?1",
            params![
                s.id,
                now + srs::due_in(&sched, DAY),
                sched.interval_days,
                sched.ease,
                sched.reps,
                now
            ],
        )?;
        Ok(sched)
    }

    pub fn level_events(&self, topic_id: i64, limit: i64) -> Result<Vec<LevelEvent>> {
        let mut stmt = self.conn.prepare(
            "SELECT s.name, e.old_level, e.new_level, e.reason, e.at
             FROM level_events e JOIN subskills s ON s.id = e.subskill_id
             WHERE s.topic_id = ?1 ORDER BY e.id DESC LIMIT ?2",
        )?;
        let rows = stmt.query_map(params![topic_id, limit], |r| {
            Ok(LevelEvent {
                subskill: r.get(0)?,
                old_level: r.get(1)?,
                new_level: r.get(2)?,
                reason: r.get(3)?,
                at: r.get(4)?,
            })
        })?;
        Ok(rows.collect::<rusqlite::Result<_>>()?)
    }

    // ---- cards ----

    pub fn add_card(
        &self,
        topic_id: i64,
        subskill_id: Option<i64>,
        front: &str,
        back: &str,
        now: i64,
    ) -> Result<()> {
        self.conn.execute(
            "INSERT INTO cards(topic_id, subskill_id, front, back, created_at, due_at)
             VALUES (?1, ?2, ?3, ?4, ?5, ?5)",
            params![topic_id, subskill_id, front, back, now],
        )?;
        Ok(())
    }

    fn query_cards(&self, filter: &str, args: &[i64]) -> Result<Vec<Card>> {
        let mut stmt = self.conn.prepare(&format!(
            "SELECT c.id, t.title, s.name, c.front, c.back, c.interval_days, c.ease, c.reps
             FROM cards c JOIN topics t ON t.id = c.topic_id
             LEFT JOIN subskills s ON s.id = c.subskill_id
             {filter}"
        ))?;
        let rows = stmt.query_map(rusqlite::params_from_iter(args), |r| {
            Ok(Card {
                id: r.get(0)?,
                topic_title: r.get(1)?,
                subskill: r.get(2)?,
                front: r.get(3)?,
                back: r.get(4)?,
                sched: Sched {
                    interval_days: r.get(5)?,
                    ease: r.get(6)?,
                    reps: r.get(7)?,
                },
            })
        })?;
        Ok(rows.collect::<rusqlite::Result<_>>()?)
    }

    pub fn due_cards(&self, now: i64) -> Result<Vec<Card>> {
        self.query_cards("WHERE c.due_at <= ?1 ORDER BY c.due_at, c.id", &[now])
    }

    pub fn all_cards(&self) -> Result<Vec<Card>> {
        self.query_cards("ORDER BY t.title, c.id", &[])
    }

    pub fn card_fronts(&self, topic_id: i64) -> Result<Vec<String>> {
        let mut stmt = self
            .conn
            .prepare("SELECT front FROM cards WHERE topic_id = ?1 ORDER BY id")?;
        let rows = stmt.query_map([topic_id], |r| r.get(0))?;
        Ok(rows.collect::<rusqlite::Result<_>>()?)
    }

    /// (total cards, cards due now) for one topic.
    pub fn card_counts(&self, topic_id: i64, now: i64) -> Result<(i64, i64)> {
        Ok(self.conn.query_row(
            "SELECT COUNT(*), COALESCE(SUM(due_at <= ?2), 0) FROM cards WHERE topic_id = ?1",
            params![topic_id, now],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )?)
    }

    pub fn grade_card(&self, card: &Card, grade: Grade, now: i64) -> Result<Sched> {
        let sched = srs::next(card.sched, grade);
        self.conn.execute(
            "UPDATE cards SET due_at = ?2, interval_days = ?3, ease = ?4, reps = ?5,
             lapses = lapses + ?6 WHERE id = ?1",
            params![
                card.id,
                now + srs::due_in(&sched, CARD_AGAIN_DELAY),
                sched.interval_days,
                sched.ease,
                sched.reps,
                (grade == Grade::Again) as i64
            ],
        )?;
        self.conn.execute(
            "INSERT INTO card_reviews(card_id, grade, at) VALUES (?1, ?2, ?3)",
            params![card.id, grade.as_str(), now],
        )?;
        Ok(sched)
    }

    pub fn delete_card(&self, id: i64) -> Result<()> {
        self.conn.execute("DELETE FROM cards WHERE id = ?1", [id])?;
        Ok(())
    }

    // ---- sessions and messages ----

    pub fn create_session(&self, topic_id: i64, kind: &str, now: i64) -> Result<i64> {
        self.conn.execute(
            "INSERT INTO sessions(topic_id, kind, started_at) VALUES (?1, ?2, ?3)",
            params![topic_id, kind, now],
        )?;
        Ok(self.conn.last_insert_rowid())
    }

    pub fn delete_session(&self, id: i64) -> Result<()> {
        self.conn
            .execute("DELETE FROM sessions WHERE id = ?1", [id])?;
        Ok(())
    }

    pub fn add_message(
        &self,
        session_id: i64,
        role: &str,
        content: &Value,
        hidden: bool,
        now: i64,
    ) -> Result<()> {
        self.conn.execute(
            "INSERT INTO messages(session_id, role, content, hidden, at) VALUES (?1, ?2, ?3, ?4, ?5)",
            params![session_id, role, content.to_string(), hidden, now],
        )?;
        Ok(())
    }

    pub fn add_usage(&self, session_id: i64, input: u64, output: u64) -> Result<()> {
        self.conn.execute(
            "UPDATE sessions SET input_tokens = input_tokens + ?2,
             output_tokens = output_tokens + ?3 WHERE id = ?1",
            params![session_id, input as i64, output as i64],
        )?;
        Ok(())
    }

    pub fn messages(&self, session_id: i64) -> Result<Vec<StoredMessage>> {
        let mut stmt = self.conn.prepare(
            "SELECT role, content, hidden FROM messages WHERE session_id = ?1 ORDER BY id",
        )?;
        let rows = stmt.query_map([session_id], |r| {
            let content: String = r.get(1)?;
            Ok(StoredMessage {
                role: r.get(0)?,
                content: serde_json::from_str(&content).unwrap_or(Value::Null),
                hidden: r.get(2)?,
            })
        })?;
        Ok(rows.collect::<rusqlite::Result<_>>()?)
    }

    /// Sessions newest first. `turns` counts the messages the learner typed.
    pub fn sessions(&self) -> Result<Vec<SessionRow>> {
        let mut stmt = self.conn.prepare(
            "SELECT s.id, s.topic_id, t.title, s.kind, s.started_at, s.input_tokens, s.output_tokens,
                    (SELECT COUNT(*) FROM messages m
                     WHERE m.session_id = s.id AND m.role = 'user' AND m.hidden = 0)
             FROM sessions s JOIN topics t ON t.id = s.topic_id
             ORDER BY s.started_at DESC, s.id DESC",
        )?;
        let rows = stmt.query_map([], |r| {
            Ok(SessionRow {
                id: r.get(0)?,
                topic_id: r.get(1)?,
                topic_title: r.get(2)?,
                kind: r.get(3)?,
                started_at: r.get(4)?,
                input_tokens: r.get(5)?,
                output_tokens: r.get(6)?,
                turns: r.get(7)?,
            })
        })?;
        Ok(rows.collect::<rusqlite::Result<_>>()?)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn setup() -> (Db, i64) {
        let db = Db::in_memory().unwrap();
        let topic = db.create_topic("Rust lifetimes", 4, 1000).unwrap();
        (db, topic)
    }

    #[test]
    fn subskills_are_added_once_and_found_case_insensitively() {
        let (db, t) = setup();
        let names = vec!["Borrowing".to_string(), "Elision".to_string()];
        assert_eq!(db.add_subskills(t, &names).unwrap(), 2);
        assert_eq!(
            db.add_subskills(t, &["borrowing".into(), "Variance".into()])
                .unwrap(),
            1
        );
        let all = db.subskills(t).unwrap();
        let order: Vec<_> = all.iter().map(|s| s.name.as_str()).collect();
        assert_eq!(order, ["Borrowing", "Elision", "Variance"]);
        assert!(db.find_subskill(t, " ELISION ").unwrap().is_some());
    }

    #[test]
    fn level_changes_are_logged_and_schedule_reviews() {
        let (db, t) = setup();
        db.add_subskills(t, &["Borrowing".into()]).unwrap();
        let s = db.find_subskill(t, "Borrowing").unwrap().unwrap();
        assert!(!s.is_due(i64::MAX));

        db.set_level(&s, 3, "two correct checks", 1000).unwrap();
        let s = db.find_subskill(t, "Borrowing").unwrap().unwrap();
        assert_eq!((s.level, s.due_at), (3, Some(1000 + DAY)));
        assert!(s.is_due(1000 + DAY) && !s.is_due(1000));

        let sched = db.review_subskill(&s, Grade::Good, 2000).unwrap();
        assert_eq!(sched.reps, 1);
        let s = db.find_subskill(t, "Borrowing").unwrap().unwrap();
        assert_eq!(s.due_at, Some(2000 + DAY));

        db.set_level(&s, 2, "failed review", 3000).unwrap();
        let s = db.find_subskill(t, "Borrowing").unwrap().unwrap();
        assert_eq!((s.level, s.sched.reps, s.due_at), (2, 0, Some(3000 + DAY)));

        let events = db.level_events(t, 10).unwrap();
        assert_eq!(events.len(), 2);
        assert_eq!((events[0].old_level, events[0].new_level), (3, 2));
    }

    #[test]
    fn cards_come_due_and_are_rescheduled() {
        let (db, t) = setup();
        db.add_card(t, None, "What is a lifetime?", "A region of code", 1000)
            .unwrap();
        assert_eq!(db.card_counts(t, 1000).unwrap(), (1, 1));
        let card = db.due_cards(1000).unwrap().remove(0);

        db.grade_card(&card, Grade::Again, 1000).unwrap();
        assert!(db.due_cards(1000 + 599).unwrap().is_empty());
        let card = db.due_cards(1000 + 600).unwrap().remove(0);

        db.grade_card(&card, Grade::Good, 2000).unwrap();
        assert!(db.due_cards(2000 + DAY - 1).unwrap().is_empty());
        assert_eq!(db.due_cards(2000 + DAY).unwrap().len(), 1);
        assert_eq!(db.all_cards().unwrap().len(), 1);
    }

    #[test]
    fn sessions_store_messages_and_cascade_on_topic_delete() {
        let (db, t) = setup();
        let s = db.create_session(t, "learn", 1000).unwrap();
        let content = json!([{"type": "text", "text": "hello"}]);
        db.add_message(s, "user", &content, true, 1000).unwrap();
        db.add_message(s, "assistant", &content, false, 1001)
            .unwrap();
        db.add_message(s, "user", &content, false, 1002).unwrap();
        db.add_usage(s, 10, 5).unwrap();
        db.add_usage(s, 10, 5).unwrap();

        let msgs = db.messages(s).unwrap();
        assert_eq!(msgs.len(), 3);
        assert!(msgs[0].hidden && !msgs[1].hidden);
        assert_eq!(msgs[1].content, content);

        let rows = db.sessions().unwrap();
        assert_eq!((rows[0].turns, rows[0].input_tokens), (1, 20));

        db.delete_topic(t).unwrap();
        assert!(db.sessions().unwrap().is_empty());
        assert!(db.messages(s).unwrap().is_empty());
    }
}
