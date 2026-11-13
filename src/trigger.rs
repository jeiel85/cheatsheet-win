//! The trigger rules as a pure state machine: no Win32, no clock — events and times go in, Show/Hide come
//! out. `hook.rs` feeds it from the low-level hooks; the tests below drive it through every scenario.
//!
//! - Hold: the trigger held alone for `hold_ms` shows the sheet until it is released.
//! - Double tap: two quick taps (each ≤ TAP_MAX ms, the second within TAP_GAP ms of the first release, nothing
//!   else pressed or clicked in between) show the sheet until the next key press or click. It is decided when
//!   the second tap is *released*, so a long second press is a hold, never a double tap.
//! - Any other key, any click or scroll, or moving the mouse more than MOVE_SLOP px cancels a pending hold.

pub const TAP_MAX: u64 = 300;
pub const TAP_GAP: u64 = 400;
pub const MOVE_SLOP: i32 = 16;

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum State {
    Idle,
    /// Trigger is down, waiting for `deadline` to show.
    Pending,
    /// Shown by a hold; hides on release.
    Showing,
    /// Cancelled while the trigger is still down; re-arms on release.
    Blocked,
    /// Shown by a double tap; hides on the next key press or click.
    Sticky,
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Out {
    Show,
    Hide,
}

#[derive(Clone, Copy, Debug)]
pub struct Config {
    pub hold: bool,
    pub tap: bool,
    pub hold_ms: u64,
}

#[derive(Debug)]
pub struct Machine {
    pub state: State,
    pub trigger_down: bool,
    /// Time at which `tick` has something to do (0 = nothing scheduled).
    pub deadline: u64,
    /// Whether the mouse hook is needed right now.
    pub want_mouse: bool,
    start: (i32, i32),
    tap_down_at: u64,
    tap_clean: bool,
    last_tap_up: u64,
    /// The current press started within TAP_GAP of a clean tap: if it is a clean tap too, it's a double tap.
    second: bool,
}

impl Default for Machine {
    fn default() -> Self {
        Machine { state: State::Idle, trigger_down: false, deadline: 0, want_mouse: false, start: (0, 0), tap_down_at: 0, tap_clean: false, last_tap_up: 0, second: false }
    }
}

impl Machine {
    /// Back to idle (settings changed, hook re-installed …). Returns Hide if the sheet was up.
    pub fn reset(&mut self) -> Option<Out> {
        let was_up = matches!(self.state, State::Showing | State::Sticky);
        *self = Machine::default();
        if was_up { Some(Out::Hide) } else { None }
    }

    /// A key event. `trigger` = it is the trigger key; `mouse_btn` = a mouse button is down right now.
    pub fn key(&mut self, cfg: &Config, trigger: bool, down: bool, now: u64, cursor: (i32, i32), mouse_btn: bool) -> Option<Out> {
        // Shown by a double tap: any key press closes it (releasing the second tap doesn't).
        if self.state == State::Sticky {
            if trigger && !down {
                self.trigger_down = false;
                return None;
            }
            if down {
                self.deadline = 0;
                self.want_mouse = false;
                self.last_tap_up = 0;
                if trigger {
                    self.trigger_down = true;
                    self.state = State::Blocked;
                } else {
                    self.state = State::Idle;
                }
                return Some(Out::Hide);
            }
            return None;
        }

        if trigger {
            if down {
                if self.trigger_down {
                    return None; // auto-repeat
                }
                self.trigger_down = true;
                self.tap_down_at = now;
                self.tap_clean = true;
                self.start = cursor;
                // A press right after a clean tap may become a double tap — decided on its release.
                self.second = cfg.tap && self.state == State::Idle && self.last_tap_up != 0 && now.saturating_sub(self.last_tap_up) <= TAP_GAP;
                self.last_tap_up = 0;
                if self.state == State::Idle {
                    if cfg.hold {
                        self.state = State::Pending;
                        self.deadline = now + cfg.hold_ms;
                    }
                    // Watch the mouse during a hold, and during taps (Ctrl+click is not a tap).
                    self.want_mouse = cfg.hold || cfg.tap;
                }
                return None;
            }
            // Trigger released.
            self.trigger_down = false;
            let was_showing = self.state == State::Showing;
            let tap = cfg.tap && self.tap_clean && self.state != State::Blocked && !was_showing && now.saturating_sub(self.tap_down_at) <= TAP_MAX;
            let second = std::mem::take(&mut self.second);
            if tap && second {
                // Double tap.
                self.last_tap_up = 0;
                self.deadline = 0;
                if mouse_btn {
                    self.state = State::Idle;
                    self.want_mouse = false;
                    return None;
                }
                self.state = State::Sticky;
                self.want_mouse = true;
                return Some(Out::Show);
            }
            self.state = State::Idle;
            if tap {
                self.last_tap_up = now;
                self.deadline = now + TAP_GAP; // close the tap window later
                self.want_mouse = true;
            } else {
                self.last_tap_up = 0;
                self.deadline = 0;
                self.want_mouse = false;
            }
            return if was_showing { Some(Out::Hide) } else { None };
        }

        // Another key.
        if down {
            self.tap_clean = false;
            self.last_tap_up = 0;
            if matches!(self.state, State::Pending | State::Showing) {
                // The trigger is being used as a modifier (Ctrl+C …).
                let was_showing = self.state == State::Showing;
                self.deadline = 0;
                self.want_mouse = false;
                self.state = if self.trigger_down { State::Blocked } else { State::Idle };
                return if was_showing { Some(Out::Hide) } else { None };
            }
            if self.state == State::Idle && !self.trigger_down {
                self.deadline = 0;
                self.want_mouse = false;
            }
        }
        None
    }

    /// A mouse event: `is_move` for plain movement, otherwise a button or the wheel.
    pub fn mouse(&mut self, is_move: bool, pos: (i32, i32)) -> Option<Out> {
        if !is_move {
            self.tap_clean = false;
            self.last_tap_up = 0;
        }
        match self.state {
            State::Sticky if !is_move => {
                self.state = if self.trigger_down { State::Blocked } else { State::Idle };
                self.want_mouse = false;
                self.deadline = 0;
                Some(Out::Hide)
            }
            State::Pending | State::Showing => {
                let far = (pos.0 - self.start.0).abs() > MOVE_SLOP || (pos.1 - self.start.1).abs() > MOVE_SLOP;
                if !is_move || (self.state == State::Pending && far) {
                    let was_showing = self.state == State::Showing;
                    self.deadline = 0;
                    self.want_mouse = false;
                    self.state = State::Blocked;
                    return if was_showing { Some(Out::Hide) } else { None };
                }
                None
            }
            State::Idle if !is_move && !self.trigger_down => {
                // Tap window ruined by a click: nothing more to watch.
                self.deadline = 0;
                self.want_mouse = false;
                None
            }
            _ => None,
        }
    }

    /// Called when `deadline` is reached. `mouse_btn` = a mouse button is down right now.
    pub fn tick(&mut self, now: u64, mouse_btn: bool) -> Option<Out> {
        if self.deadline == 0 || now < self.deadline {
            return None;
        }
        self.deadline = 0;
        if self.state == State::Pending && self.trigger_down {
            // A drag with Ctrl held (copy-drag) starts with the button already down: not a request.
            if mouse_btn {
                self.state = State::Blocked;
                self.want_mouse = false;
                return None;
            }
            self.state = State::Showing;
            return Some(Out::Show);
        }
        if self.state == State::Idle && !self.trigger_down {
            // Tap window closed without a second tap.
            self.last_tap_up = 0;
            self.want_mouse = false;
        }
        None
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const BOTH: Config = Config { hold: true, tap: true, hold_ms: 900 };
    const HOLD: Config = Config { hold: true, tap: false, hold_ms: 900 };
    const DOUBLE: Config = Config { hold: false, tap: true, hold_ms: 900 };
    const P: (i32, i32) = (100, 100);

    /// Runs a script of (time, event) and returns every output with its time.
    #[derive(Clone, Copy)]
    enum E {
        Trig(bool),
        Other(bool),
        Click,
        Move(i32, i32),
        Tick,
    }

    fn run(cfg: Config, script: &[(u64, E)]) -> (Machine, Vec<(u64, Out)>) {
        let mut m = Machine::default();
        let mut outs = Vec::new();
        for &(t, e) in script {
            // Fire any due deadline first, like the timer would.
            if m.deadline != 0 && t >= m.deadline {
                let d = m.deadline;
                if let Some(o) = m.tick(d, false) {
                    outs.push((d, o));
                }
            }
            let o = match e {
                E::Trig(d) => m.key(&cfg, true, d, t, P, false),
                E::Other(d) => m.key(&cfg, false, d, t, P, false),
                E::Click => m.mouse(false, P),
                E::Move(x, y) => m.mouse(true, (x, y)),
                E::Tick => m.tick(t, false),
            };
            if let Some(o) = o {
                outs.push((t, o));
            }
        }
        (m, outs)
    }

    use E::*;

    #[test]
    fn hold_shows_and_release_hides() {
        let (m, o) = run(BOTH, &[(0, Trig(true)), (950, Tick), (1200, Trig(false))]);
        assert_eq!(o, vec![(900, Out::Show), (1200, Out::Hide)]);
        assert_eq!(m.state, State::Idle);
    }

    #[test]
    fn short_press_shows_nothing() {
        let (_, o) = run(HOLD, &[(0, Trig(true)), (300, Trig(false)), (2000, Tick)]);
        assert!(o.is_empty());
    }

    #[test]
    fn ctrl_c_cancels_hold_until_release() {
        let (_, o) = run(BOTH, &[(0, Trig(true)), (200, Other(true)), (250, Other(false)), (1500, Tick), (1600, Trig(false)), (2000, Trig(true)), (3000, Tick)]);
        assert_eq!(o, vec![(2900, Out::Show)]);
    }

    #[test]
    fn mouse_move_or_click_cancels_hold() {
        let (_, o) = run(HOLD, &[(0, Trig(true)), (200, Move(140, 100)), (1500, Tick)]);
        assert!(o.is_empty());
        let (_, o) = run(HOLD, &[(0, Trig(true)), (200, Move(105, 103)), (1000, Tick)]);
        assert_eq!(o, vec![(900, Out::Show)], "small jitter must not cancel");
        let (_, o) = run(HOLD, &[(0, Trig(true)), (200, Click), (1500, Tick)]);
        assert!(o.is_empty());
    }

    #[test]
    fn double_tap_shows_sticky_until_next_key() {
        let (m, o) = run(BOTH, &[(0, Trig(true)), (80, Trig(false)), (230, Trig(true)), (310, Trig(false)), (2000, Tick), (3000, Other(true))]);
        assert_eq!(o, vec![(310, Out::Show), (3000, Out::Hide)]);
        assert_eq!(m.state, State::Idle);
    }

    #[test]
    fn double_tap_closed_by_click_or_trigger() {
        let (_, o) = run(DOUBLE, &[(0, Trig(true)), (80, Trig(false)), (230, Trig(true)), (310, Trig(false)), (1000, Click)]);
        assert_eq!(o, vec![(310, Out::Show), (1000, Out::Hide)]);
        let (m, o) = run(DOUBLE, &[(0, Trig(true)), (80, Trig(false)), (230, Trig(true)), (310, Trig(false)), (1000, Trig(true))]);
        assert_eq!(o, vec![(310, Out::Show), (1000, Out::Hide)]);
        assert_eq!(m.state, State::Blocked, "the closing press must not start a new tap or hold");
    }

    #[test]
    fn mouse_movement_does_not_close_sticky() {
        let (m, o) = run(BOTH, &[(0, Trig(true)), (80, Trig(false)), (230, Trig(true)), (310, Trig(false)), (500, Move(400, 400))]);
        assert_eq!(o, vec![(310, Out::Show)]);
        assert_eq!(m.state, State::Sticky);
    }

    #[test]
    fn slow_taps_do_nothing() {
        let (_, o) = run(BOTH, &[(0, Trig(true)), (80, Trig(false)), (730, Trig(true)), (810, Trig(false)), (3000, Tick)]);
        assert!(o.is_empty());
    }

    #[test]
    fn long_first_press_is_not_a_tap() {
        let (_, o) = run(DOUBLE, &[(0, Trig(true)), (450, Trig(false)), (600, Trig(true)), (680, Trig(false))]);
        assert!(o.is_empty());
    }

    #[test]
    fn ctrl_click_twice_is_not_a_double_tap() {
        // Multi-select: Ctrl+click, Ctrl+click.
        let (_, o) = run(BOTH, &[(0, Trig(true)), (40, Click), (90, Trig(false)), (200, Trig(true)), (240, Click), (290, Trig(false)), (2000, Tick)]);
        assert!(o.is_empty());
    }

    #[test]
    fn click_between_taps_cancels() {
        let (_, o) = run(DOUBLE, &[(0, Trig(true)), (80, Trig(false)), (150, Click), (230, Trig(true)), (310, Trig(false))]);
        assert!(o.is_empty());
    }

    #[test]
    fn ctrl_shift_then_tap_is_not_a_double_tap() {
        let (_, o) = run(BOTH, &[(0, Trig(true)), (40, Other(true)), (60, Other(false)), (90, Trig(false)), (200, Trig(true)), (280, Trig(false)), (2000, Tick)]);
        assert!(o.is_empty());
    }

    #[test]
    fn hold_only_ignores_double_tap() {
        let (_, o) = run(HOLD, &[(0, Trig(true)), (80, Trig(false)), (230, Trig(true)), (310, Trig(false)), (2000, Tick)]);
        assert!(o.is_empty());
    }

    #[test]
    fn double_only_ignores_hold() {
        let (m, o) = run(DOUBLE, &[(0, Trig(true)), (1500, Tick), (1600, Trig(false))]);
        assert!(o.is_empty());
        assert_eq!(m.state, State::Idle);
    }

    #[test]
    fn mouse_hook_is_released_after_tap_window() {
        let (m, _) = run(DOUBLE, &[(0, Trig(true)), (80, Trig(false)), (600, Tick)]);
        assert!(!m.want_mouse);
        assert_eq!(m.deadline, 0);
    }

    #[test]
    fn tap_then_long_press_is_not_a_double_tap() {
        // double-only: a long second press shows nothing and leaves nothing open
        let (m, o) = run(DOUBLE, &[(0, Trig(true)), (80, Trig(false)), (230, Trig(true)), (1500, Trig(false)), (2000, Tick)]);
        assert!(o.is_empty());
        assert_eq!(m.state, State::Idle);
        // both: a long second press is a normal hold — shows, and hides on release
        let (m, o) = run(BOTH, &[(0, Trig(true)), (80, Trig(false)), (230, Trig(true)), (1200, Tick), (1500, Trig(false))]);
        assert_eq!(o, vec![(1130, Out::Show), (1500, Out::Hide)]);
        assert_eq!(m.state, State::Idle);
    }

    #[test]
    fn tap_then_shortcut_shows_nothing() {
        // Ctrl tap, then Ctrl+C right after: no flash of the sheet
        let (_, o) = run(BOTH, &[(0, Trig(true)), (80, Trig(false)), (230, Trig(true)), (260, Other(true)), (280, Other(false)), (310, Trig(false)), (2000, Tick)]);
        assert!(o.is_empty());
    }

    #[test]
    fn hold_after_shortcut_and_tap_still_works() {
        // The exact live-test sequence: Ctrl+Shift, a Ctrl tap, a pause, then a hold.
        let (_, o) = run(BOTH, &[(0, Trig(true)), (20, Other(true)), (70, Other(false)), (100, Trig(false)), (200, Trig(true)), (280, Trig(false)), (1080, Trig(true)), (2400, Tick), (2500, Trig(false))]);
        assert_eq!(o, vec![(1980, Out::Show), (2500, Out::Hide)]);
    }

    #[test]
    fn third_tap_after_close_starts_fresh() {
        // double tap → sticky → closed by trigger press → release → a new double tap works again
        let (_, o) = run(
            BOTH,
            &[(0, Trig(true)), (80, Trig(false)), (230, Trig(true)), (310, Trig(false)), (1000, Trig(true)), (1080, Trig(false)), (1600, Tick), (2000, Trig(true)), (2080, Trig(false)), (2200, Trig(true)), (2280, Trig(false))],
        );
        assert_eq!(o, vec![(310, Out::Show), (1000, Out::Hide), (2280, Out::Show)]);
    }
}
