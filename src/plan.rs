//! A learning plan: weighted skills, read from a JSON file in the
//! `socratui-plan/1` format (see docs/plan-format.md).

use serde_json::Value;
use std::collections::{HashMap, HashSet};
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

impl Plan {
    /// The name of the skill with this id, or the id if the plan lacks it.
    pub fn name_of<'a>(&'a self, id: &'a str) -> &'a str {
        self.skills
            .iter()
            .find(|s| s.id == id)
            .map_or(id, |s| s.name.as_str())
    }
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

pub const DEFAULT_TARGET: i64 = 4;
const PREREQUISITE_BELOW: f64 = 2.0;
const FOLLOWED_BOOST: f64 = 1.5;

/// Measured progress on a skill, from a linked topic.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Progress {
    pub level: f64,
    pub target: i64,
}

#[derive(Clone, Debug, PartialEq)]
pub struct RankedSkill {
    pub id: String,
    pub name: String,
    pub weight: f64,
    pub facts: Vec<String>,
    pub trend: Option<Trend>,
    pub level: f64,
    pub target: i64,
    pub priority: f64,
    /// Name of the skill this one unlocks, if it was lifted for that reason.
    pub before: Option<String>,
}

#[derive(Clone, Debug, PartialEq)]
pub struct Ranking {
    pub next: Vec<RankedSkill>,
    pub done: Vec<RankedSkill>,
}

/// Orders the plan's skills by weight and the gap to the target level.
/// `progress` is keyed by skill id; `followed` holds the followed track ids.
pub fn rank(
    plan: &Plan,
    progress: &HashMap<String, Progress>,
    followed: &HashSet<String>,
) -> Ranking {
    let in_followed: HashSet<&str> = plan
        .tracks
        .iter()
        .filter(|t| followed.contains(&t.id))
        .flat_map(|t| t.skills.iter().map(String::as_str))
        .collect();
    let mut ranked: Vec<RankedSkill> = plan
        .skills
        .iter()
        .map(|s| {
            let measured = progress.get(&s.id);
            let level = measured.map_or_else(|| s.start_level.unwrap_or(0) as f64, |p| p.level);
            let target = measured.map_or(DEFAULT_TARGET, |p| p.target);
            let gap = if target > 0 {
                1.0 - level / target as f64
            } else {
                0.0
            };
            let boost = if in_followed.contains(s.id.as_str()) {
                FOLLOWED_BOOST
            } else {
                1.0
            };
            RankedSkill {
                id: s.id.clone(),
                name: s.name.clone(),
                weight: s.weight,
                facts: s.facts.clone(),
                trend: s.trend,
                level,
                target,
                priority: (s.weight * gap).max(0.0) * boost,
                before: None,
            }
        })
        .collect();

    let base: Vec<f64> = ranked.iter().map(|r| r.priority).collect();
    for (dependent, skill) in plan.skills.iter().enumerate() {
        for id in &skill.prerequisites {
            let Some(p) = ranked.iter().position(|r| &r.id == id) else {
                continue;
            };
            let low =
                ranked[p].level < PREREQUISITE_BELOW && ranked[p].level < ranked[p].target as f64;
            let lifted = ranked[p].before.is_some();
            let current = if lifted { ranked[p].priority } else { base[p] };
            if low && base[dependent] > current {
                ranked[p].priority = base[dependent];
                ranked[p].before = Some(skill.name.clone());
            }
        }
    }

    let (done, mut next): (Vec<_>, Vec<_>) =
        ranked.into_iter().partition(|r| r.level >= r.target as f64);
    next.sort_by(|a, b| {
        b.priority
            .total_cmp(&a.priority)
            .then(b.before.is_some().cmp(&a.before.is_some()))
            .then_with(|| a.name.cmp(&b.name))
    });
    Ranking { next, done }
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

    fn ps(id: &str, weight: f64, start: Option<i64>, prereqs: &[&str]) -> PlanSkill {
        PlanSkill {
            id: id.into(),
            name: id.to_uppercase(),
            weight,
            start_level: start,
            facts: vec![],
            trend: None,
            prerequisites: prereqs.iter().map(|s| s.to_string()).collect(),
            related: vec![],
            former_ids: vec![],
        }
    }

    fn plan_of(skills: Vec<PlanSkill>, tracks: Vec<Track>) -> Plan {
        Plan {
            generated_at: String::new(),
            source: String::new(),
            summary: String::new(),
            skills,
            tracks,
        }
    }

    fn none() -> (HashMap<String, Progress>, HashSet<String>) {
        (HashMap::new(), HashSet::new())
    }

    fn close(a: f64, b: f64) -> bool {
        (a - b).abs() < 1e-9
    }

    #[test]
    fn weight_times_gap() {
        let (mut prog, fol) = none();
        let p = plan_of(vec![ps("a", 0.8, None, &[])], vec![]);
        assert!(close(rank(&p, &prog, &fol).next[0].priority, 0.8));
        let p2 = plan_of(vec![ps("a", 0.8, Some(2), &[])], vec![]);
        assert!(close(rank(&p2, &prog, &fol).next[0].priority, 0.4));
        prog.insert(
            "a".into(),
            Progress {
                level: 3.0,
                target: 4,
            },
        );
        assert!(close(rank(&p2, &prog, &fol).next[0].priority, 0.2));
    }

    #[test]
    fn reached_target_is_done() {
        let (mut prog, fol) = none();
        prog.insert(
            "a".into(),
            Progress {
                level: 4.0,
                target: 4,
            },
        );
        let r = rank(&plan_of(vec![ps("a", 0.8, None, &[])], vec![]), &prog, &fol);
        assert!(r.next.is_empty());
        assert_eq!(r.done[0].id, "a");
    }

    #[test]
    fn prerequisite_first() {
        let (mut prog, fol) = none();
        let p = plan_of(
            vec![
                ps("kubernetes", 0.9, Some(1), &["docker"]),
                ps("docker", 0.2, None, &[]),
            ],
            vec![],
        );
        let r = rank(&p, &prog, &fol);
        assert_eq!(r.next[0].id, "docker");
        assert_eq!(r.next[0].before.as_deref(), Some("KUBERNETES"));
        assert_eq!(r.next[1].id, "kubernetes");
        prog.insert(
            "docker".into(),
            Progress {
                level: 2.0,
                target: 4,
            },
        );
        let r = rank(&p, &prog, &fol);
        assert_eq!(r.next[0].id, "kubernetes");
        assert!(r.next.iter().all(|s| s.before.is_none()));
    }

    #[test]
    fn followed_track_boost() {
        let (prog, mut fol) = none();
        let track = Track {
            id: "t".into(),
            name: "T".into(),
            facts: vec![],
            why: String::new(),
            skills: vec!["b".into()],
        };
        let p = plan_of(
            vec![ps("a", 0.5, None, &[]), ps("b", 0.5, None, &[])],
            vec![track],
        );
        fol.insert("t".into());
        let r = rank(&p, &prog, &fol);
        assert_eq!(r.next[0].id, "b");
        assert!(close(r.next[0].priority, 0.75));
        assert!(close(r.next[1].priority, 0.5));
    }

    #[test]
    fn missing_prerequisite_id_is_ignored() {
        let (prog, fol) = none();
        let p = plan_of(vec![ps("a", 0.5, None, &["ghost"])], vec![]);
        let r = rank(&p, &prog, &fol);
        assert_eq!(r.next.len(), 1);
        assert!(close(r.next[0].priority, 0.5));
    }
}
