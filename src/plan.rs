//! A learning plan: weighted skills, read from a JSON file in the
//! `socratui-plan/1` format (see docs/plan-format.md).

use serde_json::Value;
use std::path::{Path, PathBuf};

pub const FORMAT: &str = "socratui-plan/1";
const PLAN_FILE: &str = "plan.json";

#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Trend {
    Up,
    Down,
    Flat,
}

#[derive(Clone, Debug, PartialEq)]
pub struct PlanSkill {
    pub id: String,
    pub name: String,
    pub weight: f64,
    pub start_level: Option<i64>,
    pub facts: Vec<String>,
    pub trend: Option<Trend>,
    pub prerequisites: Vec<String>,
    pub related: Vec<String>,
    pub former_ids: Vec<String>,
}

#[derive(Clone, Debug, PartialEq)]
pub struct Track {
    pub id: String,
    pub name: String,
    pub facts: Vec<String>,
    pub why: String,
    pub skills: Vec<String>,
}

#[derive(Clone, Debug, PartialEq)]
pub struct Plan {
    pub generated_at: String,
    pub source: String,
    pub summary: String,
    pub skills: Vec<PlanSkill>,
    pub tracks: Vec<Track>,
}

fn text(v: &Value, key: &str, at: &str) -> Result<String, String> {
    v[key]
        .as_str()
        .map(str::to_string)
        .ok_or_else(|| format!("{at}{key} must be text"))
}

fn text_or_empty(v: &Value, key: &str, at: &str) -> Result<String, String> {
    match &v[key] {
        Value::Null => Ok(String::new()),
        _ => text(v, key, at),
    }
}

fn strings(v: &Value, key: &str, at: &str) -> Result<Vec<String>, String> {
    match &v[key] {
        Value::Null => Ok(Vec::new()),
        Value::Array(items) => items
            .iter()
            .map(|i| i.as_str().map(str::to_string))
            .collect::<Option<_>>()
            .ok_or_else(|| format!("{at}{key} must be a list of text")),
        _ => Err(format!("{at}{key} must be a list of text")),
    }
}

fn skill(v: &Value, i: usize) -> Result<PlanSkill, String> {
    let at = format!("skills[{i}].");
    let weight = v["weight"]
        .as_f64()
        .ok_or_else(|| format!("{at}weight must be a number"))?;
    let start_level = match &v["start_level"] {
        Value::Null => None,
        n => Some(
            n.as_i64()
                .ok_or_else(|| format!("{at}start_level must be a whole number"))?,
        ),
    };
    let trend = match v["trend"].as_str() {
        Some("up") => Some(Trend::Up),
        Some("down") => Some(Trend::Down),
        Some("flat") => Some(Trend::Flat),
        _ => None,
    };
    Ok(PlanSkill {
        id: text(v, "id", &at)?,
        name: text(v, "name", &at)?,
        weight: weight.clamp(0.0, 1.0),
        start_level,
        facts: strings(v, "facts", &at)?,
        trend,
        prerequisites: strings(v, "prerequisites", &at)?,
        related: strings(v, "related", &at)?,
        former_ids: strings(v, "former_ids", &at)?,
    })
}

fn track(v: &Value, i: usize) -> Result<Track, String> {
    let at = format!("tracks[{i}].");
    Ok(Track {
        id: text(v, "id", &at)?,
        name: text(v, "name", &at)?,
        facts: strings(v, "facts", &at)?,
        why: text_or_empty(v, "why", &at)?,
        skills: strings(v, "skills", &at)?,
    })
}

fn list<'a>(v: &'a Value, key: &str, required: bool) -> Result<&'a [Value], String> {
    match &v[key] {
        Value::Array(items) => Ok(items),
        Value::Null if !required => Ok(&[]),
        _ => Err(format!("{key} must be a list")),
    }
}

pub fn parse(input: &str) -> Result<Plan, String> {
    let v: Value = serde_json::from_str(input).map_err(|e| format!("not valid JSON: {e}"))?;
    let format = v["format"].as_str().ok_or("format must be text")?;
    if format != FORMAT {
        return Err(match format.strip_prefix("socratui-plan/") {
            Some(n) => format!(
                "This plan file uses format socratui-plan/{n}; this socratui reads {FORMAT}."
            ),
            None => format!("format must be {FORMAT}"),
        });
    }
    Ok(Plan {
        generated_at: text_or_empty(&v, "generated_at", "")?,
        source: text_or_empty(&v, "source", "")?,
        summary: text_or_empty(&v, "summary", "")?,
        skills: list(&v, "skills", true)?
            .iter()
            .enumerate()
            .map(|(i, s)| skill(s, i))
            .collect::<Result<_, _>>()?,
        tracks: list(&v, "tracks", false)?
            .iter()
            .enumerate()
            .map(|(i, t)| track(t, i))
            .collect::<Result<_, _>>()?,
    })
}

/// The plan file: `plan_path` from config.json (a leading `~` is the home
/// directory), else `plan.json` in the data directory.
pub fn plan_path(data_dir: &Path) -> PathBuf {
    let configured = std::fs::read_to_string(data_dir.join(crate::app::CONFIG_FILE))
        .ok()
        .and_then(|t| serde_json::from_str::<Value>(&t).ok())
        .and_then(|v| v["plan_path"].as_str().map(str::to_string));
    match configured {
        Some(p) => match p.strip_prefix("~/") {
            Some(rest) => dirs::home_dir().map_or_else(|| PathBuf::from(&p), |h| h.join(rest)),
            None => PathBuf::from(p),
        },
        None => data_dir.join(PLAN_FILE),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const EXAMPLE: &str = r#"{"format":"socratui-plan/1","generated_at":"2026-11-23T15:00:00Z","source":"example","summary":"412 sources, last 180 days",
 "skills":[{"id":"kubernetes","name":"Kubernetes","weight":0.92,"start_level":1,"facts":["38 mentions","steady","rising"],"trend":"up","prerequisites":["docker"],"related":["terraform","helm"],"former_ids":["k8s"]}],
 "tracks":[{"id":"ml-platform-engineer","name":"ML platform engineer","facts":["high demand","70% overlap"],"why":"Your Python and Kubernetes plus two MLOps tools","skills":["ray","mlflow"]}]}"#;

    const MINIMAL: &str =
        r#"{"format":"socratui-plan/1","skills":[{"id":"a","name":"A","weight":0.5}]}"#;

    #[test]
    fn parses_the_spec_example() {
        let plan = parse(EXAMPLE).unwrap();
        let s = &plan.skills[0];
        assert_eq!(s.id, "kubernetes");
        assert_eq!(s.weight, 0.92);
        assert_eq!(s.start_level, Some(1));
        assert_eq!(s.trend, Some(Trend::Up));
        assert_eq!(s.prerequisites, ["docker"]);
        assert_eq!(s.former_ids, ["k8s"]);
        assert_eq!(plan.summary, "412 sources, last 180 days");
        assert_eq!(plan.tracks[0].skills, ["ray", "mlflow"]);
    }

    #[test]
    fn optional_fields_default() {
        let plan = parse(MINIMAL).unwrap();
        let s = &plan.skills[0];
        assert!(s.facts.is_empty() && s.former_ids.is_empty());
        assert_eq!(s.start_level, None);
        assert!(plan.tracks.is_empty());
        assert_eq!(plan.summary, "");
    }

    #[test]
    fn ignores_unknown_fields() {
        let input = r#"{"format":"socratui-plan/1","x":1,"skills":[{"id":"a","name":"A","weight":0.5,"y":[]}]}"#;
        assert!(parse(input).is_ok());
    }

    #[test]
    fn refuses_other_versions() {
        let err = parse(r#"{"format":"socratui-plan/2","skills":[]}"#).unwrap_err();
        assert_eq!(
            err,
            "This plan file uses format socratui-plan/2; this socratui reads socratui-plan/1."
        );
    }

    #[test]
    fn refuses_wrong_types() {
        let bad_weight =
            r#"{"format":"socratui-plan/1","skills":[{"id":"a","name":"A","weight":"high"}]}"#;
        assert!(parse(bad_weight).unwrap_err().contains("skills[0].weight"));
        let no_skills = r#"{"format":"socratui-plan/1"}"#;
        assert!(parse(no_skills).unwrap_err().contains("skills"));
        assert!(parse("hello").unwrap_err().contains("not valid JSON"));
    }

    #[test]
    fn clamps_weight() {
        let weights = |w: &str| {
            let input = format!(
                r#"{{"format":"socratui-plan/1","skills":[{{"id":"a","name":"A","weight":{w}}}]}}"#
            );
            parse(&input).unwrap().skills[0].weight
        };
        assert_eq!(weights("1.7"), 1.0);
        assert_eq!(weights("-0.2"), 0.0);
    }

    fn temp_dir(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("socratui-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    #[test]
    fn plan_path_defaults_to_data_dir() {
        let dir = temp_dir("plan-default");
        assert_eq!(plan_path(&dir), dir.join("plan.json"));
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn plan_path_reads_config_and_expands_home() {
        let dir = temp_dir("plan-config");
        let config = dir.join("config.json");
        std::fs::write(&config, r#"{"plan_path":"~/x/p.json"}"#).unwrap();
        assert_eq!(plan_path(&dir), dirs::home_dir().unwrap().join("x/p.json"));
        std::fs::write(&config, r#"{"plan_path":"/abs/p.json"}"#).unwrap();
        assert_eq!(plan_path(&dir), PathBuf::from("/abs/p.json"));
        std::fs::remove_dir_all(&dir).unwrap();
    }
}
