//! Polling-based button handler for the M5StickC Plus 2.
//!
//! GPIO37 (A), GPIO39 (B), and GPIO35 (C/power) are active-low with external
//! pull-ups on the board.  Call `poll()` every [`POLL_MS`] ms from the main loop.

use esp_idf_svc::hal::gpio::{Input, InputPin, PinDriver};

/// Main-loop poll interval in milliseconds.
pub const POLL_MS: u32 = 20;

/// Long-press threshold in milliseconds.
pub const LONG_PRESS_MS: u32 = 1500;

#[allow(clippy::enum_variant_names)]
#[derive(Copy, Clone)]
pub enum ButtonEvent {
    ALongPress,
    AShortPress,
    BLongPress,
    BShortPress,
    /// Button C short press → power off.
    CShortPress,
    /// Button C (power) held ≥ 1.5 s → power off.
    CPowerLongPress,
}

/// Internal state machine for a single button's hold / long-press logic.
struct ButtonState {
    down_since: Option<std::time::Instant>,
    long_fired: bool,
}

impl ButtonState {
    const fn new() -> Self {
        Self {
            down_since: None,
            long_fired: false,
        }
    }

    /// Advance the state machine for one poll tick.
    fn poll(
        &mut self,
        is_down: bool,
        long_event: ButtonEvent,
        short_event: Option<ButtonEvent>,
    ) -> Option<ButtonEvent> {
        if is_down {
            let start = self.down_since.get_or_insert_with(std::time::Instant::now);
            if !self.long_fired && start.elapsed().as_millis() >= LONG_PRESS_MS as u128 {
                self.long_fired = true;
                return Some(long_event);
            }
        } else if let Some(_start) = self.down_since.take() {
            let fired = if !self.long_fired { short_event } else { None };
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
        // Button C: both short-press and long-press cut power.
        self.state_c.poll(
            c_down,
            ButtonEvent::CPowerLongPress,
            Some(ButtonEvent::CShortPress),
        )
    }
}
