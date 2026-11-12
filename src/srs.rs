//! SM-2 style scheduler shared by flashcards and concept reviews.

pub const DAY: i64 = 86_400;

#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Grade {
    Again,
    Hard,
    Good,
    Easy,
}

impl Grade {
    pub fn parse(s: &str) -> Option<Self> {
        match s.trim().to_ascii_lowercase().as_str() {
            "again" => Some(Self::Again),
            "hard" => Some(Self::Hard),
            "good" => Some(Self::Good),
            "easy" => Some(Self::Easy),
            _ => None,
        }
    }

    pub fn as_str(self) -> &'static str {
        match self {
            Self::Again => "again",
            Self::Hard => "hard",
            Self::Good => "good",
            Self::Easy => "easy",
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Sched {
    pub interval_days: f64,
    pub ease: f64,
    pub reps: i64,
}

impl Default for Sched {
    fn default() -> Self {
        Self {
            interval_days: 0.0,
            ease: 2.5,
            reps: 0,
        }
    }
}

pub fn next(s: Sched, grade: Grade) -> Sched {
    match grade {
        Grade::Again => Sched {
            interval_days: 0.0,
            ease: (s.ease - 0.2).max(1.3),
            reps: 0,
        },
        Grade::Hard => Sched {
            interval_days: if s.reps == 0 {
                1.0
            } else {
                (s.interval_days * 1.2).max(1.0)
            },
            ease: (s.ease - 0.15).max(1.3),
            reps: s.reps + 1,
        },
        Grade::Good => Sched {
            interval_days: match s.reps {
                0 => 1.0,
                1 => 3.0,
                _ => (s.interval_days * s.ease).max(1.0),
            },
            ease: s.ease,
            reps: s.reps + 1,
        },
        Grade::Easy => Sched {
            interval_days: if s.reps == 0 {
                4.0
            } else {
                s.interval_days.max(1.0) * s.ease * 1.3
            },
            ease: s.ease + 0.15,
            reps: s.reps + 1,
        },
    }
}

/// Seconds until the item is due again. A failed item has no interval, so it
/// comes back after `again_delay` instead.
pub fn due_in(s: &Sched, again_delay: i64) -> i64 {
    if s.interval_days <= 0.0 {
        again_delay
    } else {
        (s.interval_days * DAY as f64) as i64
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn good_grades_grow_the_interval() {
        let a = next(Sched::default(), Grade::Good);
        let b = next(a, Grade::Good);
        let c = next(b, Grade::Good);
        assert_eq!(a.interval_days, 1.0);
        assert_eq!(b.interval_days, 3.0);
        assert_eq!(c.interval_days, 7.5);
        assert_eq!(c.reps, 3);
    }

    #[test]
    fn again_resets_and_lowers_ease_with_a_floor() {
        let mut s = next(next(Sched::default(), Grade::Good), Grade::Good);
        s = next(s, Grade::Again);
        assert_eq!(s.reps, 0);
        assert_eq!(s.interval_days, 0.0);
        assert!((s.ease - 2.3).abs() < 1e-9);
        for _ in 0..20 {
            s = next(s, Grade::Again);
        }
        assert_eq!(s.ease, 1.3);
        assert_eq!(due_in(&s, 600), 600);
    }

    #[test]
    fn easy_beats_good_beats_hard() {
        let base = next(next(Sched::default(), Grade::Good), Grade::Good);
        let hard = next(base, Grade::Hard).interval_days;
        let good = next(base, Grade::Good).interval_days;
        let easy = next(base, Grade::Easy).interval_days;
        assert!(hard < good && good < easy);
        assert_eq!(due_in(&next(Sched::default(), Grade::Easy), 600), 4 * DAY);
    }

    #[test]
    fn parses_grades() {
        assert_eq!(Grade::parse(" Good "), Some(Grade::Good));
        assert_eq!(Grade::parse("nope"), None);
    }
}
