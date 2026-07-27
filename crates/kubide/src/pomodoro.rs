//! A work timer.
//!
//! Deliberately not a notification system. It shows a countdown in the status
//! bar and stops when the period ends; it does not pop up, make noise, or take
//! focus. An editor that interrupts you is the opposite of what this is for.
//!
//! Time is asked for on every read rather than accumulated on a tick, so the
//! countdown stays right even when the window is in the background and Windows
//! throttles the timer down to almost nothing.

use std::time::{Duration, Instant};

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Phase {
    Work,
    ShortBreak,
    LongBreak,
}

impl Phase {
    fn label(self) -> &'static str {
        match self {
            Phase::Work => "work",
            Phase::ShortBreak => "break",
            Phase::LongBreak => "long break",
        }
    }
}

pub struct Pomodoro {
    phase: Phase,
    /// `None` while paused; the remaining time is held in `left`.
    started: Option<Instant>,
    left: Duration,
    /// Work periods finished since the last long break.
    done: u32,
    cfg: kb_cfg::Pomodoro,
}

impl Pomodoro {
    pub fn new(cfg: kb_cfg::Pomodoro) -> Self {
        Self {
            phase: Phase::Work,
            started: None,
            left: Duration::from_secs(cfg.work.max(1) as u64 * 60),
            done: 0,
            cfg,
        }
    }

    /// Picks up a changed config without resetting a running period.
    ///
    /// Changing the work length mid-period and having the countdown jump would
    /// be worse than it taking effect next time round.
    pub fn set_config(&mut self, cfg: kb_cfg::Pomodoro) {
        self.cfg = cfg;
        if self.started.is_none() && self.done == 0 && self.phase == Phase::Work {
            self.left = self.length(Phase::Work);
        }
    }

    fn length(&self, phase: Phase) -> Duration {
        let minutes = match phase {
            Phase::Work => self.cfg.work,
            Phase::ShortBreak => self.cfg.short_break,
            Phase::LongBreak => self.cfg.long_break,
        };
        Duration::from_secs(minutes.max(1) as u64 * 60)
    }

    pub fn running(&self) -> bool {
        self.started.is_some()
    }

    /// Time left, clamped at zero.
    pub fn remaining(&self) -> Duration {
        match self.started {
            Some(at) => self.left.saturating_sub(at.elapsed()),
            None => self.left,
        }
    }

    pub fn finished(&self) -> bool {
        self.running() && self.remaining().is_zero()
    }

    /// Start, or pause and keep what is left.
    pub fn toggle(&mut self) {
        match self.started.take() {
            Some(at) => self.left = self.left.saturating_sub(at.elapsed()),
            None => self.started = Some(Instant::now()),
        }
    }

    /// Back to the start of the current period, paused.
    pub fn reset(&mut self) {
        self.started = None;
        self.left = self.length(self.phase);
    }

    /// Moves to the next period. Called when one ends, and by the user.
    pub fn advance(&mut self) {
        self.phase = match self.phase {
            Phase::Work => {
                self.done += 1;
                if self.cfg.rounds > 0 && self.done.is_multiple_of(self.cfg.rounds) {
                    Phase::LongBreak
                } else {
                    Phase::ShortBreak
                }
            }
            _ => Phase::Work,
        };
        self.left = self.length(self.phase);
        self.started = self.cfg.auto_advance.then(Instant::now);
    }

    /// Rolls over a finished period. Returns true when something changed, so
    /// the caller only redraws for that.
    pub fn poll(&mut self) -> bool {
        if !self.finished() {
            return false;
        }
        if self.cfg.auto_advance {
            self.advance();
        } else {
            // Stop and sit there. Silently starting the break while someone is
            // mid-thought is the nagging this is meant to avoid.
            self.started = None;
            self.left = Duration::ZERO;
        }
        true
    }

    /// `work 24:13`, or `work 25:00 paused`.
    pub fn label(&self) -> String {
        let left = self.remaining().as_secs();
        let state = if self.remaining().is_zero() {
            " done"
        } else if self.running() {
            ""
        } else {
            " paused"
        };
        // No clock glyph here. It is a Nerd Font codepoint and this module has
        // no idea what the font can draw; the status bar knows, and prepends
        // one when it is safe.
        format!("{} {:02}:{:02}{state}", self.phase.label(), left / 60, left % 60)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn cfg() -> kb_cfg::Pomodoro {
        kb_cfg::Pomodoro {
            work: 25,
            short_break: 5,
            long_break: 15,
            rounds: 4,
            auto_advance: false,
        }
    }

    #[test]
    fn it_starts_paused_at_the_full_period() {
        // Opening the editor should not start a clock on you.
        let p = Pomodoro::new(cfg());
        assert!(!p.running());
        assert_eq!(p.remaining(), Duration::from_secs(25 * 60));
        assert!(p.label().contains("work 25:00"));
        assert!(p.label().contains("paused"));
    }

    #[test]
    fn pausing_keeps_what_is_left() {
        let mut p = Pomodoro::new(cfg());
        p.toggle();
        assert!(p.running());
        p.toggle();
        assert!(!p.running());
        // Some fraction of a second has passed, so it must be under the full
        // period but nowhere near zero.
        assert!(p.remaining() <= Duration::from_secs(25 * 60));
        assert!(p.remaining() > Duration::from_secs(24 * 60));
    }

    #[test]
    fn the_long_break_comes_after_the_configured_rounds() {
        let mut p = Pomodoro::new(cfg());
        for _ in 0..3 {
            p.advance(); // work -> short break
            p.advance(); // break -> work
        }
        assert_eq!(p.phase, Phase::Work);
        p.advance();
        assert_eq!(p.phase, Phase::LongBreak, "fourth work period");
    }

    #[test]
    fn a_finished_period_does_not_start_the_next_one_by_itself() {
        // Counting your break while you are still typing is nagging.
        let mut p = Pomodoro::new(cfg());
        p.left = Duration::ZERO;
        p.started = Some(Instant::now());
        assert!(p.finished());
        assert!(p.poll());
        assert!(!p.running());
        assert_eq!(p.phase, Phase::Work, "still on the same period");
        assert!(p.label().contains("done"));
    }

    #[test]
    fn auto_advance_rolls_over_and_keeps_running() {
        let mut p = Pomodoro::new(kb_cfg::Pomodoro { auto_advance: true, ..cfg() });
        p.left = Duration::ZERO;
        p.started = Some(Instant::now());
        assert!(p.poll());
        assert_eq!(p.phase, Phase::ShortBreak);
        assert!(p.running());
    }

    #[test]
    fn polling_an_idle_timer_costs_nothing() {
        // It runs on every frame; it must not report a change when there is
        // none, or the window would redraw forever.
        let mut p = Pomodoro::new(cfg());
        assert!(!p.poll());
        p.toggle();
        assert!(!p.poll());
    }

    #[test]
    fn reset_goes_back_to_the_start_of_this_period() {
        let mut p = Pomodoro::new(cfg());
        p.advance();
        assert_eq!(p.phase, Phase::ShortBreak);
        p.toggle();
        p.reset();
        assert!(!p.running());
        assert_eq!(p.remaining(), Duration::from_secs(5 * 60));
    }

    #[test]
    fn a_zero_length_period_is_treated_as_one_minute() {
        // Straight from a config file, and a zero-length period would be
        // permanently finished.
        let p = Pomodoro::new(kb_cfg::Pomodoro { work: 0, ..cfg() });
        assert_eq!(p.remaining(), Duration::from_secs(60));
    }

    #[test]
    fn zero_rounds_never_reaches_the_long_break() {
        // `done % 0` would panic.
        let mut p = Pomodoro::new(kb_cfg::Pomodoro { rounds: 0, ..cfg() });
        for _ in 0..10 {
            p.advance();
        }
        assert_ne!(p.phase, Phase::LongBreak);
    }

    #[test]
    fn changing_the_config_does_not_disturb_a_running_period() {
        // The countdown jumping under you is worse than it taking effect next
        // time round.
        let mut p = Pomodoro::new(cfg());
        p.toggle();
        let before = p.remaining();
        p.set_config(kb_cfg::Pomodoro { work: 50, ..cfg() });
        assert!(p.remaining() <= before);
        assert!(p.remaining() > Duration::from_secs(24 * 60));
    }

    #[test]
    fn changing_the_config_while_idle_takes_effect_at_once() {
        let mut p = Pomodoro::new(cfg());
        p.set_config(kb_cfg::Pomodoro { work: 50, ..cfg() });
        assert_eq!(p.remaining(), Duration::from_secs(50 * 60));
    }
}
