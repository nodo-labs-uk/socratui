//! Drawing. Reads the app state; the only thing it writes back is the clamped
//! transcript scroll position.

use crate::app::{self, App, CardReview, Chat, Item, Modal, Screen, SetupStep, TopicRow};
use crate::db::Subskill;
use crate::provider::Kind;
use crate::srs::{self, Grade};
use crate::tutor::{LEVEL_NAMES, Mode};
use chrono::{DateTime, Local};
use ratatui::Frame;
use ratatui::layout::{Constraint, Layout, Rect};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Clear, List, ListItem, ListState, Paragraph, Wrap};
use unicode_width::UnicodeWidthStr;

const ACCENT: Color = Color::Cyan;
const SIDEBAR_WIDTH: u16 = 46;
const MAX_INPUT_ROWS: usize = 6;

fn dim() -> Style {
    Style::new().fg(Color::DarkGray)
}

fn bold() -> Style {
    Style::new().add_modifier(Modifier::BOLD)
}

/// The selected row of a list: a grey bar that keeps each cell's colour.
fn selected() -> Style {
    Style::new()
        .bg(Color::Indexed(238))
        .add_modifier(Modifier::BOLD)
}

/// Pads or cuts `text` to exactly `width` columns.
fn fit(text: &str, width: usize) -> String {
    if text.width() <= width {
        return format!("{text}{}", " ".repeat(width - text.width()));
    }
    let mut out = String::new();
    for c in text.chars() {
        if out.width() + 1 >= width {
            break;
        }
        out.push(c);
    }
    out.push('…');
    format!("{out}{}", " ".repeat(width.saturating_sub(out.width())))
}

fn bar(filled: usize, width: usize) -> String {
    let filled = filled.min(width);
    format!("{}{}", "█".repeat(filled), "░".repeat(width - filled))
}

fn date(ts: i64) -> String {
    DateTime::from_timestamp(ts, 0)
        .map(|d| d.with_timezone(&Local).format("%d %b %H:%M").to_string())
        .unwrap_or_default()
}

fn interval(days: f64) -> String {
    if days <= 0.0 {
        "10m".to_string()
    } else {
        format!("{}d", days.round().max(1.0) as i64)
    }
}

fn hints(pairs: &[(&str, &str)]) -> Line<'static> {
    // Non-breaking spaces keep each hint on one line when the footer wraps.
    let keep = |s: &str| s.replace(' ', "\u{a0}");
    let mut spans = Vec::new();
    for (key, what) in pairs {
        spans.push(Span::styled(keep(key), bold().fg(ACCENT)));
        spans.push(Span::styled(format!("\u{a0}{}   ", keep(what)), dim()));
    }
    Line::from(spans)
}

fn wrap(text: &str, width: usize) -> Vec<String> {
    let mut out = Vec::new();
    for line in text.lines() {
        if line.trim().is_empty() {
            out.push(String::new());
        } else {
            out.extend(
                textwrap::wrap(line, width.max(8))
                    .into_iter()
                    .map(|l| l.into_owned()),
            );
        }
    }
    out
}

fn key_hints(app: &App) -> Line<'static> {
    match &app.screen {
        Screen::Home => hints(&[
            ("Enter", "learn"),
            ("n", "new topic"),
            ("r", "review concepts"),
            ("f", "flashcards"),
            ("H", "history"),
            ("m", "model"),
            ("e", "export to Anki"),
            ("s", "settings"),
            ("dd", "delete"),
            ("q", "quit"),
        ]),
        Screen::Chat(chat) if chat.read_only => hints(&[
            ("j k", "scroll"),
            ("Ctrl+D Ctrl+U", "half page"),
            ("gg G", "top, end"),
            ("q", "back"),
        ]),
        Screen::Chat(chat) => {
            let mut keys = if chat.insert {
                vec![
                    ("Enter", "send"),
                    ("Alt+Enter", "new line"),
                    ("Ctrl+F", "make flashcards"),
                    ("Esc", "normal mode"),
                ]
            } else {
                vec![
                    ("i", "type"),
                    ("j k", "scroll"),
                    ("Ctrl+D Ctrl+U", "half page"),
                    ("gg G", "top, end"),
                    ("Ctrl+F", "make flashcards"),
                    ("m", "model"),
                    ("q", "back"),
                ]
            };
            if chat.error.is_some() {
                keys.insert(0, ("Ctrl+R", "retry"));
            }
            hints(&keys)
        }
        Screen::Cards(_) => hints(&[
            ("Space", "show answer"),
            ("1", "again"),
            ("2", "hard"),
            ("3", "good"),
            ("4", "easy"),
            ("dd", "delete card"),
            ("q", "back"),
        ]),
        Screen::History => hints(&[("j k", "move"), ("Enter", "open"), ("q", "back")]),
    }
}

pub fn draw(f: &mut Frame, app: &mut App) {
    let keys = key_hints(app);
    // The key hints wrap on narrow terminals so that none is cut off.
    let text: String = keys.spans.iter().map(|s| s.content.as_ref()).collect();
    let footer_height = wrap(text.trim_end(), f.area().width as usize).len().max(1) as u16;
    let [header, body, status, footer] = Layout::vertical([
        Constraint::Length(1),
        Constraint::Min(3),
        Constraint::Length(1),
        Constraint::Length(footer_height),
    ])
    .areas(f.area());

    let due = format!(
        "{} flashcards due   {} concepts due   {}",
        app.cards_due(),
        app.concepts_due(),
        app.provider
            .as_ref()
            .map_or("no model set", |p| p.model.as_str())
    );
    let gap = (header.width as usize).saturating_sub(9 + due.width());
    f.render_widget(
        Line::from(vec![
            Span::styled(" Socratik", bold().fg(ACCENT)),
            Span::raw(" ".repeat(gap)),
            Span::styled(due, dim()),
        ]),
        header,
    );

    match &mut app.screen {
        Screen::Home => home(f, body, app),
        Screen::Chat(chat) => draw_chat(f, body, chat),
        Screen::Cards(review) => cards(f, body, review),
        Screen::History => history(f, body, app),
    }
    f.render_widget(
        Line::styled(format!(" {}", app.status), Style::new().fg(Color::Yellow)),
        status,
    );
    f.render_widget(Paragraph::new(keys).wrap(Wrap { trim: true }), footer);

    if let Some(modal) = &app.modal {
        draw_modal(f, modal);
    }
}

// ---- home ----

fn skill_map(subskills: &[Subskill], target: i64, width: usize) -> Vec<Line<'static>> {
    let now = app::now();
    let name_width = width.saturating_sub(20).max(8);
    let pct = (app::progress(subskills, target) * 100.0).round() as usize;
    let mut lines = vec![
        Line::from(vec![
            Span::styled(format!("{} ", bar(pct / 10, 10)), Style::new().fg(ACCENT)),
            Span::raw(format!("{pct}% of the way to level {target}")),
        ]),
        Line::default(),
    ];
    for s in subskills {
        let colour = if s.level >= target {
            Color::Green
        } else {
            ACCENT
        };
        let mut spans = vec![
            Span::raw(fit(&s.name, name_width)),
            Span::styled(
                format!(" {}", bar(s.level as usize * 2, 10)),
                Style::new().fg(colour),
            ),
            Span::raw(format!(" {}/5", s.level)),
        ];
        if s.is_due(now) {
            spans.push(Span::styled(" due", Style::new().fg(Color::Yellow)));
        }
        lines.push(Line::from(spans));
    }
    lines
}

fn home(f: &mut Frame, area: Rect, app: &App) {
    if app.topics.is_empty() {
        let text = vec![
            Line::default(),
            Line::from("  Nothing here yet."),
            Line::default(),
            Line::from(vec![
                Span::raw("  Press "),
                Span::styled("n", bold().fg(ACCENT)),
                Span::raw(" and type what you want to learn. The tutor starts with a few"),
            ]),
            Line::from("  questions to find your level, then teaches by asking, not telling."),
        ];
        f.render_widget(
            Paragraph::new(text).block(Block::bordered().title(" Topics ")),
            area,
        );
        return;
    }

    let [left, right] = if area.width >= 100 {
        Layout::horizontal([Constraint::Percentage(55), Constraint::Percentage(45)]).areas(area)
    } else {
        Layout::vertical([Constraint::Percentage(45), Constraint::Percentage(55)]).areas(area)
    };

    let title_width = (left.width as usize).saturating_sub(2 + 2 + 37);
    let row = |t: &TopicRow| {
        let pct = (t.progress() * 100.0).round() as usize;
        let done = t.topic.status == "complete";
        Line::from(vec![
            Span::raw(fit(&t.topic.title, title_width)),
            Span::styled(
                format!(" {}", bar(pct / 10, 10)),
                Style::new().fg(if done { Color::Green } else { ACCENT }),
            ),
            Span::raw(format!(" {pct:>3}%")),
            Span::styled(
                format!(" {:>9}", format!("{} review", t.concepts_due)),
                if t.concepts_due > 0 {
                    Style::new().fg(Color::Yellow)
                } else {
                    dim()
                },
            ),
            Span::styled(
                format!(
                    " {:>10}",
                    format!("{}/{} cards", t.cards_due, t.cards_total)
                ),
                if t.cards_due > 0 {
                    Style::new().fg(Color::Yellow)
                } else {
                    dim()
                },
            ),
        ])
    };
    let list = List::new(app.topics.iter().map(|t| ListItem::new(row(t))))
        .block(Block::bordered().title(" Topics "))
        .highlight_style(selected())
        .highlight_symbol("▸ ");
    let mut state = ListState::default().with_selected(Some(app.selected));
    f.render_stateful_widget(list, left, &mut state);

    let Some(t) = app.topics.get(app.selected) else {
        return;
    };
    let width = right.width.saturating_sub(2) as usize;
    let mut lines = if t.subskills.is_empty() {
        vec![Line::styled(
            "The skill map appears after the first session.",
            dim(),
        )]
    } else {
        skill_map(&t.subskills, t.topic.target_level, width)
    };
    if !t.events.is_empty() {
        lines.push(Line::default());
        lines.push(Line::styled("Recent level changes", bold()));
        for e in &t.events {
            let arrow = if e.new_level > e.old_level {
                "▲"
            } else {
                "▼"
            };
            lines.push(Line::from(vec![
                Span::styled(format!("{} ", date(e.at)), dim()),
                Span::styled(
                    format!("{arrow} {}→{} ", e.old_level, e.new_level),
                    Style::new().fg(if e.new_level > e.old_level {
                        Color::Green
                    } else {
                        Color::Red
                    }),
                ),
                Span::raw(e.subskill.clone()),
                Span::styled(format!("  {}", e.reason), dim()),
            ]));
        }
    }
    if !t.topic.notes.is_empty() {
        lines.push(Line::default());
        lines.push(Line::styled("Tutor's notes", bold()));
        lines.extend(wrap(&t.topic.notes, width).into_iter().map(Line::from));
    }
    let title = format!(
        " {} ",
        fit(&t.topic.title, width.saturating_sub(4)).trim_end()
    );
    f.render_widget(
        Paragraph::new(lines).block(Block::bordered().title(title)),
        right,
    );
}

// ---- tutoring ----

fn transcript(chat: &Chat, width: usize) -> Vec<Line<'static>> {
    let mut lines: Vec<Line> = Vec::new();
    let speech = |lines: &mut Vec<Line<'static>>, who: &str, colour: Color, text: &str| {
        if lines.last().is_some_and(|l| l.width() > 0) {
            lines.push(Line::default());
        }
        lines.push(Line::styled(who.to_string(), bold().fg(colour)));
        let mut in_code = false;
        for row in wrap(text, width) {
            let fence = row.trim_start().starts_with("```");
            let style = if in_code || fence {
                Style::new().fg(Color::Yellow)
            } else {
                Style::new()
            };
            in_code ^= fence;
            lines.push(Line::styled(row, style));
        }
        lines.push(Line::default());
    };

    for item in &chat.items {
        match item {
            Item::User(text) => speech(&mut lines, "You", ACCENT, text),
            Item::Tutor(text) => speech(&mut lines, "Tutor", Color::Green, text),
            Item::Event(text) => {
                for (i, row) in wrap(text, width.saturating_sub(2)).into_iter().enumerate() {
                    let mark = if i == 0 { "◆ " } else { "  " };
                    lines.push(Line::styled(
                        format!("{mark}{row}"),
                        Style::new().fg(Color::Magenta),
                    ));
                }
            }
            Item::Notice(text) => {
                lines.push(Line::styled(text.clone(), Style::new().fg(Color::Yellow)));
                lines.push(Line::default());
            }
        }
    }
    if !chat.streaming.is_empty() {
        speech(&mut lines, "Tutor", Color::Green, &chat.streaming);
    } else if chat.busy {
        lines.push(Line::styled("Tutor", bold().fg(Color::Green)));
        lines.push(Line::styled("thinking...", dim()));
    }
    if let Some(error) = &chat.error {
        for row in wrap(error, width) {
            lines.push(Line::styled(row, Style::new().fg(Color::Red)));
        }
    }
    lines
}

fn draw_chat(f: &mut Frame, area: Rect, chat: &mut Chat) {
    let map_height = (chat.subskills.len() as u16 + 4).min(area.height / 2);
    let [main, side] = if area.width >= 100 {
        Layout::horizontal([Constraint::Min(40), Constraint::Length(SIDEBAR_WIDTH)]).areas(area)
    } else {
        let [side, main] =
            Layout::vertical([Constraint::Length(map_height), Constraint::Min(6)]).areas(area);
        [main, side]
    };

    let map_width = side.width.saturating_sub(2) as usize;
    let mut map = skill_map(&chat.subskills, chat.topic.target_level, map_width);
    if chat.subskills.is_empty() {
        map = vec![Line::styled(
            "The tutor builds this after its first questions.",
            dim(),
        )];
    } else if side.height as usize > chat.subskills.len() + 12 {
        map.push(Line::default());
        for (level, name) in LEVEL_NAMES.iter().enumerate() {
            map.push(Line::styled(format!("{level} {name}"), dim()));
        }
    }
    f.render_widget(
        Paragraph::new(map)
            .wrap(Wrap { trim: false })
            .block(Block::bordered().title(" Skill map ")),
        side,
    );

    let input_width = main.width.saturating_sub(2) as usize;
    let (rows, cursor) = chat.input.layout(input_width);
    let first_row = (cursor.1 + 1).saturating_sub(MAX_INPUT_ROWS);
    let input_height = if chat.read_only {
        0
    } else {
        rows.len().min(MAX_INPUT_ROWS) as u16 + 2
    };
    let [log, input] =
        Layout::vertical([Constraint::Min(3), Constraint::Length(input_height)]).areas(main);

    let lines = transcript(chat, log.width.saturating_sub(2) as usize);
    let view = log.height.saturating_sub(2) as usize;
    chat.view = view;
    let max_scroll = lines.len().saturating_sub(view);
    chat.scroll = chat.scroll.min(max_scroll);
    let top = max_scroll - chat.scroll;
    let visible: Vec<Line> = lines.into_iter().skip(top).take(view).collect();
    let kind = match (chat.read_only, chat.mode) {
        (true, _) => "past session",
        (false, Mode::Learn) => "learning",
        (false, Mode::Review) => "concept review",
    };
    let mut block = Block::bordered().title(format!(" {} · {kind} ", chat.topic.title));
    if chat.scroll > 0 {
        block = block.title_bottom(Line::styled(" ↓ more below ", dim()).right_aligned());
    }
    block = block.title_bottom(Line::styled(
        format!(" {} tokens in · {} out ", chat.tokens.0, chat.tokens.1),
        dim(),
    ));
    f.render_widget(Paragraph::new(visible).block(block), log);

    if chat.read_only {
        return;
    }
    let text_style = if chat.insert { Style::new() } else { dim() };
    let shown: Vec<Line> = rows
        .into_iter()
        .skip(first_row)
        .take(MAX_INPUT_ROWS)
        .map(|r| Line::styled(r, text_style))
        .collect();
    let title = if chat.busy {
        " Your answer (the tutor is replying) "
    } else {
        " Your answer "
    };
    let mut block = Block::bordered().title(title);
    if !chat.insert {
        block = block
            .border_style(dim())
            .title(Line::styled(" NORMAL · i to type ", bold().fg(Color::Yellow)).right_aligned());
    }
    f.render_widget(Paragraph::new(shown).block(block), input);
    if !chat.insert {
        return;
    }
    f.set_cursor_position((
        input.x + 1 + cursor.0 as u16,
        input.y + 1 + (cursor.1 - first_row) as u16,
    ));
}

// ---- flashcards ----

fn cards(f: &mut Frame, area: Rect, review: &CardReview) {
    let width = area.width.min(84);
    let [column] = Layout::horizontal([Constraint::Length(width)])
        .flex(ratatui::layout::Flex::Center)
        .areas(area);
    let inner = width.saturating_sub(4) as usize;

    let Some(card) = review.queue.front() else {
        let text = vec![
            Line::default(),
            Line::from(format!(" Review finished. {} cards done.", review.done)),
            Line::styled(" Press Esc to go back.", dim()),
        ];
        f.render_widget(
            Paragraph::new(text).block(Block::bordered().title(" Flashcards ")),
            column,
        );
        return;
    };

    let source = match &card.subskill {
        Some(subskill) => format!("{} · {subskill}", card.topic_title),
        None => card.topic_title.clone(),
    };
    let mut lines = vec![Line::styled(source, dim()), Line::default()];
    lines.extend(
        wrap(&card.front, inner)
            .into_iter()
            .map(|l| Line::styled(l, bold())),
    );
    lines.push(Line::default());
    if review.revealed {
        lines.push(Line::styled("─".repeat(inner), dim()));
        lines.push(Line::default());
        lines.extend(wrap(&card.back, inner).into_iter().map(Line::from));
        lines.push(Line::default());
        let mut buttons = Vec::new();
        for (key, grade, label) in [
            ("1", Grade::Again, "Again"),
            ("2", Grade::Hard, "Hard"),
            ("3", Grade::Good, "Good"),
            ("4", Grade::Easy, "Easy"),
        ] {
            let next = interval(srs::next(card.sched, grade).interval_days);
            buttons.push(Span::styled(
                format!(" {key} "),
                bold().fg(Color::Black).bg(ACCENT),
            ));
            buttons.push(Span::raw(format!(" {label} ")));
            buttons.push(Span::styled(format!("{next}    "), dim()));
        }
        lines.push(Line::from(buttons));
    } else {
        lines.push(Line::from(vec![
            Span::styled(" Space ", bold().fg(Color::Black).bg(ACCENT)),
            Span::raw(" Show answer"),
        ]));
    }
    let title = format!(
        " Flashcards · {} left · {} done ",
        review.queue.len(),
        review.done
    );
    f.render_widget(
        Paragraph::new(lines).block(
            Block::bordered()
                .title(title)
                .padding(ratatui::widgets::Padding::horizontal(1)),
        ),
        column,
    );
}

// ---- history ----

fn history(f: &mut Frame, area: Rect, app: &App) {
    let block = Block::bordered().title(" History ");
    if app.history.is_empty() {
        let text = Line::styled(
            " No sessions yet. Sessions appear here once you answer the tutor.",
            dim(),
        );
        f.render_widget(
            Paragraph::new(vec![Line::default(), text]).block(block),
            area,
        );
        return;
    }
    let title_width = (area.width as usize).saturating_sub(4 + 13 + 10 + 12 + 22);
    let items = app.history.iter().map(|s| {
        ListItem::new(Line::from(vec![
            Span::styled(format!("{:<13}", date(s.started_at)), dim()),
            Span::raw(fit(&s.topic_title, title_width)),
            Span::raw(format!(" {:<9}", s.kind)),
            Span::raw(format!(" {:>3} answers", s.turns)),
            Span::styled(
                format!(" {:>14} tokens", s.input_tokens + s.output_tokens),
                dim(),
            ),
        ]))
    });
    let list = List::new(items)
        .block(block)
        .highlight_style(selected())
        .highlight_symbol("▸ ");
    let mut state = ListState::default().with_selected(Some(app.history_selected));
    f.render_stateful_widget(list, area, &mut state);
}

// ---- modals ----

const PICKER_ROWS: usize = 8;

fn draw_modal(f: &mut Frame, modal: &Modal) {
    let screen = f.area();
    // The picker keeps one size, whatever the filter leaves in its list.
    let height = match modal {
        Modal::Model(_) => PICKER_ROWS as u16 + 7,
        _ => 8,
    };
    let (width, height) = (screen.width.min(64), height.min(screen.height));
    let area = Rect::new(
        screen.x + (screen.width - width) / 2,
        screen.y + (screen.height - height) / 2,
        width,
        height,
    );
    f.render_widget(Clear, area);
    let inner = width.saturating_sub(4) as usize;

    match modal {
        Modal::Model(picker) => {
            let shown = picker.shown();
            let typed = picker.input.text();
            // Keep the highlighted row on screen.
            let first = (picker.selected + 1).saturating_sub(PICKER_ROWS);
            let fetching = if picker.loading {
                " · fetching the list"
            } else {
                ""
            };
            let mut lines = vec![
                Line::styled(format!("Type to filter{fetching}"), dim()),
                Line::from(vec![
                    Span::styled("› ", bold().fg(ACCENT)),
                    Span::raw(typed.clone()),
                ]),
                Line::default(),
            ];
            for (i, model) in shown.iter().enumerate().skip(first).take(PICKER_ROWS) {
                // Padded to the full width, so the highlight spans the row.
                let tag = if *model == picker.current {
                    "current"
                } else {
                    ""
                };
                let line = Line::from(vec![
                    Span::raw(fit(model, inner.saturating_sub(8))),
                    Span::styled(format!("{tag:>8}"), dim()),
                ]);
                lines.push(if i == picker.selected {
                    line.style(selected())
                } else {
                    line
                });
            }
            if shown.is_empty() {
                let message = match typed.trim() {
                    "" => "No models to show. Type a name.".to_string(),
                    t => format!("No match. Enter uses \"{t}\" as typed."),
                };
                lines.push(Line::styled(message, dim()));
            }
            while lines.len() < PICKER_ROWS + 3 {
                lines.push(Line::default());
            }
            lines.push(Line::styled(picker.note.clone().unwrap_or_default(), dim()));
            lines.push(hints(&[
                ("Enter", "use"),
                ("Ctrl+N Ctrl+P", "move"),
                ("Esc", "close"),
            ]));
            let title = format!(" Model · {} ", picker.provider);
            f.render_widget(
                Paragraph::new(lines).block(
                    Block::bordered()
                        .title(title)
                        .padding(ratatui::widgets::Padding::horizontal(1)),
                ),
                area,
            );
            let (_, cursor) = picker.input.layout(inner);
            f.set_cursor_position((area.x + 4 + cursor.0 as u16, area.y + 2));
        }
        Modal::NewTopic { input, target } => {
            let (rows, cursor) = input.layout(inner);
            let row = cursor.1;
            let choice = |level: i64, label: &str| {
                let style = if *target == level {
                    bold().fg(Color::Black).bg(ACCENT)
                } else {
                    dim()
                };
                Span::styled(format!(" {label} "), style)
            };
            let lines = vec![
                Line::styled("What do you want to learn?", dim()),
                Line::from(rows[row].clone()),
                Line::default(),
                Line::from(vec![
                    Span::styled("Aim for  ", dim()),
                    choice(4, "4 independent"),
                    Span::raw(" "),
                    choice(5, "5 can teach it"),
                ]),
                Line::default(),
                hints(&[("Enter", "start"), ("Tab", "change aim"), ("Esc", "cancel")]),
            ];
            f.render_widget(
                Paragraph::new(lines).block(
                    Block::bordered()
                        .title(" New topic ")
                        .padding(ratatui::widgets::Padding::horizontal(1)),
                ),
                area,
            );
            f.set_cursor_position((area.x + 2 + cursor.0 as u16, area.y + 2));
        }
        Modal::Setup(setup) => {
            let block = Block::bordered()
                .title(" Who should tutor you? ")
                .padding(ratatui::widgets::Padding::horizontal(1));
            if setup.step == SetupStep::Provider {
                let mut buttons = Vec::new();
                for (i, kind) in Kind::ALL.into_iter().enumerate() {
                    let style = if kind == setup.kind {
                        bold().fg(Color::Black).bg(ACCENT)
                    } else {
                        dim()
                    };
                    buttons.push(Span::styled(format!(" {} {} ", i + 1, kind.label()), style));
                    buttons.push(Span::raw(" "));
                }
                let lines = vec![
                    Line::styled("Pick a model provider. You can change it later.", dim()),
                    Line::from(buttons),
                    Line::default(),
                    Line::from(setup.kind.about()),
                    Line::default(),
                    hints(&[
                        ("1-4", "choose"),
                        ("← →", "look"),
                        ("Enter", "choose"),
                        ("Esc", "close"),
                    ]),
                ];
                f.render_widget(Paragraph::new(lines).block(block), area);
                return;
            }

            let (rows, cursor) = setup.input.layout(inner);
            let (question, shown, column, note) = match setup.step {
                SetupStep::Key => {
                    // Never show the key itself, and keep long keys on one line.
                    let dots = setup
                        .input
                        .text()
                        .chars()
                        .count()
                        .min(inner.saturating_sub(1));
                    let note = if setup.has_saved_key {
                        "Leave empty to keep the saved key."
                    } else {
                        "Paste it here. It is saved in a file only you can read."
                    };
                    let question = match setup.kind {
                        Kind::HuggingFace => "Hugging Face token".to_string(),
                        kind => format!("{} API key", kind.label()),
                    };
                    (question, "•".repeat(dots), dots, note)
                }
                SetupStep::Url => (
                    "Server address".to_string(),
                    rows[cursor.1].clone(),
                    cursor.0,
                    "Start llama.cpp with: llama-server -m model.gguf --jinja",
                ),
                _ => (
                    "Model name".to_string(),
                    rows[cursor.1].clone(),
                    cursor.0,
                    match setup.kind {
                        Kind::Local => "llama.cpp ignores this and uses the model it loaded.",
                        _ => "The model must support tool calling.",
                    },
                ),
            };
            let lines = vec![
                Line::styled(format!("{} · {question}", setup.kind.label()), dim()),
                Line::from(shown),
                Line::default(),
                Line::styled(note, dim()),
                Line::default(),
                hints(&[("Enter", "next"), ("Esc", "back")]),
            ];
            f.render_widget(Paragraph::new(lines).block(block), area);
            f.set_cursor_position((area.x + 2 + column as u16, area.y + 2));
        }
        Modal::Confirm { message, .. } => {
            let mut lines: Vec<Line> = wrap(message, inner).into_iter().map(Line::from).collect();
            lines.push(Line::default());
            lines.push(hints(&[("y", "delete"), ("n", "cancel")]));
            f.render_widget(
                Paragraph::new(lines).block(
                    Block::bordered()
                        .title(" Confirm ")
                        .padding(ratatui::widgets::Padding::horizontal(1)),
                ),
                area,
            );
        }
    }
}
