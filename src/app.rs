//! Application state and key handling.

use crate::api::{self, Reply, StreamEvent};
use crate::db::{Card, Db, LevelEvent, SessionRow, Subskill, Topic};
use crate::export;
use crate::input::Input;
use crate::plan::{self, Plan, Progress, Ranking};
use crate::provider::{Kind, Provider};
use crate::srs::Grade;
use crate::tutor::{self, Mode};
use anyhow::Result;
use ratatui::crossterm::event::{Event, KeyCode, KeyEvent, KeyEventKind, KeyModifiers};
use serde_json::{Value, json};
use std::collections::{HashMap, HashSet, VecDeque};
use std::path::PathBuf;
use std::sync::mpsc::{self, Receiver, Sender};
use std::time::SystemTime;

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
    /// Typing into the answer box (vim's insert mode); otherwise keys move
    /// around the transcript (normal mode).
    pub insert: bool,
    /// Rows of transcript on screen, set while drawing, for half-page moves.
    pub view: usize,
    system: String,
    messages: Vec<Value>,
    turn_id: u64,
}

impl Chat {
    /// The source of the latest ```dot diagram the tutor wrote.
    pub fn last_diagram(&self) -> Option<String> {
        self.items.iter().rev().find_map(|item| match item {
            Item::Tutor(text) => crate::diagram::last_dot(text),
            _ => None,
        })
    }
}

pub struct CardReview {
    pub queue: VecDeque<Card>,
    pub revealed: bool,
    pub done: usize,
}

pub enum PlanState {
    Missing(PathBuf),
    Broken(String),
    Loaded(Plan),
}

/// One selectable row of the plan screen.
#[derive(Clone, Debug, PartialEq)]
pub enum PlanRow {
    /// An index into `ranking.next`.
    Next(usize),
    /// An index into the plan's tracks.
    Track(usize),
    /// A followed track that is no longer in the plan.
    Gone(String),
    Done,
    /// An index into `ranking.done`.
    DoneSkill(usize),
}

pub struct PlanView {
    pub state: PlanState,
    pub ranking: Ranking,
    pub followed: HashSet<String>,
    /// Index into `rows()`.
    pub selected: usize,
    pub done_open: bool,
    mtime: Option<SystemTime>,
}

impl PlanView {
    pub fn tracks(&self) -> &[plan::Track] {
        match &self.state {
            PlanState::Loaded(plan) => &plan.tracks,
            _ => &[],
        }
    }

    /// Followed tracks that the plan no longer lists, sorted.
    pub fn gone(&self) -> Vec<String> {
        let mut gone: Vec<String> = self
            .followed
            .iter()
            .filter(|id| !self.tracks().iter().any(|t| &t.id == *id))
            .cloned()
            .collect();
        gone.sort();
        gone
    }

    pub fn rows(&self) -> Vec<PlanRow> {
        if !matches!(self.state, PlanState::Loaded(_)) {
            return Vec::new();
        }
        let mut rows: Vec<PlanRow> = (0..self.ranking.next.len()).map(PlanRow::Next).collect();
        rows.extend((0..self.tracks().len()).map(PlanRow::Track));
        rows.extend(self.gone().into_iter().map(PlanRow::Gone));
        if !self.ranking.done.is_empty() {
            rows.push(PlanRow::Done);
            if self.done_open {
                rows.extend((0..self.ranking.done.len()).map(PlanRow::DoneSkill));
            }
        }
        rows
    }
}

fn modified(path: &std::path::Path) -> Option<SystemTime> {
    std::fs::metadata(path).and_then(|m| m.modified()).ok()
}

pub enum Screen {
    Home,
    Chat(Box<Chat>),
    Cards(CardReview),
    History,
    Plan(PlanView),
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
    Model(ModelPicker),
    Settings(Settings),
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub enum SettingsRow {
    Model,
    Plan,
    PlanFile,
}

/// The settings page: the model, and the learning plan switch and file.
pub struct Settings {
    pub row: SettingsRow,
    /// The plan file field while it is being edited.
    pub editing: Option<Input>,
    /// The plan file in use, with the home folder as `~`.
    pub plan_file: String,
}

/// Picks the model from the provider's list, filtered by what is typed.
pub struct ModelPicker {
    /// The provider's name, for the title.
    pub provider: &'static str,
    pub input: Input,
    pub models: Vec<String>,
    pub current: String,
    /// Index into `shown()`.
    pub selected: usize,
    pub loading: bool,
    pub note: Option<String>,
    id: u64,
}

impl ModelPicker {
    pub fn shown(&self) -> Vec<&str> {
        let filter = self.input.text().trim().to_lowercase();
        self.models
            .iter()
            .map(String::as_str)
            .filter(|m| m.to_lowercase().contains(&filter))
            .collect()
    }

    /// The highlighted model, or what was typed when nothing matches it.
    fn choice(&self) -> Option<String> {
        match self.shown().get(self.selected) {
            Some(m) => Some(m.to_string()),
            None => Some(self.input.text().trim().to_string()).filter(|t| !t.is_empty()),
        }
    }

    fn loaded(&mut self, result: Result<Vec<String>, String>) {
        self.loading = false;
        match result {
            Ok(models) if !models.is_empty() => {
                self.models = models;
                if !self.models.contains(&self.current) {
                    self.models.insert(0, self.current.clone());
                }
            }
            Ok(_) => self.note = Some("The provider listed no models. Type a name instead.".into()),
            Err(e) => self.note = Some(format!("Could not fetch the list ({e}).")),
        }
        if self.input.is_blank() {
            self.selected = self
                .models
                .iter()
                .position(|m| *m == self.current)
                .unwrap_or(0);
        }
    }
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
    /// Opened from settings, so closing it goes back there.
    from_settings: bool,
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
    /// The learning plan is on in settings. Off, the plan file is never read.
    pub plan_enabled: bool,
    /// The first key of a two-key command such as `gg` or `dd`.
    pending: Option<char>,
    data_dir: PathBuf,
    models_tx: Sender<(u64, Result<Vec<String>, String>)>,
    models_rx: Receiver<(u64, Result<Vec<String>, String>)>,
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
        let (models_tx, models_rx) = mpsc::channel();
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
            plan_enabled: plan::plan_enabled(&data_dir),
            pending: None,
            data_dir,
            models_tx,
            models_rx,
            tx,
            rx,
            next_turn: 0,
        };
        app.refresh_home()?;
        if app.provider.is_none() {
            app.open_setup(false);
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
                    Screen::Plan(_) => self.plan_key(key),
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
                    (Some(Modal::Model(picker)), _) => {
                        picker.input.insert_str(text.trim());
                        picker.selected = 0;
                    }
                    (
                        Some(Modal::Settings(Settings {
                            editing: Some(input),
                            ..
                        })),
                        _,
                    ) => input.insert_str(text.trim()),
                    (None, Screen::Chat(chat)) if !chat.read_only => {
                        chat.insert = true;
                        chat.input.insert_str(&text)
                    }
                    _ => {}
                }
                Ok(())
            }
            _ => Ok(()),
        }
    }

    fn open_setup(&mut self, from_settings: bool) {
        self.modal = Some(Modal::Setup(Setup {
            kind: self.provider.as_ref().map_or(Kind::Anthropic, |p| p.kind),
            step: SetupStep::Provider,
            input: Input::default(),
            has_saved_key: false,
            from_settings,
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
            (SetupStep::Provider, KeyCode::Esc) => {
                let back = setup.from_settings;
                self.close_setup(back);
            }
            (SetupStep::Provider, KeyCode::Left | KeyCode::Char('h')) => {
                let i = Kind::ALL.iter().position(|k| *k == setup.kind).unwrap_or(0);
                setup.kind = Kind::ALL[i.saturating_sub(1)];
            }
            (SetupStep::Provider, KeyCode::Right | KeyCode::Char('l') | KeyCode::Tab) => {
                let i = Kind::ALL.iter().position(|k| *k == setup.kind).unwrap_or(0);
                setup.kind = Kind::ALL[(i + 1).min(Kind::ALL.len() - 1)];
            }
            (SetupStep::Provider, KeyCode::Enter) => go(setup, first(setup.kind)),
            (SetupStep::Provider, KeyCode::Char(c))
                if c.to_digit(10)
                    .is_some_and(|n| (1..=Kind::ALL.len() as u32).contains(&n)) =>
            {
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
                let back = setup.from_settings;
                self.close_setup(back);
            }
            _ => edit_key(&mut setup.input, key),
        }
        Ok(())
    }

    fn close_setup(&mut self, from_settings: bool) {
        self.modal = None;
        if from_settings {
            self.open_settings();
        }
    }

    // ---- settings ----

    fn open_settings(&mut self) {
        self.modal = Some(Modal::Settings(Settings {
            row: SettingsRow::Model,
            editing: None,
            plan_file: short_path(&plan::plan_path(&self.data_dir)),
        }));
    }

    /// A failed save is shown in the status line and changes nothing.
    fn set_plan_enabled(&mut self, enabled: bool) {
        self.status = match plan::save_plan_enabled(&self.data_dir, enabled) {
            Err(e) => format!("Could not save settings: {e}"),
            Ok(()) => {
                self.plan_enabled = enabled;
                if enabled {
                    "The learning plan is on. Press p on the home screen.".into()
                } else {
                    "The learning plan is off.".into()
                }
            }
        };
    }

    fn settings_key(&mut self, key: KeyEvent) -> Result<()> {
        let Some(Modal::Settings(settings)) = &mut self.modal else {
            return Ok(());
        };
        if let Some(input) = &mut settings.editing {
            match key.code {
                KeyCode::Esc => settings.editing = None,
                KeyCode::Enter => {
                    if let Err(e) = plan::save_plan_path(&self.data_dir, &input.text()) {
                        self.status = format!("Could not save settings: {e}");
                    }
                    settings.editing = None;
                    settings.plan_file = short_path(&plan::plan_path(&self.data_dir));
                }
                _ => edit_key(input, key),
            }
            return Ok(());
        }
        use SettingsRow::*;
        // The plan file row exists only while the plan is on.
        let rows: &[SettingsRow] = if self.plan_enabled {
            &[Model, Plan, PlanFile]
        } else {
            &[Model, Plan]
        };
        let i = rows.iter().position(|r| *r == settings.row).unwrap_or(0);
        match (settings.row, key.code) {
            (_, KeyCode::Esc) => self.modal = None,
            (_, KeyCode::Down | KeyCode::Char('j')) => {
                settings.row = rows[(i + 1).min(rows.len() - 1)]
            }
            (_, KeyCode::Up | KeyCode::Char('k')) => settings.row = rows[i.saturating_sub(1)],
            (Plan, KeyCode::Left | KeyCode::Char('h')) => self.set_plan_enabled(false),
            (Plan, KeyCode::Right | KeyCode::Char('l')) => self.set_plan_enabled(true),
            (Plan, KeyCode::Enter) => self.set_plan_enabled(!self.plan_enabled),
            (Model, KeyCode::Enter) => self.open_setup(true),
            (PlanFile, KeyCode::Enter) => {
                let mut input = Input::default();
                input.insert_str(&plan::configured_plan_path(&self.data_dir).unwrap_or_default());
                settings.editing = Some(input);
            }
            _ => {}
        }
        Ok(())
    }

    fn open_model_picker(&mut self) {
        let Some(provider) = self.provider.clone() else {
            self.open_setup(false);
            return;
        };
        let mut models: Vec<String> = provider
            .kind
            .presets()
            .iter()
            .map(|m| m.to_string())
            .collect();
        if !models.contains(&provider.model) {
            models.insert(0, provider.model.clone());
        }
        self.next_turn += 1;
        let id = self.next_turn;
        let tx = self.models_tx.clone();
        let fetch = provider.clone();
        std::thread::spawn(move || {
            let _ = tx.send((id, api::list_models(&fetch)));
        });
        self.modal = Some(Modal::Model(ModelPicker {
            provider: provider.kind.label(),
            input: Input::default(),
            selected: models
                .iter()
                .position(|m| *m == provider.model)
                .unwrap_or(0),
            models,
            current: provider.model,
            loading: true,
            note: None,
            id,
        }));
    }

    fn model_key(&mut self, key: KeyEvent) -> Result<()> {
        let Some(Modal::Model(picker)) = &mut self.modal else {
            return Ok(());
        };
        let ctrl = key.modifiers.contains(KeyModifiers::CONTROL);
        let last = picker.shown().len().saturating_sub(1);
        match key.code {
            KeyCode::Esc => self.modal = None,
            KeyCode::Down => picker.selected = (picker.selected + 1).min(last),
            KeyCode::Char('n') if ctrl => picker.selected = (picker.selected + 1).min(last),
            KeyCode::Up => picker.selected = picker.selected.saturating_sub(1),
            KeyCode::Char('p') if ctrl => picker.selected = picker.selected.saturating_sub(1),
            KeyCode::Enter => {
                if let (Some(model), Some(provider)) = (picker.choice(), &mut self.provider) {
                    provider.model = model;
                    provider.save(&self.data_dir.join(CONFIG_FILE))?;
                    self.status = match &self.screen {
                        Screen::Chat(chat) if !chat.read_only => {
                            format!("The tutor uses {} from the next reply.", provider.model)
                        }
                        _ => format!("The tutor now uses {}.", provider.model),
                    };
                    self.modal = None;
                }
            }
            _ => {
                let before = picker.input.text();
                edit_key(&mut picker.input, key);
                if picker.input.text() != before {
                    picker.selected = 0;
                }
            }
        }
        Ok(())
    }

    fn modal_key(&mut self, key: KeyEvent) -> Result<()> {
        let Some(modal) = &mut self.modal else {
            return Ok(());
        };
        match modal {
            Modal::Setup(_) => return self.setup_key(key),
            Modal::Model(_) => return self.model_key(key),
            Modal::Settings(_) => return self.settings_key(key),
            Modal::NewTopic { input, target } => match key.code {
                KeyCode::Esc => self.modal = None,
                KeyCode::Tab => *target = if *target == 4 { 5 } else { 4 },
                KeyCode::Enter if !input.is_blank() => {
                    let id = self.db.create_topic(input.text().trim(), *target, now())?;
                    self.modal = None;
                    self.start_chat(id, Mode::Learn, None)?;
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
        let pending = self.pending.take();
        let topic = self
            .topics
            .get(self.selected)
            .map(|t| (t.topic.id, t.topic.title.clone()));
        let last = self.topics.len().saturating_sub(1);
        match key.code {
            KeyCode::Char('q') => self.quit = true,
            KeyCode::Down | KeyCode::Char('j') => self.selected = (self.selected + 1).min(last),
            KeyCode::Up | KeyCode::Char('k') => self.selected = self.selected.saturating_sub(1),
            KeyCode::Char('g') if pending == Some('g') => self.selected = 0,
            KeyCode::Char('g') => self.pending = Some('g'),
            KeyCode::Char('G') => self.selected = last,
            KeyCode::Char('n') => {
                self.modal = Some(Modal::NewTopic {
                    input: Input::default(),
                    target: 4,
                })
            }
            KeyCode::Char('p') if self.plan_enabled => self.open_plan()?,
            KeyCode::Enter | KeyCode::Char('l') => {
                if let Some((id, _)) = topic {
                    self.start_chat(id, Mode::Learn, None)?;
                }
            }
            KeyCode::Char('r') => {
                if let Some((id, _)) = topic {
                    if self.topics[self.selected].concepts_due > 0 {
                        self.start_chat(id, Mode::Review, None)?;
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
            KeyCode::Char('H') => {
                self.history = self.db.sessions()?;
                self.history_selected = 0;
                self.screen = Screen::History;
            }
            KeyCode::Char('e') => self.export()?,
            KeyCode::Char('s') => self.open_settings(),
            KeyCode::Char('m') => self.open_model_picker(),
            KeyCode::Char('d') if pending == Some('d') => {
                if let Some((id, title)) = topic {
                    self.modal = Some(Modal::Confirm {
                        message: format!("Delete \"{title}\" with its sessions and flashcards?"),
                        action: ConfirmAction::DeleteTopic(id),
                    });
                }
            }
            KeyCode::Char('d') if topic.is_some() => {
                self.pending = Some('d');
                self.status = "Press d again to delete this topic.".into();
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
        self.status = format!(
            "Exported {} cards for Anki (File > Import): {}",
            cards.len(),
            short_path(&path)
        );
        Ok(())
    }

    // ---- learning plan ----

    /// Reads the plan file, moves links from merged skill ids, and ranks it.
    fn load_plan(&self) -> Result<PlanView> {
        let path = plan::plan_path(&self.data_dir);
        let mtime = modified(&path);
        let state = match std::fs::read_to_string(&path) {
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => PlanState::Missing(path),
            Err(e) => PlanState::Broken(e.to_string()),
            Ok(text) => match plan::parse(&text) {
                Ok(plan) => PlanState::Loaded(plan),
                Err(e) => PlanState::Broken(e),
            },
        };
        if let PlanState::Loaded(plan) = &state {
            for skill in &plan.skills {
                for old in &skill.former_ids {
                    self.db.relink_plan_skill(old, &skill.id)?;
                }
            }
        }
        let mut view = PlanView {
            state,
            ranking: Ranking {
                next: Vec::new(),
                done: Vec::new(),
            },
            followed: HashSet::new(),
            selected: 0,
            done_open: false,
            mtime,
        };
        self.rank_plan(&mut view)?;
        Ok(view)
    }

    /// Ranks the plan against the levels of the linked topics.
    fn rank_plan(&self, view: &mut PlanView) -> Result<()> {
        view.followed = self.db.followed_tracks()?;
        if let PlanState::Loaded(plan) = &view.state {
            let mut progress = HashMap::new();
            for (skill, topic_id) in self.db.plan_links()? {
                let subskills = self.db.subskills(topic_id)?;
                // Without subskills the skill's start level still applies.
                if subskills.is_empty() {
                    continue;
                }
                let level =
                    subskills.iter().map(|s| s.level as f64).sum::<f64>() / subskills.len() as f64;
                let target = self.db.topic(topic_id)?.target_level;
                progress.insert(skill, Progress { level, target });
            }
            view.ranking = plan::rank(plan, &progress, &view.followed);
        }
        view.selected = view.selected.min(view.rows().len().saturating_sub(1));
        Ok(())
    }

    fn open_plan(&mut self) -> Result<()> {
        self.screen = Screen::Plan(self.load_plan()?);
        Ok(())
    }

    /// Reloads the open plan screen when the file's modification time changes.
    fn reload_plan_if_changed(&mut self) -> Result<()> {
        let Screen::Plan(view) = &self.screen else {
            return Ok(());
        };
        if modified(&plan::plan_path(&self.data_dir)) == view.mtime {
            return Ok(());
        }
        let mut fresh = self.load_plan()?;
        if let Screen::Plan(view) = &mut self.screen {
            fresh.done_open = view.done_open;
            fresh.selected = view.selected.min(fresh.rows().len().saturating_sub(1));
            *view = fresh;
        }
        Ok(())
    }

    fn plan_key(&mut self, key: KeyEvent) -> Result<()> {
        let pending = self.pending.take();
        let Screen::Plan(mut view) = std::mem::replace(&mut self.screen, Screen::Home) else {
            return Ok(());
        };
        let rows = view.rows();
        let last = rows.len().saturating_sub(1);
        match key.code {
            KeyCode::Esc | KeyCode::Char('q') | KeyCode::Char('h') | KeyCode::Tab => {
                return Ok(());
            }
            KeyCode::Down | KeyCode::Char('j') => view.selected = (view.selected + 1).min(last),
            KeyCode::Up | KeyCode::Char('k') => view.selected = view.selected.saturating_sub(1),
            KeyCode::Char('g') if pending == Some('g') => view.selected = 0,
            KeyCode::Char('g') => self.pending = Some('g'),
            KeyCode::Char('G') => view.selected = last,
            KeyCode::Enter | KeyCode::Char('l') => match rows.get(view.selected) {
                Some(PlanRow::Done) => view.done_open = !view.done_open,
                Some(PlanRow::Next(i)) => {
                    let id = view.ranking.next[*i].id.clone();
                    return self.start_skill(&view, &id);
                }
                Some(PlanRow::DoneSkill(i)) => {
                    let id = view.ranking.done[*i].id.clone();
                    return self.start_skill(&view, &id);
                }
                _ => {}
            },
            KeyCode::Char('b') => {
                let id = match rows.get(view.selected) {
                    Some(PlanRow::Track(i)) => Some(view.tracks()[*i].id.clone()),
                    Some(PlanRow::Gone(id)) => Some(id.clone()),
                    _ => None,
                };
                if let Some(id) = id {
                    if view.followed.contains(&id) {
                        self.db.unfollow_track(&id)?;
                    } else {
                        self.db.follow_track(&id, now())?;
                    }
                    self.rank_plan(&mut view)?;
                }
            }
            _ => {}
        }
        self.screen = Screen::Plan(view);
        Ok(())
    }

    /// Opens the topic linked to a plan skill, or creates and links one.
    fn start_skill(&mut self, view: &PlanView, skill_id: &str) -> Result<()> {
        let PlanState::Loaded(plan) = &view.state else {
            return Ok(());
        };
        let Some(skill) = plan.skills.iter().find(|s| s.id == skill_id) else {
            return Ok(());
        };
        let mut hint = String::new();
        if let Some(level) = skill.start_level {
            hint += &format!(
                "Starting level estimate: {level} of {}. ",
                plan::DEFAULT_TARGET
            );
        }
        if !skill.prerequisites.is_empty() {
            let names: Vec<&str> = skill
                .prerequisites
                .iter()
                .map(|id| plan.name_of(id))
                .collect();
            hint += &format!("It builds on: {}. ", names.join(", "));
        }
        let hint = Some(hint).filter(|h| !h.is_empty());
        if let Some(&topic_id) = self.db.plan_links()?.get(skill_id) {
            return self.start_chat(topic_id, Mode::Learn, hint);
        }
        let topic_id = self
            .db
            .create_topic(&skill.name, plan::DEFAULT_TARGET, now())?;
        self.db.link_plan_skill(skill_id, topic_id)?;
        self.refresh_home()?;
        self.start_chat(topic_id, Mode::Learn, hint)
    }

    fn history_key(&mut self, key: KeyEvent) -> Result<()> {
        let pending = self.pending.take();
        let last = self.history.len().saturating_sub(1);
        match key.code {
            KeyCode::Esc | KeyCode::Char('q') | KeyCode::Char('h') => self.screen = Screen::Home,
            KeyCode::Down | KeyCode::Char('j') => {
                self.history_selected = (self.history_selected + 1).min(last)
            }
            KeyCode::Up | KeyCode::Char('k') => {
                self.history_selected = self.history_selected.saturating_sub(1)
            }
            KeyCode::Char('g') if pending == Some('g') => self.history_selected = 0,
            KeyCode::Char('g') => self.pending = Some('g'),
            KeyCode::Char('G') => self.history_selected = last,
            KeyCode::Char('m') => self.open_model_picker(),
            KeyCode::Enter | KeyCode::Char('l') => {
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
                        insert: false,
                        view: 0,
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
        let pending = self.pending.take();
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
            KeyCode::Esc | KeyCode::Char('q') | KeyCode::Char('h') => {
                self.screen = Screen::Home;
                self.refresh_home()?;
            }
            KeyCode::Char(' ') | KeyCode::Enter => review.revealed = true,
            KeyCode::Char('m') => self.open_model_picker(),
            KeyCode::Char('d') if pending != Some('d') => {
                if !review.queue.is_empty() {
                    self.pending = Some('d');
                    self.status = "Press d again to delete this card.".into();
                }
            }
            KeyCode::Char('d') => {
                self.status.clear();
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

    /// `hint` goes into the opening message of a new topic, before "Please begin."
    fn start_chat(&mut self, topic_id: i64, mode: Mode, hint: Option<String>) -> Result<()> {
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
            (Mode::Learn, true) => format!(
                "I want to learn: {}. {}Please begin.",
                topic.title,
                hint.unwrap_or_default()
            ),
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
            insert: true,
            view: 0,
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
        let pending = self.pending.take();
        let Screen::Chat(chat) = &mut self.screen else {
            return Ok(());
        };
        let ctrl = key.modifiers.contains(KeyModifiers::CONTROL);
        let alt = key.modifiers.contains(KeyModifiers::ALT);
        let page = chat.view.max(2) - 1;
        let half = (chat.view / 2).max(1);

        // Keys that work in both modes.
        match key.code {
            KeyCode::PageUp => chat.scroll = chat.scroll.saturating_add(page),
            KeyCode::PageDown => chat.scroll = chat.scroll.saturating_sub(page),
            KeyCode::Up => chat.scroll = chat.scroll.saturating_add(1),
            KeyCode::Down => chat.scroll = chat.scroll.saturating_sub(1),
            KeyCode::Char('d') if ctrl => chat.scroll = chat.scroll.saturating_sub(half),
            KeyCode::Char('u') if ctrl => chat.scroll = chat.scroll.saturating_add(half),
            KeyCode::Char('r') if ctrl => {
                if chat.error.is_some() && !chat.read_only {
                    self.request();
                }
            }
            KeyCode::Char('f') if ctrl => {
                if !chat.busy && !chat.read_only {
                    return self.say("Make flashcards for what we have covered so far.");
                }
            }
            KeyCode::Char('o') if ctrl => {
                let path = self.data_dir.join("diagram.svg");
                self.status = match chat.last_diagram() {
                    None => "The tutor has not drawn a diagram in this session.".into(),
                    Some(dot) => match crate::diagram::open(&dot, &path) {
                        Ok(true) => format!("Opened the diagram: {}", short_path(&path)),
                        Ok(false) => format!(
                            "Saved the diagram; there is no display to open it on: {}",
                            short_path(&path)
                        ),
                        Err(e) => format!("Could not open the diagram: {e}"),
                    },
                };
            }
            _ if chat.insert && !chat.read_only => match key.code {
                KeyCode::Esc => chat.insert = false,
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
            },
            // Normal mode, and every past session.
            KeyCode::Esc | KeyCode::Char('q') | KeyCode::Char('h') => return self.leave_chat(),
            KeyCode::Char('j') => chat.scroll = chat.scroll.saturating_sub(1),
            KeyCode::Char('k') => chat.scroll = chat.scroll.saturating_add(1),
            KeyCode::Char('G') => chat.scroll = 0,
            // Drawing clamps this to the top.
            KeyCode::Char('g') if pending == Some('g') => chat.scroll = usize::MAX,
            KeyCode::Char('g') => self.pending = Some('g'),
            KeyCode::Char('m') => self.open_model_picker(),
            KeyCode::Char('i') | KeyCode::Enter if !chat.read_only => chat.insert = true,
            KeyCode::Char('a') if !chat.read_only => {
                chat.input.right();
                chat.insert = true;
            }
            KeyCode::Char('A') if !chat.read_only => {
                chat.input.end();
                chat.insert = true;
            }
            _ => {}
        }
        Ok(())
    }

    /// Handles everything the API worker threads have sent since the last call,
    /// and reloads the plan screen if its file changed.
    pub fn poll_stream(&mut self) -> Result<()> {
        self.reload_plan_if_changed()?;
        while let Ok((id, result)) = self.models_rx.try_recv() {
            if let Some(Modal::Model(picker)) = &mut self.modal
                && picker.id == id
            {
                picker.loaded(result);
            }
        }
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
    let ctrl = key.modifiers.contains(KeyModifiers::CONTROL);
    match key.code {
        KeyCode::Char('w') if ctrl => input.delete_word(),
        KeyCode::Char('u') if ctrl => input.delete_to_line_start(),
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

/// `path` with the home folder written as `~`.
pub fn short_path(path: &std::path::Path) -> String {
    let shown = path.display().to_string();
    match dirs::home_dir().map(|h| h.display().to_string()) {
        Some(home) if shown.starts_with(&home) => format!("~{}", &shown[home.len()..]),
        _ => shown,
    }
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;

    /// An app on an in-memory database, with `plan` (if any) as its plan file.
    /// The provider points at a closed port, so requests fail at once and offline,
    /// and is saved in config.json with the plan turned on or off.
    pub(crate) fn app_in(name: &str, plan: Option<&str>, plan_enabled: bool) -> App {
        let dir = std::env::temp_dir().join(format!("socratui-app-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        if let Some(plan) = plan {
            std::fs::write(dir.join("plan.json"), plan).unwrap();
        }
        let provider = Provider {
            kind: Kind::Local,
            base_url: "http://127.0.0.1:9".into(),
            key: String::new(),
            model: "test".into(),
        };
        provider.save(&dir.join(CONFIG_FILE)).unwrap();
        if plan_enabled {
            plan::save_plan_enabled(&dir, true).unwrap();
        }
        App::new(Db::in_memory().unwrap(), Some(provider), dir).unwrap()
    }

    /// An app with the learning plan turned on.
    pub(crate) fn app_with_plan(name: &str, plan: Option<&str>) -> App {
        app_in(name, plan, true)
    }

    pub(crate) fn press(app: &mut App, code: KeyCode) {
        app.on_event(Event::Key(KeyEvent::from(code))).unwrap();
    }

    fn config(app: &App) -> Value {
        crate::provider::read_config(&app.data_dir.join(CONFIG_FILE)).unwrap()
    }

    fn settings(app: &App) -> &Settings {
        match &app.modal {
            Some(Modal::Settings(settings)) => settings,
            _ => panic!("settings are not open"),
        }
    }

    #[test]
    fn plan_is_off_by_default() {
        let mut app = app_in("plan-off", Some("{"), false);
        assert!(!app.plan_enabled);
        press(&mut app, KeyCode::Char('p'));
        assert!(matches!(app.screen, Screen::Home));
        app.poll_stream().unwrap();
        assert!(matches!(app.screen, Screen::Home));
        assert!(app.status.is_empty(), "{}", app.status);
    }

    #[test]
    fn settings_toggle_turns_plan_on_and_saves() {
        let mut app = app_in("toggle-on", Some(ONE_SKILL), false);
        press(&mut app, KeyCode::Char('s'));
        assert_eq!(settings(&app).row, SettingsRow::Model);
        press(&mut app, KeyCode::Char('j'));
        assert_eq!(settings(&app).row, SettingsRow::Plan);
        press(&mut app, KeyCode::Char('l'));
        let v = config(&app);
        assert_eq!(v["plan_enabled"], true);
        assert_eq!(v["provider"], "local");
        assert_eq!(v["base_url"], "http://127.0.0.1:9");
        assert_eq!(v["model"], "test");
        assert!(app.plan_enabled);
        press(&mut app, KeyCode::Esc);
        assert!(app.modal.is_none());
        press(&mut app, KeyCode::Char('p'));
        assert!(matches!(plan_view(&app).state, PlanState::Loaded(_)));
    }

    #[test]
    fn settings_toggle_off_hides_plan_again() {
        let mut app = app_with_plan("toggle-off", Some(ONE_SKILL));
        press(&mut app, KeyCode::Char('s'));
        press(&mut app, KeyCode::Down);
        press(&mut app, KeyCode::Left);
        assert_eq!(config(&app)["plan_enabled"], false);
        assert!(!app.plan_enabled);
        press(&mut app, KeyCode::Esc);
        press(&mut app, KeyCode::Char('p'));
        assert!(matches!(app.screen, Screen::Home));
    }

    #[test]
    fn plan_file_row_shows_only_while_plan_is_on() {
        let mut app = app_in("file-row", None, false);
        press(&mut app, KeyCode::Char('s'));
        press(&mut app, KeyCode::Char('j'));
        press(&mut app, KeyCode::Char('j'));
        assert_eq!(settings(&app).row, SettingsRow::Plan);
        press(&mut app, KeyCode::Char('l'));
        press(&mut app, KeyCode::Char('j'));
        assert_eq!(settings(&app).row, SettingsRow::PlanFile);
        // Turning the plan off from here would leave the cursor on a hidden row.
        press(&mut app, KeyCode::Char('k'));
        press(&mut app, KeyCode::Char('h'));
        press(&mut app, KeyCode::Char('j'));
        assert_eq!(settings(&app).row, SettingsRow::Plan);
    }

    #[test]
    fn plan_file_edit_saves_path() {
        let mut app = app_with_plan("plan-file", None);
        press(&mut app, KeyCode::Char('s'));
        press(&mut app, KeyCode::Char('j'));
        press(&mut app, KeyCode::Char('j'));
        assert!(settings(&app).plan_file.ends_with("plan.json"));
        press(&mut app, KeyCode::Enter);
        assert!(settings(&app).editing.is_some());
        // j and k are typed while editing, not moves.
        for c in "/tmp/jk.json".chars() {
            press(&mut app, KeyCode::Char(c));
        }
        press(&mut app, KeyCode::Enter);
        assert!(settings(&app).editing.is_none());
        assert_eq!(config(&app)["plan_path"], "/tmp/jk.json");
        assert_eq!(config(&app)["provider"], "local");
        assert_eq!(settings(&app).plan_file, "/tmp/jk.json");

        // The field starts with the saved value; Ctrl+U clears it.
        press(&mut app, KeyCode::Enter);
        let clear = KeyEvent::new(KeyCode::Char('u'), KeyModifiers::CONTROL);
        app.on_event(Event::Key(clear)).unwrap();
        press(&mut app, KeyCode::Enter);
        assert!(config(&app).get("plan_path").is_none());
        assert!(settings(&app).plan_file.ends_with("plan.json"));

        // Esc while editing cancels the edit and keeps settings open.
        press(&mut app, KeyCode::Enter);
        press(&mut app, KeyCode::Char('x'));
        press(&mut app, KeyCode::Esc);
        assert!(settings(&app).editing.is_none());
        assert!(config(&app).get("plan_path").is_none());
    }

    #[cfg(unix)]
    #[test]
    fn failed_settings_save_keeps_app_open_and_setting_unchanged() {
        use std::os::unix::fs::PermissionsExt;
        let mut app = app_in("save-fails", None, false);
        let path = app.data_dir.join(CONFIG_FILE);
        let before = std::fs::read_to_string(&path).unwrap();
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o400)).unwrap();
        press(&mut app, KeyCode::Char('s'));
        press(&mut app, KeyCode::Char('j'));
        press(&mut app, KeyCode::Char('l'));
        assert!(!app.quit);
        assert!(!app.plan_enabled);
        assert!(
            app.status.starts_with("Could not save settings:"),
            "{}",
            app.status
        );
        assert_eq!(std::fs::read_to_string(&path).unwrap(), before);
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o600)).unwrap();
    }

    #[test]
    fn unreadable_config_is_not_overwritten() {
        let mut app = app_with_plan("config-broken", None);
        let path = app.data_dir.join(CONFIG_FILE);
        std::fs::write(&path, "{ broken").unwrap();
        press(&mut app, KeyCode::Char('s'));
        press(&mut app, KeyCode::Char('j'));
        press(&mut app, KeyCode::Char('h'));
        assert!(
            app.status
                .contains("config.json could not be read, so it was not changed"),
            "{}",
            app.status
        );
        press(&mut app, KeyCode::Char('j'));
        press(&mut app, KeyCode::Enter);
        press(&mut app, KeyCode::Char('x'));
        press(&mut app, KeyCode::Enter);
        assert!(app.status.contains("could not be read"), "{}", app.status);
        assert!(!app.quit);
        assert_eq!(std::fs::read_to_string(&path).unwrap(), "{ broken");
        let provider = app.provider.clone().unwrap();
        assert!(provider.save(&path).is_err());
        assert_eq!(std::fs::read_to_string(&path).unwrap(), "{ broken");
    }

    #[test]
    fn model_row_opens_provider_wizard() {
        let mut app = app_in("model-row", None, false);
        press(&mut app, KeyCode::Char('s'));
        press(&mut app, KeyCode::Enter);
        assert!(matches!(app.modal, Some(Modal::Setup(_))));
        press(&mut app, KeyCode::Esc);
        assert_eq!(settings(&app).row, SettingsRow::Model);

        // Finishing the wizard also returns to settings.
        press(&mut app, KeyCode::Enter);
        press(&mut app, KeyCode::Char('5'));
        press(&mut app, KeyCode::Enter);
        press(&mut app, KeyCode::Enter);
        assert_eq!(settings(&app).row, SettingsRow::Model);
        assert_eq!(config(&app)["provider"], "local");
        press(&mut app, KeyCode::Esc);
        assert!(app.modal.is_none());
    }

    #[test]
    fn no_provider_still_opens_wizard_directly() {
        let dir = std::env::temp_dir().join(format!("socratui-app-none-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let mut app = App::new(Db::in_memory().unwrap(), None, dir).unwrap();
        assert!(matches!(app.modal, Some(Modal::Setup(_))));
        press(&mut app, KeyCode::Esc);
        assert!(app.modal.is_none());
    }

    const ONE_SKILL: &str = r#"{"format":"socratui-plan/1","skills":[
        {"id":"kubernetes","name":"Kubernetes","weight":0.9,"start_level":1,"prerequisites":["docker"]},
        {"id":"docker","name":"Docker","weight":0.2,"start_level":3}]}"#;

    fn plan_view(app: &App) -> &PlanView {
        match &app.screen {
            Screen::Plan(view) => view,
            _ => panic!("not on the plan screen"),
        }
    }

    fn opening(app: &App) -> String {
        match &app.screen {
            Screen::Chat(chat) => chat.messages[0]["content"][0]["text"]
                .as_str()
                .unwrap()
                .to_string(),
            _ => panic!("not in a chat"),
        }
    }

    #[test]
    fn missing_plan_shows_empty_state() {
        let mut app = app_with_plan("missing", None);
        press(&mut app, KeyCode::Char('p'));
        match &plan_view(&app).state {
            PlanState::Missing(path) => assert!(path.ends_with("plan.json")),
            _ => panic!("expected the empty state"),
        }
    }

    #[test]
    fn broken_plan_shows_one_error() {
        let mut app = app_with_plan("broken", Some("{"));
        app.db.create_topic("Rust", 4, 0).unwrap();
        app.refresh_home().unwrap();
        press(&mut app, KeyCode::Char('p'));
        assert!(matches!(plan_view(&app).state, PlanState::Broken(_)));
        press(&mut app, KeyCode::Char('q'));
        assert!(matches!(app.screen, Screen::Home));
        assert!(!app.quit);
        assert_eq!(app.topics.len(), 1);
    }

    #[test]
    fn enter_creates_and_links_topic() {
        let mut app = app_with_plan("enter", Some(ONE_SKILL));
        press(&mut app, KeyCode::Char('p'));
        press(&mut app, KeyCode::Enter);
        let topics = app.db.topics().unwrap();
        assert_eq!(topics.len(), 1);
        assert_eq!(topics[0].title, "Kubernetes");
        assert_eq!(topics[0].target_level, 4);
        assert_eq!(app.db.plan_links().unwrap()["kubernetes"], topics[0].id);
        let text = opening(&app);
        assert!(text.contains("I want to learn: Kubernetes."), "{text}");
        assert!(text.contains("Starting level estimate: 1 of 4."), "{text}");
        assert!(text.contains("It builds on: Docker."), "{text}");
        assert!(text.ends_with("Please begin."), "{text}");
    }

    #[test]
    fn enter_reopens_linked_topic() {
        let mut app = app_with_plan("reopen", Some(ONE_SKILL));
        press(&mut app, KeyCode::Char('p'));
        press(&mut app, KeyCode::Enter);
        press(&mut app, KeyCode::Esc);
        press(&mut app, KeyCode::Char('q'));
        assert!(matches!(app.screen, Screen::Home));
        press(&mut app, KeyCode::Char('p'));
        press(&mut app, KeyCode::Enter);
        assert!(matches!(app.screen, Screen::Chat(_)));
        assert_eq!(app.db.topics().unwrap().len(), 1);
    }

    #[test]
    fn ctrl_u_and_d_scroll_while_typing() {
        let mut app = app_with_plan("scroll", Some(ONE_SKILL));
        press(&mut app, KeyCode::Char('p'));
        press(&mut app, KeyCode::Enter);
        press(&mut app, KeyCode::Char('x'));
        let ctrl = |c| Event::Key(KeyEvent::new(KeyCode::Char(c), KeyModifiers::CONTROL));
        let chat = |app: &App| match &app.screen {
            Screen::Chat(chat) => (chat.insert, chat.scroll, chat.input.text()),
            _ => panic!("not in a chat"),
        };
        app.on_event(ctrl('u')).unwrap();
        assert_eq!(chat(&app), (true, 1, "x".to_string()));
        app.on_event(ctrl('d')).unwrap();
        assert_eq!(chat(&app), (true, 0, "x".to_string()));
    }

    #[test]
    fn former_id_relinks_on_load() {
        let plan = r#"{"format":"socratui-plan/1","skills":[
            {"id":"kubernetes","name":"Kubernetes","weight":0.9,"former_ids":["k8s"]}]}"#;
        let mut app = app_with_plan("former", Some(plan));
        let topic = app.db.create_topic("K8s", 4, 0).unwrap();
        app.db.link_plan_skill("k8s", topic).unwrap();
        press(&mut app, KeyCode::Char('p'));
        let links = app.db.plan_links().unwrap();
        assert_eq!(links.get("kubernetes"), Some(&topic));
        assert!(!links.contains_key("k8s"));
        press(&mut app, KeyCode::Enter);
        match &app.screen {
            Screen::Chat(chat) => assert_eq!(chat.topic.id, topic),
            _ => panic!("not in a chat"),
        }
        assert_eq!(app.db.topics().unwrap().len(), 1);
    }

    #[test]
    fn b_follows_a_track() {
        let plan = r#"{"format":"socratui-plan/1","skills":[{"id":"ray","name":"Ray","weight":0.5}],
            "tracks":[{"id":"ml","name":"ML platform","skills":["ray"]}]}"#;
        let mut app = app_with_plan("follow", Some(plan));
        app.db.follow_track("gone", 0).unwrap();
        press(&mut app, KeyCode::Char('p'));
        // Rows: Ray, the track, then the followed track that left the plan.
        press(&mut app, KeyCode::Char('j'));
        press(&mut app, KeyCode::Char('b'));
        assert!(app.db.followed_tracks().unwrap().contains("ml"));
        press(&mut app, KeyCode::Char('b'));
        assert!(!app.db.followed_tracks().unwrap().contains("ml"));
        press(&mut app, KeyCode::Char('G'));
        press(&mut app, KeyCode::Char('b'));
        assert!(app.db.followed_tracks().unwrap().is_empty());
    }

    #[test]
    fn reloads_when_the_file_changes() {
        let mut app = app_with_plan("reload", Some(ONE_SKILL));
        press(&mut app, KeyCode::Char('p'));
        press(&mut app, KeyCode::Char('G'));
        assert_eq!(plan_view(&app).selected, 1);
        let path = crate::plan::plan_path(&app.data_dir);
        let newer =
            r#"{"format":"socratui-plan/1","skills":[{"id":"rust","name":"Rust","weight":0.5}]}"#;
        std::fs::write(&path, newer).unwrap();
        let later = std::time::SystemTime::now() + std::time::Duration::from_secs(60);
        std::fs::File::options()
            .write(true)
            .open(&path)
            .unwrap()
            .set_modified(later)
            .unwrap();
        app.poll_stream().unwrap();
        let view = plan_view(&app);
        assert_eq!(view.ranking.next[0].name, "Rust");
        assert!(view.selected < view.rows().len());
    }

    #[test]
    fn retry_keeps_the_hint_while_the_topic_has_no_subskills() {
        let mut app = app_with_plan("retry-hint", Some(ONE_SKILL));
        press(&mut app, KeyCode::Char('p'));
        press(&mut app, KeyCode::Enter);
        press(&mut app, KeyCode::Esc);
        press(&mut app, KeyCode::Char('q'));
        press(&mut app, KeyCode::Char('p'));
        press(&mut app, KeyCode::Enter);
        let text = opening(&app);
        assert!(text.contains("Starting level estimate: 1 of 4."), "{text}");
        assert!(text.contains("It builds on: Docker."), "{text}");
    }
}
