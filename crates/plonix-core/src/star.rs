//! The one-time "Star Plonix on GitHub" card.
//!
//! It is asked for only after Plonix has proved useful: on a third different
//! day of use, and right after the researcher finishes something (saves a
//! finding, an Access check comes back, a report is saved). Never on first
//! launch, never in the demo, never mid-task. "Maybe later" asks again two
//! weeks on, once; "No thanks" or a star ends it for good.
//!
//! Everything counted here stays in `star.json` in the Plonix folder. Nothing
//! is sent anywhere.

use anyhow::Result;
use serde::{Deserialize, Serialize};

use crate::paths::{Home, write_atomic};

/// Different days of use before the card may show.
const MIN_DAYS: u32 = 3;
/// After this many days a plain Bench send that got an answer may show it
/// too, for researchers who never save findings or run an Access check.
const SEND_DAYS: u32 = 7;
/// "Maybe later" asks again after this many days.
const SNOOZE_DAYS: i64 = 14;
/// The card shows at most this many times, answered or not.
const MAX_SHOWN: u32 = 3;
const DAY_MS: i64 = 86_400_000;

#[derive(Debug, Default, Clone, Serialize, Deserialize, PartialEq)]
#[serde(default)]
struct State {
    /// Day numbers (days since 1970, UTC).
    first_day: i64,
    last_day: i64,
    days: u32,
    moments: u32,
    shown: u32,
    shown_day: i64,
    snoozes: u32,
    snoozed_until: i64,
    /// Starred, or "No thanks": never asked again.
    done: bool,
}

/// What the window reports.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Event {
    /// A project window opened.
    Open,
    /// The researcher finished something worth having.
    Finished,
    /// A Bench send got a response.
    Sent,
    Later,
    Never,
    Starred,
}

impl Event {
    pub fn parse(s: &str) -> Option<Self> {
        Some(match s {
            "open" => Self::Open,
            "finished" => Self::Finished,
            "sent" => Self::Sent,
            "later" => Self::Later,
            "never" => Self::Never,
            "starred" => Self::Starred,
            _ => return None,
        })
    }
}

fn path(home: &Home) -> std::path::PathBuf {
    home.root.join("star.json")
}

fn read(home: &Home) -> State {
    std::fs::read(path(home)).ok().and_then(|b| serde_json::from_slice(&b).ok()).unwrap_or_default()
}

static WRITE: std::sync::Mutex<()> = std::sync::Mutex::new(());

/// Records `event` and says whether the card should show now.
pub fn note(home: &Home, event: Event) -> Result<bool> {
    let _held = WRITE.lock().unwrap_or_else(|e| e.into_inner());
    let mut st = read(home);
    let before = st.clone();
    let ask = step(&mut st, event, crate::model::now_ms() / DAY_MS);
    if st != before {
        std::fs::create_dir_all(&home.root)?;
        write_atomic(&path(home), &serde_json::to_vec_pretty(&st)?)?;
    }
    Ok(ask)
}

fn step(st: &mut State, event: Event, today: i64) -> bool {
    if st.last_day != today {
        st.days += 1;
        st.last_day = today;
        if st.first_day == 0 {
            st.first_day = today;
        }
    }
    let moment = match event {
        Event::Open => false,
        Event::Finished => true,
        Event::Sent => st.days >= SEND_DAYS,
        Event::Later => {
            st.snoozes += 1;
            st.snoozed_until = today + SNOOZE_DAYS;
            // Snoozed twice is a no.
            st.done |= st.snoozes >= 2;
            false
        }
        Event::Never | Event::Starred => {
            st.done = true;
            false
        }
    };
    if matches!(event, Event::Finished | Event::Sent) {
        st.moments += 1;
    }
    let ask = moment && !st.done && st.days >= MIN_DAYS && today >= st.snoozed_until && st.shown_day != today && st.shown < MAX_SHOWN;
    if ask {
        st.shown += 1;
        st.shown_day = today;
    }
    ask
}

#[cfg(test)]
mod tests {
    use super::*;

    fn days(st: &mut State, from: i64, n: i64) {
        for d in from..from + n {
            assert!(!step(st, Event::Open, d));
        }
    }

    #[test]
    fn waits_for_the_third_day_and_a_finished_task() {
        let mut st = State::default();
        days(&mut st, 100, 2);
        assert!(!step(&mut st, Event::Finished, 101), "two days is too early");
        assert!(!step(&mut st, Event::Open, 102), "opening never asks");
        assert!(!step(&mut st, Event::Sent, 102), "a send asks only after a week");
        assert!(step(&mut st, Event::Finished, 102));
        assert!(!step(&mut st, Event::Finished, 102), "once a day at most");
    }

    #[test]
    fn a_send_counts_after_a_week() {
        let mut st = State::default();
        days(&mut st, 100, 6);
        assert!(!step(&mut st, Event::Sent, 105));
        assert!(step(&mut st, Event::Sent, 106));
    }

    #[test]
    fn later_snoozes_once_then_stops() {
        let mut st = State::default();
        days(&mut st, 100, 3);
        assert!(step(&mut st, Event::Finished, 102));
        step(&mut st, Event::Later, 102);
        assert!(!step(&mut st, Event::Finished, 110), "snoozed for two weeks");
        assert!(step(&mut st, Event::Finished, 116));
        step(&mut st, Event::Later, 116);
        assert!(!step(&mut st, Event::Finished, 200), "a second later is a no");
    }

    #[test]
    fn never_and_starred_end_it() {
        for end in [Event::Never, Event::Starred] {
            let mut st = State::default();
            days(&mut st, 100, 3);
            step(&mut st, end, 102);
            assert!(!step(&mut st, Event::Finished, 102));
            assert!(!step(&mut st, Event::Finished, 300));
        }
    }

    #[test]
    fn an_ignored_card_shows_three_times_at_most() {
        let mut st = State::default();
        days(&mut st, 100, 3);
        let shown = (102..120).filter(|d| step(&mut st, Event::Finished, *d)).count();
        assert_eq!(shown, 3);
    }

    #[test]
    fn kept_in_the_plonix_folder() {
        let home = Home { root: tempfile::tempdir().unwrap().keep() };
        assert!(!note(&home, Event::Open).unwrap());
        assert_eq!(read(&home).days, 1);
        note(&home, Event::Never).unwrap();
        assert!(read(&home).done);
    }
}
