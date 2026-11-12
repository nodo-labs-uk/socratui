//! Application state and key handling.

use crate::api::{self, Reply, StreamEvent};
use crate::db::{Card, Db, LevelEvent, SessionRow, Subskill, Topic};
use crate::export;
use crate::input::Input;
use crate::provider::{Kind, Provider};
use crate::srs::Grade;
use crate::tutor::{self, Mode};
use anyhow::Result;
use ratatui::crossterm::event::{Event, KeyCode, KeyEvent, KeyEventKind, KeyModifiers};
use serde_json::{Value, json};
use std::collections::VecDeque;
use std::path::PathBuf;
use std::sync::mpsc::{self, Receiver, Sender};

const PREVIOUS_SESSION_MESSAGES: usize = 8;
const PREVIOUS_SESSION_CHARS: usize = 600;
pub const CONFIG_FILE: &str = "config.json";

pub fn now() -> i64 {
    chrono::Utc::now().timestamp()
}

/// One entry in a transcript.
pub enum Item {
    User(String),
    Tutor(String),
    /// A change the tutor made: a level, a flashcard, a review result.
    Event(String),
    Notice(String),
}

pub struct Chat {
    pub topic: Topic,
    pub session_id: i64,
    pub mode: Mode,
    pub subskills: Vec<Subskill>,
    pub items: Vec<Item>,
    pub streaming: String,
    pub busy: bool,
    pub error: Option<String>,
    pub input: Input,
    /// Lines scrolled up from the bottom of the transcript.
    pub scroll: usize,
    pub tokens: (u64, u64),
    /// A past session opened from the history screen.
    pub read_only: bool,
    system: String,
    messages: Vec<Value>,
    turn_id: u64,
}

pub struct CardReview {
    pub queue: VecDeque<Card>,
    pub revealed: bool,
    pub done: usize,
}

pub enum Screen {
    Home,
    Chat(Box<Chat>),
    Cards(CardReview),
    History,
}

pub enum ConfirmAction {
    DeleteTopic(i64),
    DeleteCard(i64),
}

pub enum Modal {
    NewTopic {
        input: Input,
        target: i64,
    },
    Confirm {
        message: String,
        action: ConfirmAction,
    },
    Setup(Setup),
}

#[derive(Clone, Copy, PartialEq)]
pub enum SetupStep {
    Provider,
    Url,
    Key,
    Model,
}

/// Choosing who tutors, one question at a time.
pub struct Setup {
    pub kind: Kind,
    pub step: SetupStep,
    pub input: Input,
    /// True when Enter on an empty key field keeps the saved key.
    pub has_saved_key: bool,
    url: String,
    key: String,
}

pub struct TopicRow {
    pub topic: Topic,
    pub subskills: Vec<Subskill>,
    pub events: Vec<LevelEvent>,
    pub cards_total: i64,
    pub cards_due: i64,
    pub concepts_due: usize,
}

impl TopicRow {
    pub fn progress(&self) -> f64 {
        progress(&self.subskills, self.topic.target_level)
    }
}

/// Share of the way to the target level, across all subskills.
pub fn progress(subskills: &[Subskill], target: i64) -> f64 {
    if subskills.is_empty() {
        return 0.0;
    }
    let earned: i64 = subskills.iter().map(|s| s.level.min(target)).sum();
    earned as f64 / (subskills.len() as i64 * target) as f64
}

pub struct App {
    pub db: Db,
    pub provider: Option<Provider>,
    pub screen: Screen,
    pub modal: Option<Modal>,
    pub topics: Vec<TopicRow>,
    pub selected: usize,
    pub history: Vec<SessionRow>,
    pub history_selected: usize,
    pub status: String,
    pub quit: bool,
    data_dir: PathBuf,
    tx: Sender<(u64, StreamEvent)>,
    rx: Receiver<(u64, StreamEvent)>,
    next_turn: u64,
}

fn text_message(text: &str) -> Value {
    json!([{"type": "text", "text": text}])
}

fn texts(content: &Value) -> impl Iterator<Item = &str> {
    content
        .as_array()
        .into_iter()
        .flatten()
        .filter(|b| b["type"] == "text")
        .filter_map(|b| b["text"].as_str())
}

fn items_from(role: &str, content: &Value, hidden: bool) -> Vec<Item> {
    if hidden {
        return Vec::new();
    }
    if role == "user" {
        return texts(content).map(|t| Item::User(t.to_string())).collect();
    }
    let mut items = Vec::new();
    for block in content.as_array().into_iter().flatten() {
        match block["type"].as_str() {
            Some("text") => items.push(Item::Tutor(
                block["text"].as_str().unwrap_or("").trim().to_string(),
            )),
            Some("tool_use") => items.extend(
                tutor::describe(block["name"].as_str().unwrap_or(""), &block["input"])
                    .into_iter()
                    .map(Item::Event),
            ),
            _ => {}
        }
    }
    items
}

fn truncate(text: &str, max: usize) -> String {
    if text.chars().count() <= max {
        text.to_string()
    } else {
        format!("{}...", text.chars().take(max).collect::<String>())
    }
}

impl App {
    pub fn new(db: Db, provider: Option<Provider>, data_dir: PathBuf) -> Result<Self> {
        let (tx, rx) = mpsc::channel();
        let mut app = Self {
            db,
            provider,
            screen: Screen::Home,
            modal: None,
            topics: Vec::new(),
            selected: 0,
            history: Vec::new(),
            history_selected: 0,
            status: String::new(),
            quit: false,
            data_dir,
            tx,
            rx,
            next_turn: 0,
        };
        app.refresh_home()?;
        if app.provider.is_none() {
            app.open_setup();
        }
        Ok(app)
    }

    fn refresh_home(&mut self) -> Result<()> {
        let now = now();
        self.topics.clear();
        for topic in self.db.topics()? {
            let subskills = self.db.subskills(topic.id)?;
            let (cards_total, cards_due) = self.db.card_counts(topic.id, now)?;
            self.topics.push(TopicRow {
                events: self.db.level_events(topic.id, 6)?,
                concepts_due: subskills.iter().filter(|s| s.is_due(now)).count(),
                cards_total,
                cards_due,
                subskills,
                topic,
            });
        }
        self.selected = self.selected.min(self.topics.len().saturating_sub(1));
        Ok(())
    }

    pub fn cards_due(&self) -> i64 {
        self.topics.iter().map(|t| t.cards_due).sum()
    }

    pub fn concepts_due(&self) -> usize {
        self.topics.iter().map(|t| t.concepts_due).sum()
    }

    // ---- events ----

    pub fn on_event(&mut self, event: Event) -> Result<()> {
        match event {
            Event::Key(key) if key.kind == KeyEventKind::Press => {
                if key.modifiers.contains(KeyModifiers::CONTROL) && key.code == KeyCode::Char('c') {
                    self.quit = true;
                    return Ok(());
                }
                if self.modal.is_some() {
                    return self.modal_key(key);
                }
                match self.screen {
                    Screen::Home => self.home_key(key),
                    Screen::Chat(_) => self.chat_key(key),
                    Screen::Cards(_) => self.cards_key(key),
                    Screen::History => self.history_key(key),
                }
            }
            Event::Paste(text) => {
                match (&mut self.modal, &mut self.screen) {
                    (Some(Modal::NewTopic { input, .. }), _) => {
                        input.insert_str(&text.replace('\n', " "))
                    }
                    (Some(Modal::Setup(setup)), _) if setup.step != SetupStep::Provider => {
                        setup.input.insert_str(text.trim())
                    }
                    (None, Screen::Chat(chat)) if !chat.read_only => chat.input.insert_str(&text),
                    _ => {}
                }
                Ok(())
            }
            _ => Ok(()),
        }
    }

    fn open_setup(&mut self) {
        self.modal = Some(Modal::Setup(Setup {
            kind: self.provider.as_ref().map_or(Kind::Anthropic, |p| p.kind),
            step: SetupStep::Provider,
            input: Input::default(),
            has_saved_key: false,
            url: String::new(),
            key: String::new(),
        }));
    }

    fn setup_key(&mut self, key: KeyEvent) -> Result<()> {
        let Some(Modal::Setup(setup)) = &mut self.modal else {
            return Ok(());
        };
        // What is saved for the chosen provider, to offer as the starting value.
        let saved = self.provider.as_ref().filter(|p| p.kind == setup.kind);
        let go = |setup: &mut Setup, step: SetupStep| {
            setup.step = step;
            setup.input.clear();
            match step {
                SetupStep::Url => setup
                    .input
                    .insert_str(saved.map_or(setup.kind.default_url(), |p| &p.base_url)),
                SetupStep::Model => setup
                    .input
                    .insert_str(saved.map_or(setup.kind.default_model(), |p| &p.model)),
                _ => {}
            }
            setup.has_saved_key = saved.is_some();
        };
        let first = |kind: Kind| {
            if kind == Kind::Local {
                SetupStep::Url
            } else {
                SetupStep::Key
            }
        };
        let text = setup.input.text().trim().to_string();

        match (setup.step, key.code) {
            (SetupStep::Provider, KeyCode::Esc) => self.modal = None,
            (SetupStep::Provider, KeyCode::Left) => {
                let i = Kind::ALL.iter().position(|k| *k == setup.kind).unwrap_or(0);
                setup.kind = Kind::ALL[i.saturating_sub(1)];
            }
            (SetupStep::Provider, KeyCode::Right | KeyCode::Tab) => {
                let i = Kind::ALL.iter().position(|k| *k == setup.kind).unwrap_or(0);
                setup.kind = Kind::ALL[(i + 1).min(Kind::ALL.len() - 1)];
            }
            (SetupStep::Provider, KeyCode::Enter) => go(setup, first(setup.kind)),
            (SetupStep::Provider, KeyCode::Char(c @ '1'..='4')) => {
                setup.kind = Kind::ALL[c as usize - '1' as usize];
                // `saved` was looked up for the previous choice, so start over.
                return self.setup_key(KeyEvent::from(KeyCode::Enter));
            }
            (SetupStep::Provider, _) => {}
            (_, KeyCode::Esc) => go(setup, SetupStep::Provider),
            (SetupStep::Url, KeyCode::Enter) if !text.is_empty() => {
                setup.url = text;
                go(setup, SetupStep::Model);
            }
            (SetupStep::Key, KeyCode::Enter) if !text.is_empty() || saved.is_some() => {
                setup.key = if text.is_empty() {
                    saved.map(|p| p.key.clone()).unwrap_or_default()
                } else {
                    text
                };
                setup.url = setup.kind.default_url().to_string();
                go(setup, SetupStep::Model);
            }
            (SetupStep::Model, KeyCode::Enter) if !text.is_empty() => {
                let provider = Provider {
                    kind: setup.kind,
                    base_url: setup.url.clone(),
                    key: setup.key.clone(),
                    model: text,
                };
                provider.save(&self.data_dir.join(CONFIG_FILE))?;
                self.status = format!(
                    "The tutor is now {} ({}).",
                    provider.kind.label(),
                    provider.model
                );
                self.provider = Some(provider);
                self.modal = None;
            }
            _ => edit_key(&mut setup.input, key),
        }
        Ok(())
    }

    fn modal_key(&mut self, key: KeyEvent) -> Result<()> {
        let Some(modal) = &mut self.modal else {
            return Ok(());
        };
        match modal {
            Modal::Setup(_) => return self.setup_key(key),
            Modal::NewTopic { input, target } => match key.code {
                KeyCode::Esc => self.modal = None,
                KeyCode::Tab => *target = if *target == 4 { 5 } else { 4 },
                KeyCode::Enter if !input.is_blank() => {
                    let id = self.db.create_topic(input.text().trim(), *target, now())?;
                    self.modal = None;
                    self.start_chat(id, Mode::Learn)?;
                }
                _ => edit_key(input, key),
            },
            Modal::Confirm { action, .. } => match key.code {
                KeyCode::Char('y') | KeyCode::Enter => {
                    match action {
                        ConfirmAction::DeleteTopic(id) => {
                            self.db.delete_topic(*id)?;
                            self.status = "Topic deleted.".into();
                        }
                        ConfirmAction::DeleteCard(id) => {
                            self.db.delete_card(*id)?;
                            if let Screen::Cards(review) = &mut self.screen {
                                review.queue.pop_front();
                                review.revealed = false;
                            }
                        }
                    }
                    self.modal = None;
                    self.refresh_home()?;
                }
                KeyCode::Char('n') | KeyCode::Esc => self.modal = None,
                _ => {}
            },
        }
        Ok(())
    }

    fn home_key(&mut self, key: KeyEvent) -> Result<()> {
        self.status.clear();
        let topic = self
            .topics
            .get(self.selected)
            .map(|t| (t.topic.id, t.topic.title.clone()));
        match key.code {
            KeyCode::Char('q') => self.quit = true,
            KeyCode::Down | KeyCode::Char('j') => {
                self.selected = (self.selected + 1).min(self.topics.len().saturating_sub(1))
            }
            KeyCode::Up | KeyCode::Char('k') => self.selected = self.selected.saturating_sub(1),
            KeyCode::Char('n') => {
                self.modal = Some(Modal::NewTopic {
                    input: Input::default(),
                    target: 4,
                })
            }
            KeyCode::Enter => {
                if let Some((id, _)) = topic {
                    self.start_chat(id, Mode::Learn)?;
                }
            }
            KeyCode::Char('r') => {
                if let Some((id, _)) = topic {
                    if self.topics[self.selected].concepts_due > 0 {
                        self.start_chat(id, Mode::Review)?;
                    } else {
                        self.status = "No concepts are due for review in this topic.".into();
                    }
                }
            }
            KeyCode::Char('f') => {
                let queue: VecDeque<Card> = self.db.due_cards(now())?.into();
                if queue.is_empty() {
                    self.status = "No flashcards are due.".into();
                } else {
                    self.screen = Screen::Cards(CardReview {
                        queue,
                        revealed: false,
                        done: 0,
                    });
                }
            }
            KeyCode::Char('h') => {
                self.history = self.db.sessions()?;
                self.history_selected = 0;
                self.screen = Screen::History;
            }
            KeyCode::Char('e') => self.export()?,
            KeyCode::Char('s') => self.open_setup(),
            KeyCode::Char('d') => {
                if let Some((id, title)) = topic {
                    self.modal = Some(Modal::Confirm {
                        message: format!("Delete \"{title}\" with its sessions and flashcards?"),
                        action: ConfirmAction::DeleteTopic(id),
                    });
                }
            }
            _ => {}
        }
        Ok(())
    }

    fn export(&mut self) -> Result<()> {
        let cards = self.db.all_cards()?;
        if cards.is_empty() {
            self.status = "There are no flashcards to export yet.".into();
            return Ok(());
        }
        let path = self.data_dir.join("anki-export.txt");
        std::fs::write(&path, export::anki_text(&cards))?;
        let mut shown = path.display().to_string();
        if let Some(home) = dirs::home_dir().map(|h| h.display().to_string())
            && let Some(rest) = shown.strip_prefix(&home)
        {
            shown = format!("~{rest}");
        }
        self.status = format!(
            "Exported {} cards for Anki (File > Import): {shown}",
            cards.len()
        );
        Ok(())
    }

    fn history_key(&mut self, key: KeyEvent) -> Result<()> {
        match key.code {
            KeyCode::Esc | KeyCode::Char('q') => self.screen = Screen::Home,
            KeyCode::Down | KeyCode::Char('j') => {
                self.history_selected =
                    (self.history_selected + 1).min(self.history.len().saturating_sub(1))
            }
            KeyCode::Up | KeyCode::Char('k') => {
                self.history_selected = self.history_selected.saturating_sub(1)
            }
            KeyCode::Enter => {
                if let Some(row) = self.history.get(self.history_selected) {
                    let items = self
                        .db
                        .messages(row.id)?
                        .iter()
                        .flat_map(|m| items_from(&m.role, &m.content, m.hidden))
                        .collect();
                    let mode = if row.kind == "review" {
                        Mode::Review
                    } else {
                        Mode::Learn
                    };
                    self.screen = Screen::Chat(Box::new(Chat {
                        topic: self.db.topic(row.topic_id)?,
                        session_id: row.id,
                        mode,
                        subskills: self.db.subskills(row.topic_id)?,
                        items,
                        streaming: String::new(),
                        busy: false,
                        error: None,
                        input: Input::default(),
                        scroll: usize::MAX,
                        tokens: (row.input_tokens as u64, row.output_tokens as u64),
                        read_only: true,
                        system: String::new(),
                        messages: Vec::new(),
                        turn_id: 0,
                    }));
                }
            }
            _ => {}
        }
        Ok(())
    }

    fn cards_key(&mut self, key: KeyEvent) -> Result<()> {
        let Screen::Cards(review) = &mut self.screen else {
            return Ok(());
        };
        let grade = match key.code {
            KeyCode::Char('1') => Some(Grade::Again),
            KeyCode::Char('2') => Some(Grade::Hard),
            KeyCode::Char('3') => Some(Grade::Good),
            KeyCode::Char('4') => Some(Grade::Easy),
            _ => None,
        };
        match key.code {
            KeyCode::Esc | KeyCode::Char('q') => {
                self.screen = Screen::Home;
                self.refresh_home()?;
            }
            KeyCode::Char(' ') | KeyCode::Enter => review.revealed = true,
            KeyCode::Char('d') => {
                if let Some(card) = review.queue.front() {
                    self.modal = Some(Modal::Confirm {
                        message: "Delete this flashcard?".into(),
                        action: ConfirmAction::DeleteCard(card.id),
                    });
                }
            }
            _ => {
                if let (Some(grade), true, Some(mut card)) =
                    (grade, review.revealed, review.queue.pop_front())
                {
                    card.sched = self.db.grade_card(&card, grade, now())?;
                    if grade == Grade::Again {
                        // Failed cards come round again before the review ends.
                        review.queue.push_back(card);
                    } else {
                        review.done += 1;
                    }
                    review.revealed = false;
                    self.refresh_home()?;
                }
            }
        }
        Ok(())
    }

    // ---- tutoring ----

    /// The visible tail of the topic's most recent session, for continuity.
    fn previous_session(&self, topic_id: i64) -> Result<Vec<(String, String)>> {
        let Some(last) = self
            .db
            .sessions()?
            .into_iter()
            .find(|s| s.topic_id == topic_id && s.turns > 0)
        else {
            return Ok(Vec::new());
        };
        let mut lines = Vec::new();
        for m in self.db.messages(last.id)?.iter().filter(|m| !m.hidden) {
            let who = if m.role == "user" { "Learner" } else { "Tutor" };
            for text in texts(&m.content) {
                lines.push((
                    who.to_string(),
                    truncate(text.trim(), PREVIOUS_SESSION_CHARS),
                ));
            }
        }
        let skip = lines.len().saturating_sub(PREVIOUS_SESSION_MESSAGES);
        Ok(lines.split_off(skip))
    }

    fn start_chat(&mut self, topic_id: i64, mode: Mode) -> Result<()> {
        let now = now();
        let topic = self.db.topic(topic_id)?;
        let subskills = self.db.subskills(topic_id)?;
        let system = tutor::system_prompt(&tutor::Context {
            topic: &topic,
            subskills: &subskills,
            card_fronts: &self.db.card_fronts(topic_id)?,
            previous_session: &self.previous_session(topic_id)?,
            mode,
            now,
        });
        let opening = match (mode, subskills.is_empty()) {
            (Mode::Review, _) => "Start the review of the due concepts.".to_string(),
            (Mode::Learn, true) => format!("I want to learn: {}. Please begin.", topic.title),
            (Mode::Learn, false) => "Let's continue the course where we left off.".to_string(),
        };
        let session_id = self.db.create_session(topic_id, mode.kind(), now)?;
        let content = text_message(&opening);
        self.db
            .add_message(session_id, "user", &content, true, now)?;

        self.screen = Screen::Chat(Box::new(Chat {
            topic,
            session_id,
            mode,
            subskills,
            items: Vec::new(),
            streaming: String::new(),
            busy: false,
            error: None,
            input: Input::default(),
            scroll: 0,
            tokens: (0, 0),
            read_only: false,
            system,
            messages: vec![json!({"role": "user", "content": content})],
            turn_id: 0,
        }));
        self.request();
        Ok(())
    }

    /// Sends the conversation so far to the API.
    fn request(&mut self) {
        let Screen::Chat(chat) = &mut self.screen else {
            return;
        };
        let Some(provider) = self.provider.clone() else {
            chat.error = Some("No model is set up. Press Esc, then s to choose one.".into());
            return;
        };
        self.next_turn += 1;
        chat.turn_id = self.next_turn;
        chat.busy = true;
        chat.error = None;
        chat.streaming.clear();
        api::spawn(
            api::Request {
                provider,
                system: chat.system.clone(),
                tools: tutor::tools(chat.mode),
                messages: chat.messages.clone(),
            },
            chat.turn_id,
            self.tx.clone(),
        );
    }

    fn say(&mut self, text: &str) -> Result<()> {
        let Screen::Chat(chat) = &mut self.screen else {
            return Ok(());
        };
        let content = text_message(text);
        self.db
            .add_message(chat.session_id, "user", &content, false, now())?;
        match chat.messages.last_mut() {
            // After a failed request the last message is still the learner's,
            // and the API wants roles to alternate, so extend it.
            Some(last) if last["role"] == "user" => {
                if let Some(blocks) = last["content"].as_array_mut() {
                    blocks.push(content[0].clone());
                }
            }
            _ => chat
                .messages
                .push(json!({"role": "user", "content": content})),
        }
        chat.items.push(Item::User(text.to_string()));
        chat.scroll = 0;
        self.request();
        Ok(())
    }

    fn leave_chat(&mut self) -> Result<()> {
        let Screen::Chat(chat) = &self.screen else {
            return Ok(());
        };
        if chat.read_only {
            self.screen = Screen::History;
            return Ok(());
        }
        // A session the learner never answered in is not worth keeping.
        if !chat.items.iter().any(|i| matches!(i, Item::User(_))) {
            self.db.delete_session(chat.session_id)?;
        }
        self.screen = Screen::Home;
        self.refresh_home()
    }

    fn chat_key(&mut self, key: KeyEvent) -> Result<()> {
        let Screen::Chat(chat) = &mut self.screen else {
            return Ok(());
        };
        let ctrl = key.modifiers.contains(KeyModifiers::CONTROL);
        let alt = key.modifiers.contains(KeyModifiers::ALT);
        match key.code {
            KeyCode::Esc => return self.leave_chat(),
            KeyCode::PageUp => chat.scroll = chat.scroll.saturating_add(10),
            KeyCode::PageDown => chat.scroll = chat.scroll.saturating_sub(10),
            KeyCode::Up => chat.scroll = chat.scroll.saturating_add(1),
            KeyCode::Down => chat.scroll = chat.scroll.saturating_sub(1),
            _ if chat.read_only => {
                if key.code == KeyCode::Char('q') {
                    return self.leave_chat();
                }
            }
            KeyCode::Char('r') if ctrl => {
                if chat.error.is_some() {
                    self.request();
                }
            }
            KeyCode::Char('f') if ctrl => {
                if !chat.busy {
                    return self.say("Make flashcards for what we have covered so far.");
                }
            }
            KeyCode::Char('j') if ctrl => chat.input.insert('\n'),
            KeyCode::Enter if alt => chat.input.insert('\n'),
            KeyCode::Enter => {
                if !chat.busy && !chat.input.is_blank() {
                    let text = chat.input.text().trim().to_string();
                    chat.input.clear();
                    return self.say(&text);
                }
            }
            _ => edit_key(&mut chat.input, key),
        }
        Ok(())
    }

    /// Handles everything the API worker threads have sent since the last call.
    pub fn poll_stream(&mut self) -> Result<()> {
        while let Ok((turn_id, event)) = self.rx.try_recv() {
            let Screen::Chat(chat) = &mut self.screen else {
                continue;
            };
            if chat.turn_id != turn_id || chat.read_only {
                continue;
            }
            match event {
                StreamEvent::Delta(text) => chat.streaming.push_str(&text),
                StreamEvent::Done(Err(message)) => {
                    chat.busy = false;
                    chat.streaming.clear();
                    chat.error = Some(format!("{message} Press Ctrl+R to retry."));
                }
                StreamEvent::Done(Ok(reply)) => self.on_reply(reply)?,
            }
        }
        Ok(())
    }

    fn on_reply(&mut self, reply: Reply) -> Result<()> {
        let Screen::Chat(chat) = &mut self.screen else {
            return Ok(());
        };
        let now = now();
        chat.streaming.clear();
        chat.tokens.0 += reply.input_tokens;
        chat.tokens.1 += reply.output_tokens;
        self.db
            .add_usage(chat.session_id, reply.input_tokens, reply.output_tokens)?;

        let content = Value::Array(reply.content);
        self.db
            .add_message(chat.session_id, "assistant", &content, false, now)?;
        chat.items.extend(items_from("assistant", &content, false));
        chat.messages
            .push(json!({"role": "assistant", "content": content}));

        if reply.stop_reason != "tool_use" {
            chat.busy = false;
            match reply.stop_reason.as_str() {
                "max_tokens" => chat.items.push(Item::Notice(
                    "The reply hit the length limit and was cut off.".into(),
                )),
                "refusal" => chat
                    .items
                    .push(Item::Notice("The model declined to answer.".into())),
                _ => {}
            }
            return Ok(());
        }

        let mut results = Vec::new();
        for block in content.as_array().into_iter().flatten() {
            if block["type"] != "tool_use" {
                continue;
            }
            let name = block["name"].as_str().unwrap_or("");
            let outcome = tutor::execute(&self.db, chat.topic.id, name, &block["input"], now)?;
            results.push(json!({
                "type": "tool_result",
                "tool_use_id": block["id"],
                "is_error": outcome.is_err(),
                "content": outcome.unwrap_or_else(|e| e),
            }));
        }
        let results = Value::Array(results);
        self.db
            .add_message(chat.session_id, "user", &results, true, now)?;
        chat.messages
            .push(json!({"role": "user", "content": results}));
        chat.subskills = self.db.subskills(chat.topic.id)?;
        chat.topic = self.db.topic(chat.topic.id)?;
        // The header's due counts come from the home rows.
        self.refresh_home()?;
        self.request();
        Ok(())
    }
}

fn edit_key(input: &mut Input, key: KeyEvent) {
    match key.code {
        KeyCode::Char(c) if !key.modifiers.contains(KeyModifiers::CONTROL) => input.insert(c),
        KeyCode::Backspace => input.backspace(),
        KeyCode::Delete => input.delete(),
        KeyCode::Left => input.left(),
        KeyCode::Right => input.right(),
        KeyCode::Home => input.home(),
        KeyCode::End => input.end(),
        _ => {}
    }
}
