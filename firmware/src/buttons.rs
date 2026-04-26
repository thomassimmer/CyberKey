//! Polling-based button handler for the M5StickC Plus 2.
//!
//! GPIO37 (A), GPIO39 (B), and GPIO35 (C/power) are active-low with external
//! pull-ups on the board.  Call `poll()` every ~20 ms from the main loop.

use esp_idf_svc::hal::gpio::{Input, InputPin, PinDriver};

/// Long-press threshold: 150 polls × 20 ms = 3 seconds.
const LONG_PRESS_POLLS: u32 = 150;

#[allow(clippy::enum_variant_names)]
pub enum ButtonEvent {
    ALongPress,
    AShortPress,
    BLongPress,
    BShortPress,
    /// Button C (power) held ≥ 3 s → power off.
    CPowerLongPress,
}

pub struct Buttons<'d, A: InputPin, B: InputPin, C: InputPin> {
    btn_a: PinDriver<'d, A, Input>,
    btn_b: PinDriver<'d, B, Input>,
    btn_c: PinDriver<'d, C, Input>,
    a_hold: u32,
    a_long_fired: bool,
    b_hold: u32,
    b_long_fired: bool,
    c_hold: u32,
    c_long_fired: bool,
}

impl<'d, A: InputPin, B: InputPin, C: InputPin> Buttons<'d, A, B, C> {
    pub fn new(
        btn_a: PinDriver<'d, A, Input>,
        btn_b: PinDriver<'d, B, Input>,
        btn_c: PinDriver<'d, C, Input>,
    ) -> Self {
        Self {
            btn_a,
            btn_b,
            btn_c,
            a_hold: 0,
            a_long_fired: false,
            b_hold: 0,
            b_long_fired: false,
            c_hold: 0,
            c_long_fired: false,
        }
    }

    /// Returns `true` if Button A is currently pressed (active-low).
    pub fn is_a_down(&self) -> bool {
        self.btn_a.is_low()
    }

    /// Call this every ~20 ms from the main loop.
    pub fn poll(&mut self) -> Option<ButtonEvent> {
        let a_down = self.btn_a.is_low();
        let b_down = self.btn_b.is_low();
        let c_down = self.btn_c.is_low();

        // --- Button A ---
        if a_down {
            self.a_hold += 1;
            if !self.a_long_fired && self.a_hold >= LONG_PRESS_POLLS {
                self.a_long_fired = true;
                return Some(ButtonEvent::ALongPress);
            }
        } else if self.a_hold > 0 {
            let fired = if !self.a_long_fired {
                Some(ButtonEvent::AShortPress)
            } else {
                None
            };
            self.a_hold = 0;
            self.a_long_fired = false;
            if fired.is_some() {
                return fired;
            }
        }

        // --- Button B ---
        if b_down {
            self.b_hold += 1;
            if !self.b_long_fired && self.b_hold >= LONG_PRESS_POLLS {
                self.b_long_fired = true;
                return Some(ButtonEvent::BLongPress);
            }
        } else if self.b_hold > 0 {
            let fired = if !self.b_long_fired {
                Some(ButtonEvent::BShortPress)
            } else {
                None
            };
            self.b_hold = 0;
            self.b_long_fired = false;
            if fired.is_some() {
                return fired;
            }
        }

        // --- Button C (power) ---
        if c_down {
            self.c_hold += 1;
            if !self.c_long_fired && self.c_hold >= LONG_PRESS_POLLS {
                self.c_long_fired = true;
                return Some(ButtonEvent::CPowerLongPress);
            }
        } else {
            self.c_hold = 0;
            self.c_long_fired = false;
        }

        None
    }
}
