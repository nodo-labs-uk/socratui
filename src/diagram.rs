//! Graph diagrams. The tutor writes Graphviz DOT in a ```dot block, the chat
//! shows the source, and Ctrl+O opens the latest one as a picture.

use layout::backends::svg::SVGWriter;
use layout::gv::{DotParser, GraphBuilder};
use std::path::Path;
use std::process::{Command, Stdio};

/// The source of the last complete ```dot (or ```graphviz) block in `text`.
pub fn last_dot(text: &str) -> Option<String> {
    let mut found = None;
    // Inside a fence: Some(lines) for a DOT block, None for any other block.
    let mut fence: Option<Option<Vec<&str>>> = None;
    for line in text.lines() {
        let Some(lang) = line.trim_start().strip_prefix("```") else {
            if let Some(Some(lines)) = &mut fence {
                lines.push(line);
            }
            continue;
        };
        fence = match fence.take() {
            Some(Some(lines)) => {
                found = Some(lines.join("\n"));
                None
            }
            Some(None) => None,
            None => Some(matches!(lang.trim(), "dot" | "graphviz").then(Vec::new)),
        };
    }
    found
}

/// Lays out DOT source as an SVG picture.
pub fn svg(dot: &str) -> Result<String, String> {
    let graph = DotParser::new(dot).process()?;
    // The layout crate can panic on input it does not expect. The text comes
    // from a model, so a panic must not take the terminal down with it.
    let hook = std::panic::take_hook();
    std::panic::set_hook(Box::new(|_| {}));
    let drawn = std::panic::catch_unwind(|| {
        let mut builder = GraphBuilder::new();
        builder.visit_graph(&graph);
        let mut writer = SVGWriter::new();
        builder.get().do_it(false, false, false, &mut writer);
        writer.finalize()
    });
    std::panic::set_hook(hook);
    let mut picture = drawn.map_err(|_| "the layout failed".to_string())?;
    // The lines are black on a transparent page, which a dark viewer hides.
    if let Some(start) = picture.find("<svg")
        && let Some(end) = picture[start..].find('>')
    {
        let at = start + end + 1;
        picture.insert_str(at, r#"<rect width="100%" height="100%" fill="white"/>"#);
    }
    Ok(picture)
}

/// Writes the picture to `path` and opens it in the desktop's default viewer.
pub fn open(dot: &str, path: &Path) -> Result<(), String> {
    std::fs::write(path, svg(dot)?).map_err(|e| e.to_string())?;
    let mut viewer = Command::new("xdg-open")
        .arg(path)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .map_err(|e| format!("xdg-open: {e}"))?;
    std::thread::spawn(move || viewer.wait());
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn finds_the_last_dot_block_and_skips_other_fences() {
        let text = "```dot\ndigraph { a -> b }\n```\n\
                    ```rust\nlet x = 1;\n```\n\
                    ```graphviz\ndigraph {\n  x -> y\n}\n```\n\
                    ```dot\nunfinished";
        assert_eq!(last_dot(text).as_deref(), Some("digraph {\n  x -> y\n}"));
        assert_eq!(last_dot("```text\na -> b\n```"), None);
    }

    #[test]
    fn draws_a_graph_and_reports_bad_source() {
        let picture = svg("digraph { borrow -> \"use\"; borrow -> drop }").unwrap();
        assert!(picture.contains("borrow"));
        let page = picture.find(r#"<rect width="100%" height="100%" fill="white"/>"#);
        assert!(page.is_some_and(|i| i < picture.find("borrow").unwrap()));
        assert!(svg("digraph { a -> }").is_err());
    }
}
