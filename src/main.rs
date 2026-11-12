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
use std::path::{Path, PathBuf};
use std::time::Duration;

fn env(name: &str) -> Option<String> {
    std::env::var(name).ok().filter(|v| !v.trim().is_empty())
}

/// The app was called Socratik, and kept its data in `socratik/socratik.db`.
/// Moves that folder to the new name once, if nothing is there yet.
fn move_old_data(base: &Path) -> Result<()> {
    let (old, new) = (base.join("socratik"), base.join("socratui"));
    if new.exists() || !old.is_dir() {
        return Ok(());
    }
    std::fs::rename(&old, &new)?;
    let db = new.join("socratik.db");
    if db.exists() {
        std::fs::rename(db, new.join("socratui.db"))?;
    }
    Ok(())
}

fn main() -> Result<()> {
    let data_dir = match env("SOCRATUI_DATA_DIR") {
        Some(dir) => PathBuf::from(dir),
        None => {
            let base = dirs::data_dir()
                .context("could not find a data directory; set SOCRATUI_DATA_DIR")?;
            move_old_data(&base)?;
            base.join("socratui")
        }
    };
    std::fs::create_dir_all(&data_dir)?;
    let db = db::Db::open(&data_dir.join("socratui.db"))?;
    // Saved settings win over keys in the environment. With neither, the app asks.
    let mut provider = provider::Provider::load(&data_dir.join(app::CONFIG_FILE))
        .or_else(|| provider::Provider::from_env(env));
    if let (Some(provider), Some(model)) = (&mut provider, env("SOCRATUI_MODEL")) {
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn moves_socratik_data_once() {
        let base = std::env::temp_dir().join(format!("socratui-move-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&base);
        std::fs::create_dir_all(base.join("socratik")).unwrap();
        std::fs::write(base.join("socratik/socratik.db"), "lessons").unwrap();
        std::fs::write(base.join("socratik/config.json"), "{}").unwrap();

        move_old_data(&base).unwrap();
        assert!(!base.join("socratik").exists());
        assert_eq!(
            std::fs::read_to_string(base.join("socratui/socratui.db")).unwrap(),
            "lessons"
        );
        assert!(base.join("socratui/config.json").exists());

        // With the new folder in place, an old one that appears is left alone.
        std::fs::create_dir_all(base.join("socratik")).unwrap();
        move_old_data(&base).unwrap();
        assert!(base.join("socratik").exists());
        std::fs::remove_dir_all(&base).unwrap();
    }
}
