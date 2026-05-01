//! Polling-based button handler for the M5StickC Plus 2.
//!
//! GPIO37 (A), GPIO39 (B), and GPIO35 (C/power) are active-low with external
//! pull-ups on the board.  Call `poll()` every [`POLL_MS`] ms from the main loop.

use esp_idf_svc::hal::gpio::{Input, InputPin, PinDriver};

/// Main-loop poll interval in milliseconds.
pub const POLL_MS: u32 = 20;

/// Long-press threshold: 150 polls × POLL_MS = 3 seconds.
const LONG_PRESS_POLLS: u32 = 150;

#[allow(clippy::enum_variant_names)]
#[derive(Copy, Clone)]
pub enum ButtonEvent {
    ALongPress,
    AShortPress,
    BLongPress,
    BShortPress,
    /// Button C (power) held ≥ 3 s → power off.
    CPowerLongPress,
}

/// Internal state machine for a single button's hold / long-press logic.
struct ButtonState {
    hold: u32,
    long_fired: bool,
}

impl ButtonState {
    const fn new() -> Self {
        Self {
            hold: 0,
            long_fired: false,
        }
    }

    /// Advance the state machine for one poll tick.
    ///
    /// `long_event` fires after [`LONG_PRESS_POLLS`] consecutive down ticks.
    /// `short_event` fires on release when no long event was emitted; pass
    /// `None` for buttons that have no short-press action (e.g. button C).
    fn poll(
        &mut self,
        is_down: bool,
        long_event: ButtonEvent,
        short_event: Option<ButtonEvent>,
    ) -> Option<ButtonEvent> {
        if is_down {
            self.hold += 1;
            if !self.long_fired && self.hold >= LONG_PRESS_POLLS {
                self.long_fired = true;
                return Some(long_event);
            }
        } else if self.hold > 0 {
            let fired = if !self.long_fired { short_event } else { None };
            self.hold = 0;
            self.long_fired = false;
            return fired;
        }
        None
    }
}

pub struct Buttons<'d, A: InputPin, B: InputPin, C: InputPin> {
    btn_a: PinDriver<'d, A, Input>,
    btn_b: PinDriver<'d, B, Input>,
    btn_c: PinDriver<'d, C, Input>,
    state_a: ButtonState,
    state_b: ButtonState,
    state_c: ButtonState,
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
            state_a: ButtonState::new(),
            state_b: ButtonState::new(),
            state_c: ButtonState::new(),
        }
    }

    /// Returns `true` if Button A is currently pressed (active-low).
    pub fn is_a_down(&self) -> bool {
        self.btn_a.is_low()
    }

    /// Returns `true` if any button is currently pressed.
    pub fn is_any_down(&self) -> bool {
        self.btn_a.is_low() || self.btn_b.is_low() || self.btn_c.is_low()
    }

    /// Call this every [`POLL_MS`] ms from the main loop.
    pub fn poll(&mut self) -> Option<ButtonEvent> {
        let a_down = self.btn_a.is_low();
        let b_down = self.btn_b.is_low();
        let c_down = self.btn_c.is_low();

        if let Some(e) = self.state_a.poll(
            a_down,
            ButtonEvent::ALongPress,
            Some(ButtonEvent::AShortPress),
        ) {
            return Some(e);
        }
        if let Some(e) = self.state_b.poll(
            b_down,
            ButtonEvent::BLongPress,
            Some(ButtonEvent::BShortPress),
        ) {
            return Some(e);
        }
        // Button C has no short-press action (power button).
        self.state_c
            .poll(c_down, ButtonEvent::CPowerLongPress, None)
    }
}
