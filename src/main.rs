mod api;
mod app;
mod db;
mod export;
mod input;
mod provider;
mod srs;
mod tutor;
mod ui;

use anyhow::{Context, Result};
use ratatui::DefaultTerminal;
use ratatui::crossterm::event::{self, DisableBracketedPaste, EnableBracketedPaste};
use ratatui::crossterm::execute;
use std::path::PathBuf;
use std::time::Duration;

fn env(name: &str) -> Option<String> {
    std::env::var(name).ok().filter(|v| !v.trim().is_empty())
}

fn main() -> Result<()> {
    let data_dir = match env("SOCRATIK_DATA_DIR") {
        Some(dir) => PathBuf::from(dir),
        None => dirs::data_dir()
            .context("could not find a data directory; set SOCRATIK_DATA_DIR")?
            .join("socratik"),
    };
    std::fs::create_dir_all(&data_dir)?;
    let db = db::Db::open(&data_dir.join("socratik.db"))?;
    // Saved settings win over keys in the environment. With neither, the app asks.
    let mut provider = provider::Provider::load(&data_dir.join(app::CONFIG_FILE))
        .or_else(|| provider::Provider::from_env(env));
    if let (Some(provider), Some(model)) = (&mut provider, env("SOCRATIK_MODEL")) {
        provider.model = model;
    }
    let mut app = app::App::new(db, provider, data_dir)?;

    let mut terminal = ratatui::init();
    let _ = execute!(std::io::stdout(), EnableBracketedPaste);
    let result = run(&mut terminal, &mut app);
    let _ = execute!(std::io::stdout(), DisableBracketedPaste);
    ratatui::restore();
    result
}

fn run(terminal: &mut DefaultTerminal, app: &mut app::App) -> Result<()> {
    while !app.quit {
        app.poll_stream()?;
        terminal.draw(|f| ui::draw(f, app))?;
        // The short timeout keeps streamed text flowing while no key is pressed.
        if event::poll(Duration::from_millis(50))? {
            while event::poll(Duration::ZERO)? {
                app.on_event(event::read()?)?;
            }
        }
    }
    Ok(())
}
