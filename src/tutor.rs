//! The Socratic tutor: system prompt, the tools it can call, and what they do.
//!
//! The prompt's method is adapted from the socratic-tutor skill in
//! https://github.com/janestreet/agent_skills (MIT, © 2026 Jane Street Group,
//! LLC). LICENSE has its notice.

use crate::db::{Db, Subskill, Topic};
use crate::srs::Grade;
use anyhow::Result;
use serde_json::{Value, json};

#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Mode {
    Learn,
    Review,
}

impl Mode {
    pub fn kind(self) -> &'static str {
        match self {
            Self::Learn => "learn",
            Self::Review => "review",
        }
    }
}

pub const LEVEL_NAMES: [&str; 6] = [
    "unaware",
    "heard of it",
    "can follow along",
    "can do with help",
    "independent",
    "can teach it",
];

const RULES: &str = r#"You are a personal Socratic tutor inside a terminal app called Socratik. Take the learner from their current level to real mastery of the topic with the maieutic method: teach mainly by asking questions that force the learner to reason, predict and explain. Do not lecture.

# How the app works
- The app stores the course and shows the skill map in a sidebar. You change the stored state only by calling tools. Never draw the skill map or progress bars yourself.
- Call tools first, then write your reply to the learner after the tool results come back. Do not write a question, call a tool, and then repeat the question.
- Replies show as plain text in a terminal. No markdown headings, tables or bold. Use backticks for code and fenced blocks for multi-line code. Keep replies short.
- Each session starts fresh. The stored notes and the end of the previous session are in "Course state" below. That is all you remember, so keep the notes useful.

# Skill levels
0 unaware. 1 heard of it. 2 can follow along. 3 can do with help. 4 can do independently. 5 can teach it and handle edge cases.

# Starting a new topic (the course state has no subskills)
1. Break the topic into 4-10 concrete subskills (fewer for a narrow topic) and call define_subskills. For a broad topic, cover the first branch and say so.
2. Tell the learner you will ask a few questions, one at a time, to see what they already know.
3. Run a diagnostic of 3-5 probing questions from easy to hard. Teach nothing until it is done.
4. Call update_levels with the starting level of every subskill, then propose where to start. The learner can override.

# Teaching loop
- Ask ONE question at a time. End the message right after the question. No hints, example answers or teaching content after it.
- Prefer questions that make the learner predict an outcome, explain why, find the flaw in a plausible but wrong claim, apply an idea to a new case, or connect two ideas.
- When the learner is wrong, do not give the answer. Ask a narrower question that exposes the contradiction in their reasoning. Explain directly only after 2-3 failed attempts, then re-test the same idea from a different angle.
- When the learner is right, sometimes play devil's advocate. They must be able to defend a correct answer under pressure.
- Give a short explanation (a few sentences) only for new material the learner could not derive, then return to questions.
- Set practical exercises where they fit ("write a query that...", "predict what this code prints") and check what the learner actually wrote.

# Evaluation: be strict
- Levels are earned. Raise a level only after 2+ independent correct checks, at least one of them unprompted recall (not right after the learner was told the answer). Call update_levels when a level changes.
- A vague or partly correct answer is not correct. Probe until you know. Never credit the learner with an insight they did not express.
- Without warning, re-test earlier subskills now and then. If the learner fails, lower the level and revisit.
- Level 5 needs an edge case or adversarial question handled AND an explanation clear enough that you would trust them to teach it.
- Do not flatter. "Not quite. Why would X happen if Y were true?" beats "Great try!".

# Flashcards
When the learner has worked through a fact, definition, distinction or rule worth memorising, call add_flashcards. One fact per card. A question on the front, a short answer on the back, both understandable without this conversation. Never make a card for something the learner has not met yet, and never repeat an existing card. If the learner asks for flashcards, make them for what has been covered.

# Notes
save_notes replaces the stored notes. Keep them under 200 words: misconceptions you saw, what has been covered, what to do next. Call it when these change, and before a natural stopping point.

# Finishing
The course ends when every subskill is at the target level. Then run a final mixed assessment: at least one question per subskill and one integrative problem, one question at a time. If the learner passes, call complete_topic. If not, return to the teaching loop.

# Things the learner can say
- "show progress": give a one-line summary; the sidebar has the map.
- "harder" / "easier": change the difficulty.
- "just tell me": answer the current question directly, this once."#;

const REVIEW_RULES: &str = r#"# This session is a spaced review
The concepts marked DUE in the course state are due for review. Do not teach new material.
- Take the due concepts one at a time. For each, ask one or two unprompted recall or application questions, one question per message. No hints and no recap first.
- After the checks for a concept, call record_review with a grade and the level the performance supports:
  again = could not recall, or wrong. hard = got there only after a narrower follow-up. good = correct and clear. easy = correct at once and handled a twist.
- Lower the level when the learner fails. After a failure, do a brief Socratic repair, then move on.
- When every due concept is recorded, tell the learner in one line that the review is finished."#;

pub struct Context<'a> {
    pub topic: &'a Topic,
    pub subskills: &'a [Subskill],
    pub card_fronts: &'a [String],
    pub previous_session: &'a [(String, String)],
    pub mode: Mode,
    pub now: i64,
}

pub fn system_prompt(ctx: &Context) -> String {
    let mut out = String::from(RULES);
    if ctx.mode == Mode::Review {
        out.push_str("\n\n");
        out.push_str(REVIEW_RULES);
    }

    out.push_str("\n\n# Course state\n");
    out.push_str(&format!(
        "Topic: {}\nTarget level: {}/5\n",
        ctx.topic.title, ctx.topic.target_level
    ));
    if ctx.subskills.is_empty() {
        out.push_str("Subskills: none yet. This is a new topic.\n");
    } else {
        out.push_str("Subskills:\n");
        for s in ctx.subskills {
            let due = if ctx.mode == Mode::Review && s.is_due(ctx.now) {
                "  DUE"
            } else {
                ""
            };
            out.push_str(&format!("- {}: {}/5{due}\n", s.name, s.level));
        }
    }
    if !ctx.topic.notes.is_empty() {
        out.push_str(&format!(
            "\nNotes from earlier sessions:\n{}\n",
            ctx.topic.notes
        ));
    }
    if !ctx.card_fronts.is_empty() {
        out.push_str("\nExisting flashcards (fronts):\n");
        for front in ctx.card_fronts {
            out.push_str(&format!("- {front}\n"));
        }
    }
    if !ctx.previous_session.is_empty() {
        out.push_str("\nEnd of the previous session:\n");
        for (who, text) in ctx.previous_session {
            out.push_str(&format!("{who}: {text}\n"));
        }
    }
    out
}

pub fn tools(mode: Mode) -> Value {
    let level = json!({"type": "integer", "minimum": 0, "maximum": 5});
    let mut tools = vec![
        json!({
            "name": "define_subskills",
            "description": "Add subskills to the topic. Call once at the start of a new topic. Names already present are skipped.",
            "input_schema": {
                "type": "object",
                "properties": {
                    "subskills": {"type": "array", "items": {"type": "string"}, "description": "Short names of at most 24 characters, in teaching order."}
                },
                "required": ["subskills"]
            }
        }),
        json!({
            "name": "update_levels",
            "description": "Set the level of one or more subskills after the diagnostic or when the learner earns or loses a level.",
            "input_schema": {
                "type": "object",
                "properties": {
                    "updates": {"type": "array", "items": {
                        "type": "object",
                        "properties": {
                            "subskill": {"type": "string", "description": "Exact subskill name."},
                            "level": level,
                            "reason": {"type": "string", "description": "The evidence, in a few words."}
                        },
                        "required": ["subskill", "level", "reason"]
                    }}
                },
                "required": ["updates"]
            }
        }),
        json!({
            "name": "add_flashcards",
            "description": "Save flashcards for facts the learner has worked through.",
            "input_schema": {
                "type": "object",
                "properties": {
                    "cards": {"type": "array", "items": {
                        "type": "object",
                        "properties": {
                            "subskill": {"type": "string", "description": "Exact subskill name the card belongs to."},
                            "front": {"type": "string"},
                            "back": {"type": "string"}
                        },
                        "required": ["front", "back"]
                    }}
                },
                "required": ["cards"]
            }
        }),
        json!({
            "name": "save_notes",
            "description": "Replace the stored course notes (misconceptions, coverage, next steps).",
            "input_schema": {
                "type": "object",
                "properties": {"notes": {"type": "string"}},
                "required": ["notes"]
            }
        }),
        json!({
            "name": "complete_topic",
            "description": "Mark the course complete after the learner passes the final assessment.",
            "input_schema": {
                "type": "object",
                "properties": {"summary": {"type": "string", "description": "Weakest areas and what to keep practising."}},
                "required": ["summary"]
            }
        }),
    ];
    if mode == Mode::Review {
        tools.push(json!({
            "name": "record_review",
            "description": "Record the outcome of reviewing due concepts. This schedules the next review.",
            "input_schema": {
                "type": "object",
                "properties": {
                    "results": {"type": "array", "items": {
                        "type": "object",
                        "properties": {
                            "subskill": {"type": "string", "description": "Exact subskill name."},
                            "grade": {"type": "string", "enum": ["again", "hard", "good", "easy"]},
                            "level": level,
                            "note": {"type": "string", "description": "What the learner got right or wrong, in a few words."}
                        },
                        "required": ["subskill", "grade", "level"]
                    }}
                },
                "required": ["results"]
            }
        }));
    }
    Value::Array(tools)
}

fn items<'a>(input: &'a Value, key: &str) -> &'a [Value] {
    input[key].as_array().map(Vec::as_slice).unwrap_or(&[])
}

fn str_of<'a>(v: &'a Value, key: &str) -> &'a str {
    v[key].as_str().unwrap_or("").trim()
}

fn days(interval: f64) -> String {
    let d = interval.round().max(1.0) as i64;
    format!("{d} day{}", if d == 1 { "" } else { "s" })
}

/// Runs one tool call against the database. `Ok` text goes back to the model
/// as the result; `Err` text goes back flagged as an error so it can correct itself.
pub fn execute(
    db: &Db,
    topic_id: i64,
    name: &str,
    input: &Value,
    now: i64,
) -> Result<std::result::Result<String, String>> {
    let unknown = |name: &str, known: &[Subskill]| {
        let names: Vec<_> = known.iter().map(|s| s.name.as_str()).collect();
        format!(
            "Unknown subskill \"{name}\". Valid names: {}",
            names.join(", ")
        )
    };

    Ok(match name {
        "define_subskills" => {
            let names: Vec<String> = items(input, "subskills")
                .iter()
                .filter_map(|v| v.as_str().map(str::to_string))
                .collect();
            if names.is_empty() {
                return Ok(Err("subskills must be a non-empty list of names.".into()));
            }
            let added = db.add_subskills(topic_id, &names)?;
            Ok(format!("Added {added} subskills."))
        }
        "update_levels" => {
            let known = db.subskills(topic_id)?;
            let mut errors = Vec::new();
            for u in items(input, "updates") {
                let name = str_of(u, "subskill");
                match (db.find_subskill(topic_id, name)?, u["level"].as_i64()) {
                    (Some(s), Some(level)) => db.set_level(&s, level, str_of(u, "reason"), now)?,
                    (None, _) => errors.push(unknown(name, &known)),
                    (_, None) => errors.push(format!("Missing level for \"{name}\".")),
                }
            }
            if errors.is_empty() {
                let map: Vec<_> = db
                    .subskills(topic_id)?
                    .iter()
                    .map(|s| format!("{} {}/5", s.name, s.level))
                    .collect();
                Ok(format!("Saved. Levels now: {}", map.join(", ")))
            } else {
                Err(errors.join(" "))
            }
        }
        "add_flashcards" => {
            let mut saved = 0;
            for c in items(input, "cards") {
                let (front, back) = (str_of(c, "front"), str_of(c, "back"));
                if front.is_empty() || back.is_empty() {
                    continue;
                }
                let subskill = db.find_subskill(topic_id, str_of(c, "subskill"))?;
                db.add_card(topic_id, subskill.map(|s| s.id), front, back, now)?;
                saved += 1;
            }
            if saved == 0 {
                Err("No cards saved: each card needs a front and a back.".into())
            } else {
                Ok(format!("Saved {saved} flashcards."))
            }
        }
        "save_notes" => {
            db.set_notes(topic_id, str_of(input, "notes"))?;
            Ok("Notes saved.".into())
        }
        "complete_topic" => {
            let topic = db.topic(topic_id)?;
            let behind: Vec<_> = db
                .subskills(topic_id)?
                .into_iter()
                .filter(|s| s.level < topic.target_level)
                .map(|s| s.name)
                .collect();
            if behind.is_empty() {
                db.set_status(topic_id, "complete")?;
                Ok("Topic marked complete.".into())
            } else {
                Err(format!(
                    "Not complete: these subskills are below the target level {}: {}",
                    topic.target_level,
                    behind.join(", ")
                ))
            }
        }
        "record_review" => {
            let known = db.subskills(topic_id)?;
            let mut done = Vec::new();
            let mut errors = Vec::new();
            for r in items(input, "results") {
                let name = str_of(r, "subskill");
                let Some(s) = db.find_subskill(topic_id, name)? else {
                    errors.push(unknown(name, &known));
                    continue;
                };
                let Some(grade) = Grade::parse(str_of(r, "grade")) else {
                    errors.push(format!("Invalid grade for \"{name}\"."));
                    continue;
                };
                if let Some(level) = r["level"].as_i64() {
                    db.set_level(&s, level, &format!("review: {}", str_of(r, "note")), now)?;
                }
                // Read again: a level drop resets the schedule the review builds on.
                let s = db.find_subskill(topic_id, name)?.unwrap_or(s);
                let sched = db.review_subskill(&s, grade, now)?;
                done.push(format!(
                    "{} next review in {}",
                    s.name,
                    days(sched.interval_days)
                ));
            }
            if errors.is_empty() {
                Ok(format!("Recorded. {}.", done.join("; ")))
            } else {
                Err(errors.join(" "))
            }
        }
        other => Err(format!("Unknown tool \"{other}\".")),
    })
}

/// One line per change, for the transcript.
pub fn describe(name: &str, input: &Value) -> Vec<String> {
    match name {
        "define_subskills" => vec![format!(
            "Skill map created with {} subskills",
            items(input, "subskills").len()
        )],
        "update_levels" => items(input, "updates")
            .iter()
            .map(|u| {
                format!(
                    "{} is now {}/5: {}",
                    str_of(u, "subskill"),
                    u["level"].as_i64().unwrap_or(0),
                    str_of(u, "reason")
                )
            })
            .collect(),
        "add_flashcards" => items(input, "cards")
            .iter()
            .map(|c| format!("Flashcard: {}", str_of(c, "front")))
            .collect(),
        "save_notes" => vec!["Notes updated".to_string()],
        "complete_topic" => vec![format!("Course complete. {}", str_of(input, "summary"))],
        "record_review" => items(input, "results")
            .iter()
            .map(|r| {
                format!(
                    "Reviewed {}: {}, level {}/5",
                    str_of(r, "subskill"),
                    str_of(r, "grade"),
                    r["level"].as_i64().unwrap_or(0)
                )
            })
            .collect(),
        _ => Vec::new(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::srs::DAY;

    fn setup() -> (Db, i64) {
        let db = Db::in_memory().unwrap();
        let t = db.create_topic("SQL joins", 4, 0).unwrap();
        let input = json!({"subskills": ["Inner join", "Outer join"]});
        assert!(
            execute(&db, t, "define_subskills", &input, 0)
                .unwrap()
                .is_ok()
        );
        (db, t)
    }

    #[test]
    fn updates_levels_and_rejects_unknown_names() {
        let (db, t) = setup();
        let ok =
            json!({"updates": [{"subskill": "inner join", "level": 3, "reason": "diagnostic"}]});
        let text = execute(&db, t, "update_levels", &ok, 0).unwrap().unwrap();
        assert!(text.contains("Inner join 3/5"));

        let bad = json!({"updates": [{"subskill": "Cross join", "level": 2, "reason": "x"}]});
        let err = execute(&db, t, "update_levels", &bad, 0)
            .unwrap()
            .unwrap_err();
        assert!(err.contains("Valid names: Inner join, Outer join"));
    }

    #[test]
    fn saves_cards_and_links_them_to_a_subskill() {
        let (db, t) = setup();
        let input = json!({"cards": [
            {"subskill": "Outer join", "front": "Q1", "back": "A1"},
            {"front": "Q2", "back": "A2"},
            {"front": "", "back": "skipped"}
        ]});
        let text = execute(&db, t, "add_flashcards", &input, 5)
            .unwrap()
            .unwrap();
        assert_eq!(text, "Saved 2 flashcards.");
        let cards = db.due_cards(5).unwrap();
        assert_eq!(cards[0].subskill.as_deref(), Some("Outer join"));
        assert_eq!(cards[1].subskill, None);
    }

    #[test]
    fn review_reschedules_and_can_lower_the_level() {
        let (db, t) = setup();
        let s = db.find_subskill(t, "Inner join").unwrap().unwrap();
        db.set_level(&s, 4, "earned", 0).unwrap();

        let good = json!({"results": [{"subskill": "Inner join", "grade": "good", "level": 4}]});
        execute(&db, t, "record_review", &good, DAY)
            .unwrap()
            .unwrap();
        let s = db.find_subskill(t, "Inner join").unwrap().unwrap();
        assert_eq!((s.level, s.due_at, s.sched.reps), (4, Some(2 * DAY), 1));

        let fail = json!({"results": [{"subskill": "Inner join", "grade": "again", "level": 3, "note": "mixed up sides"}]});
        execute(&db, t, "record_review", &fail, 2 * DAY)
            .unwrap()
            .unwrap();
        let s = db.find_subskill(t, "Inner join").unwrap().unwrap();
        assert_eq!((s.level, s.due_at, s.sched.reps), (3, Some(3 * DAY), 0));
    }

    #[test]
    fn a_topic_completes_only_at_the_target_level() {
        let (db, t) = setup();
        let input = json!({"summary": "done"});
        assert!(
            execute(&db, t, "complete_topic", &input, 0)
                .unwrap()
                .is_err()
        );
        for s in db.subskills(t).unwrap() {
            db.set_level(&s, 4, "earned", 0).unwrap();
        }
        assert!(
            execute(&db, t, "complete_topic", &input, 0)
                .unwrap()
                .is_ok()
        );
        assert_eq!(db.topic(t).unwrap().status, "complete");
    }

    #[test]
    fn review_prompt_marks_due_concepts_and_offers_the_review_tool() {
        let (db, t) = setup();
        let s = db.find_subskill(t, "Inner join").unwrap().unwrap();
        db.set_level(&s, 2, "x", 0).unwrap();
        let topic = db.topic(t).unwrap();
        let subskills = db.subskills(t).unwrap();
        let prompt = system_prompt(&Context {
            topic: &topic,
            subskills: &subskills,
            card_fronts: &[],
            previous_session: &[],
            mode: Mode::Review,
            now: DAY,
        });
        assert!(prompt.contains("- Inner join: 2/5  DUE"));
        assert!(prompt.contains("- Outer join: 0/5\n"));
        let names = |m| -> Vec<String> {
            tools(m)
                .as_array()
                .unwrap()
                .iter()
                .map(|t| t["name"].as_str().unwrap().to_string())
                .collect()
        };
        assert!(names(Mode::Review).contains(&"record_review".to_string()));
        assert!(!names(Mode::Learn).contains(&"record_review".to_string()));
    }
}
