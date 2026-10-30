//! Slideshow timing, without egui: when to move on, pausing, looping.
//!
//! Each picture's time starts once it is ready (loaded, or known not to load), so slow
//! decodes don't eat into it, and an animation plays through at least once.

use std::time::{Duration, Instant};

/// Intervals offered in the viewer, in seconds.
pub const INTERVALS: [u64; 6] = [2, 3, 5, 10, 20, 60];
pub const DEFAULT_SECS: u64 = 5;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Step {
    Stay,
    /// Show this result next.
    Go(usize),
    /// The last picture has had its time and looping is off.
    End,
}

#[derive(Debug, Clone)]
pub struct Slideshow {
    pub interval: Duration,
    pub looping: bool,
    /// The picture being timed and when its time started (once it was ready).
    showing: Option<(usize, Option<Instant>)>,
    /// Time already spent on the current picture when paused.
    paused: Option<Duration>,
}

impl Slideshow {
    pub fn new(interval: Duration, looping: bool) -> Slideshow {
        Slideshow { interval, looping, showing: None, paused: None }
    }

    pub fn is_paused(&self) -> bool {
        self.paused.is_some()
    }

    pub fn toggle_pause(&mut self, now: Instant) {
        match self.paused.take() {
            // Resume: carry on where the picture's time stopped.
            Some(spent) => {
                if let Some((_, start)) = &mut self.showing {
                    *start = now.checked_sub(spent).or(Some(now));
                }
            }
            None => self.paused = Some(self.elapsed(now)),
        }
    }

    fn elapsed(&self, now: Instant) -> Duration {
        match self.showing {
            Some((_, Some(start))) => now.saturating_duration_since(start),
            _ => Duration::ZERO,
        }
    }

    /// Called every frame with the result on screen (`pos` of `total`), whether it is ready,
    /// and how long its animation lasts (zero for a still picture).
    pub fn tick(&mut self, now: Instant, pos: usize, total: usize, ready: bool, animation: Duration) -> Step {
        match &mut self.showing {
            // Moved on (by the user or by us): time the new picture from scratch.
            Some((p, _)) if *p != pos => {
                self.showing = Some((pos, None));
                if self.paused.is_some() {
                    self.paused = Some(Duration::ZERO);
                }
            }
            None => self.showing = Some((pos, None)),
            _ => {}
        }
        if let Some((_, start @ None)) = &mut self.showing
            && ready
        {
            *start = Some(now);
        }
        if self.paused.is_some() || !ready {
            return Step::Stay;
        }
        if self.elapsed(now) < self.interval.max(animation) {
            return Step::Stay;
        }
        if pos + 1 < total {
            Step::Go(pos + 1)
        } else if self.looping && total > 1 {
            Step::Go(0)
        } else {
            Step::End
        }
    }

    /// How long until the next [`Slideshow::tick`] could move on (to wake the window then).
    pub fn wait(&self, now: Instant, animation: Duration) -> Option<Duration> {
        if self.paused.is_some() {
            return None;
        }
        match self.showing {
            Some((_, Some(_))) => Some(self.interval.max(animation).saturating_sub(self.elapsed(now))),
            _ => None,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const S: Duration = Duration::from_secs(1);

    #[test]
    fn moves_on_after_the_interval_once_ready() {
        let t0 = Instant::now();
        let mut s = Slideshow::new(5 * S, false);
        assert_eq!(s.tick(t0, 0, 3, false, Duration::ZERO), Step::Stay, "still loading");
        assert_eq!(s.tick(t0 + 9 * S, 0, 3, false, Duration::ZERO), Step::Stay, "loading time doesn't count");
        assert_eq!(s.tick(t0 + 10 * S, 0, 3, true, Duration::ZERO), Step::Stay);
        assert_eq!(s.wait(t0 + 12 * S, Duration::ZERO), Some(3 * S));
        assert_eq!(s.tick(t0 + 15 * S, 0, 3, true, Duration::ZERO), Step::Go(1));
        // The user skipped ahead: the new picture gets its full time.
        assert_eq!(s.tick(t0 + 15 * S, 2, 3, true, Duration::ZERO), Step::Stay);
        assert_eq!(s.tick(t0 + 19 * S, 2, 3, true, Duration::ZERO), Step::Stay);
        assert_eq!(s.tick(t0 + 20 * S, 2, 3, true, Duration::ZERO), Step::End, "last one, no looping");
        s.looping = true;
        assert_eq!(s.tick(t0 + 20 * S, 2, 3, true, Duration::ZERO), Step::Go(0));
        let mut one = Slideshow::new(S, true);
        one.tick(t0, 0, 1, true, Duration::ZERO);
        assert_eq!(one.tick(t0 + 2 * S, 0, 1, true, Duration::ZERO), Step::End, "nothing to loop through");
    }

    #[test]
    fn animations_play_through_and_pausing_keeps_the_time_spent() {
        let t0 = Instant::now();
        let mut s = Slideshow::new(2 * S, false);
        s.tick(t0, 0, 5, true, 7 * S);
        assert_eq!(s.tick(t0 + 6 * S, 0, 5, true, 7 * S), Step::Stay, "the animation is longer than the interval");
        assert_eq!(s.tick(t0 + 7 * S, 0, 5, true, 7 * S), Step::Go(1));

        let mut s = Slideshow::new(5 * S, false);
        s.tick(t0, 0, 5, true, Duration::ZERO);
        s.toggle_pause(t0 + 3 * S);
        assert!(s.is_paused());
        assert_eq!(s.wait(t0 + 4 * S, Duration::ZERO), None);
        assert_eq!(s.tick(t0 + 60 * S, 0, 5, true, Duration::ZERO), Step::Stay, "paused");
        s.toggle_pause(t0 + 60 * S);
        assert_eq!(s.tick(t0 + 61 * S, 0, 5, true, Duration::ZERO), Step::Stay, "3 s spent before the pause, 1 s since");
        assert_eq!(s.tick(t0 + 62 * S, 0, 5, true, Duration::ZERO), Step::Go(1));
    }
}
