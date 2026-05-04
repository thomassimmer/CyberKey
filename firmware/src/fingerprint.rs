//! Fingerprint sensor integration — M5Stack Unit Fingerprint2 (U203) via Grove.
//!
//! Grove port pin assignment (M5StickC Plus 2):
//!   GPIO32 = Yellow = TX (MCU → sensor)
//!   GPIO33 = White  = RX (sensor → MCU)
//! Source: Grove UART convention (Pin1=TX, Pin2=RX from host perspective)
//! confirmed against M5StickC Plus 2 docs (docs.m5stack.com).
//!
//! # Enrollment flow
//!
//! Enrollment is driven by a manual state machine rather than the sensor's
//! high-level `PS_AUTO_ENROLL` command. This gives us the ability to perform
//! a duplicate-finger check after the very first capture without any extra
//! finger placement:
//!
//! ```text
//! Pass 1 : GET_ENROLL_IMAGE → GEN_CHAR(buf=1) → SEARCH(buf=1) ← duplicate check
//! Pass 2 : GET_ENROLL_IMAGE → GEN_CHAR(buf=2)
//! Pass 3 : GET_ENROLL_IMAGE → GEN_CHAR(buf=1)  ← overwrites buf=1 with better quality
//! Final  : REG_MODEL → STORE_CHAR(buf=1, slot)
//! ```
//!
//! The buffer index alternates with each pass (`buf = ((pass - 1) % 2) + 1`), so
//! odd passes write to CharBuffer 1 and even passes to CharBuffer 2. `REG_MODEL`
//! merges the two buffers into a consolidated template stored in CharBuffer 1,
//! which `STORE_CHAR` then writes to the sensor's flash library.
//!
//! From the caller's perspective (`app.rs`), the ACK sequence is identical to
//! the old `PS_AUTO_ENROLL` stream:
//!   `StartCapture` → `ImageOk` → `LiftOk`   (×N passes)
//!   → `Done`
//! with the addition of `DuplicateFinger` which can fire instead of `ImageOk`
//! on the very first pass.

use esp_idf_svc::hal::{
    delay::FreeRtos,
    gpio::AnyIOPin,
    peripheral::Peripheral,
    uart::{config::Config as UartConfig, Uart, UartDriver},
    units::Hertz,
};
use fingerprint2_rs::{
    commands::{LedColor, LedMode},
    DriverEvent, Fingerprint2Driver, FingerprintError,
};

const BAUD: u32 = 115_200;
const SECURITY_LEVEL: u8 = 3;

/// Total number of fingerprint slots in the sensor's template library.
const LIBRARY_SIZE: u16 = 10;

// ---------------------------------------------------------------------------
// Public types
// ---------------------------------------------------------------------------

pub enum IdentifyResult {
    Match(u16),
    NoMatch,
}

/// Progress event returned by [`FingerprintSensor::poll_enroll_ack`].
///
/// The caller receives one event per [`FingerprintSensor::poll_enroll_ack`] call.
/// Events arrive in the following order for a successful N-pass enrollment:
///
/// ```text
/// StartCapture                   ← sensor ready, waiting for finger (×N)
/// ImageOk                        ← finger captured, safe to lift     (×N)
/// LiftOk                         ← finger removed, next pass ready   (×N)
/// Done                           ← template stored in flash
/// ```
///
/// `DuplicateFinger` replaces the first `ImageOk` when the captured finger
/// already exists in the library.
pub enum EnrollAck {
    /// No event yet — call again after a short delay.
    Pending,
    /// Sensor is ready and waiting for the user to place their finger.
    ///
    /// Emitted once at the start of each capture pass.
    StartCapture,
    /// Finger captured successfully — safe to lift.
    ///
    /// On pass 1 this is only emitted after a successful duplicate check
    /// (i.e. the finger is not already enrolled).
    ImageOk,
    /// Finger lift detected — ready for the next pass.
    LiftOk,
    /// All passes complete and the template has been stored in flash.
    Done,
    /// The captured finger is already enrolled in the library.
    ///
    /// Enrollment is aborted; the caller should inform the user.
    DuplicateFinger,
    /// An unrecoverable sensor error occurred.
    Failed,
}

// ---------------------------------------------------------------------------
// Private enrollment state machine
// ---------------------------------------------------------------------------

/// Internal phase within a single capture pass.
#[derive(Clone, Copy)]
enum ManualEnrollPhase {
    /// Waiting for the user to place their finger on the pad.
    ///
    /// `announced` tracks whether `StartCapture` has already been emitted for
    /// this pass; it is `false` at the start of every new pass.
    WaitingForFinger { announced: bool },
    /// Finger captured — waiting for the user to lift it.
    WaitingForLift,
    /// All passes done — running `REG_MODEL` + `STORE_CHAR` to finalise.
    Finalizing,
}

/// State held for the duration of an enrollment session.
struct EnrollSession {
    /// Target slot in the sensor's flash library (0–9).
    slot: u8,
    /// Total number of capture passes requested (typically 3).
    passes: u8,
    /// Current pass index, 1-based.
    current_pass: u8,
    phase: ManualEnrollPhase,
    /// Deadline for retrying SensorError(1) in WaitingForFinger — sensor may need
    /// time to settle after a mode switch or smart-poll sequence.
    /// Set at session creation so the budget is measured from begin_enroll, not
    /// from the first failure (each get_enroll_image() call can itself block ~5s).
    imaging_fail_deadline: std::time::Instant,
}

impl EnrollSession {
    /// Character buffer index for the current pass.
    ///
    /// Odd passes → CharBuffer 1, even passes → CharBuffer 2.
    fn buf(&self) -> u8 {
        if self.current_pass % 2 == 1 {
            1
        } else {
            2
        }
    }
}

// ---------------------------------------------------------------------------
// FingerprintSensor
// ---------------------------------------------------------------------------

pub struct FingerprintSensor<'d> {
    driver: Fingerprint2Driver<UartDriver<'d>, FreeRtos>,
    ready: bool,
    smart_poll_until: Option<std::time::Instant>,
    /// Active enrollment session, if one is in progress.
    enroll_session: Option<EnrollSession>,
}

impl<'d> FingerprintSensor<'d> {
    pub fn new(
        uart: impl Peripheral<P = impl Uart> + 'd,
        tx: impl Peripheral<P = impl esp_idf_svc::hal::gpio::OutputPin> + 'd,
        rx: impl Peripheral<P = impl esp_idf_svc::hal::gpio::InputPin> + 'd,
    ) -> anyhow::Result<Self> {
        let config = UartConfig::new().baudrate(Hertz(BAUD));
        let uart_driver = UartDriver::new(
            uart,
            tx,
            rx,
            Option::<AnyIOPin>::None,
            Option::<AnyIOPin>::None,
            &config,
        )?;
        Ok(Self {
            driver: Fingerprint2Driver::new(uart_driver, FreeRtos),
            ready: false,
            smart_poll_until: None,
            enroll_session: None,
        })
    }

    /// Drain pending RX bytes, send `activate`, and wait 100 ms for the sensor's
    /// UART to settle before sending the next command.
    ///
    /// Must be called before any command sequence that follows an idle/sleep
    /// period, or after the sensor has returned to autonomous wakeup mode.
    fn reactivate_sensor(&mut self, ctx: &str) {
        // Safety: drain_rx is safe to call as long as self.driver.uart is valid.
        // If the UART handle has been moved or dropped elsewhere, this will panic.
        self.driver.drain_rx();
        if let Err(e) = self.driver.activate() {
            log::warn!("{ctx}: re-activate error: {:?}", e);
        }
        FreeRtos::delay_ms(100);
    }

    /// Initialise the sensor: drain RX → activate → handshake.
    pub fn init(&mut self) -> bool {
        self.reactivate_sensor("init");
        // Ensure we start in Active Mode (1) and LEDs are enabled.
        let _ = self.driver.set_work_mode(1);
        log::info!("Fingerprint: handshake...");
        match self.driver.handshake() {
            Ok(()) => {
                self.ready = true;
                // Sensor is awake and listening on UART — arm smart poll immediately
                // so fingers are detected right away instead of waiting for autonomous sleep/wakeup cycle (~10s).
                self.smart_poll_until =
                    Some(std::time::Instant::now() + std::time::Duration::from_secs(10));
                log::info!("Fingerprint: sensor online, smart poll armed for 10s");
                true
            }
            Err(e) => {
                log::warn!("Fingerprint: handshake error = {:?}", e);
                false
            }
        }
    }

    /// Initialise a manual enrollment session for `slot` with `passes` capture rounds.
    ///
    /// Returns `false` if the sensor is not ready. On success, drive the session by
    /// calling [`poll_enroll_ack`](Self::poll_enroll_ack) in a loop until `Done`,
    /// `DuplicateFinger`, or `Failed` is returned.
    pub fn begin_enroll(&mut self, slot: u8, passes: u8) -> bool {
        if !self.ready {
            return false;
        }
        // Use set_work_mode(1) rather than activate() — activate() is a cold-boot
        // command that can briefly destabilise the sensor if called while already
        // active. Either way the sensor may return SensorError(1) for a few cycles
        // after the mode switch; poll_enroll_ack() retries through those.
        self.driver.drain_rx();
        let _ = self.driver.set_work_mode(1);
        FreeRtos::delay_ms(100);
        log::info!("begin_enroll slot={} passes={}", slot, passes);
        self.enroll_session = Some(EnrollSession {
            slot,
            passes,
            current_pass: 1,
            phase: ManualEnrollPhase::WaitingForFinger { announced: false },
            imaging_fail_deadline: std::time::Instant::now() + std::time::Duration::from_secs(6),
        });
        true
    }

    /// Advance the enrollment state machine by one step.
    ///
    /// Call this in a polling loop (with a short delay between calls so the
    /// FreeRTOS scheduler can run other tasks). The state machine progresses
    /// through the following phases for each pass:
    ///
    /// 1. **WaitingForFinger** — polls `PS_GET_ENROLL_IMAGE` until a finger is
    ///    detected. Emits `StartCapture` once per pass to notify the caller,
    ///    then `Pending` until a finger arrives. When a finger is captured,
    ///    runs `PS_GEN_CHAR` to extract features into the appropriate
    ///    CharBuffer. On pass 1 only, runs `PS_SEARCH` for duplicate detection
    ///    before emitting `ImageOk`.
    ///
    /// 2. **WaitingForLift** — polls `PS_GET_ENROLL_IMAGE` until the finger is
    ///    removed (`SensorError(2)`). Emits `LiftOk` and advances to the next
    ///    pass, or transitions to `Finalizing` after the last pass.
    ///
    /// 3. **Finalizing** — runs `PS_REG_MODEL` (merge CharBuffer 1 + 2) then
    ///    `PS_STORE_CHAR` (write to flash). Emits `Done` on success.
    pub fn poll_enroll_ack(&mut self) -> EnrollAck {
        let session = match self.enroll_session.as_mut() {
            None => return EnrollAck::Failed,
            Some(s) => s,
        };

        match session.phase {
            // ------------------------------------------------------------------
            // Phase 1: waiting for the user to place their finger
            // ------------------------------------------------------------------
            ManualEnrollPhase::WaitingForFinger { announced } => {
                // Emit StartCapture once per pass so the caller can notify the CLI.
                if !announced {
                    session.phase = ManualEnrollPhase::WaitingForFinger { announced: true };
                    return EnrollAck::StartCapture;
                }

                match self.driver.get_enroll_image() {
                    // No finger yet — keep waiting.
                    Err(FingerprintError::SensorError(2)) => EnrollAck::Pending,

                    // Sensor unresponsive — abort.
                    Err(FingerprintError::Timeout) => {
                        log::warn!(
                            "enroll: get_enroll_image timeout on pass {}",
                            session.current_pass
                        );
                        self.enroll_session = None;
                        EnrollAck::Failed
                    }

                    // SensorError(1) = "imaging fail" — transient; the sensor may still
                    // be settling after a mode switch or prior smart-poll sequence.
                    // Retry until the session deadline (6 s from begin_enroll) since
                    // get_enroll_image() can itself block for several seconds per call.
                    Err(FingerprintError::SensorError(1)) => {
                        if std::time::Instant::now() > session.imaging_fail_deadline {
                            log::warn!(
                                "enroll: SensorError(1) persists on pass {} — aborting",
                                session.current_pass
                            );
                            self.enroll_session = None;
                            EnrollAck::Failed
                        } else {
                            EnrollAck::Pending
                        }
                    }

                    // Any other sensor error — abort.
                    Err(e) => {
                        log::warn!(
                            "enroll: get_enroll_image error on pass {}: {:?}",
                            session.current_pass,
                            e
                        );
                        self.enroll_session = None;
                        EnrollAck::Failed
                    }

                    // Finger detected — extract features into the appropriate CharBuffer.
                    Ok(()) => {
                        let buf = session.buf();
                        let pass = session.current_pass;
                        log::info!("enroll: pass {} finger ok, gen_char buf={}", pass, buf);

                        if let Err(e) = self.driver.gen_char(buf) {
                            log::warn!("enroll: gen_char error on pass {}: {:?}", pass, e);
                            self.enroll_session = None;
                            return EnrollAck::Failed;
                        }

                        // Pass 1 only: search CharBuffer 1 against the full library to
                        // detect duplicates before committing to the enrollment sequence.
                        if pass == 1 {
                            match self.driver.search(1, 0, LIBRARY_SIZE) {
                                Ok((matched_slot, score))
                                    if matched_slot != session.slot as u16 =>
                                {
                                    log::info!(
                                        "enroll: duplicate detected — finger already at slot={} score={}",
                                        matched_slot, score
                                    );
                                    self.enroll_session = None;
                                    return EnrollAck::DuplicateFinger;
                                }
                                Ok(_) => {
                                    // Match on the target slot itself (shouldn't happen for a
                                    // new enrollment, but harmless — proceed normally).
                                }
                                Err(FingerprintError::NoMatch) => {
                                    // No duplicate found — proceed.
                                }
                                Err(e) => {
                                    log::warn!(
                                        "enroll: search error during duplicate check: {:?}",
                                        e
                                    );
                                    self.enroll_session = None;
                                    return EnrollAck::Failed;
                                }
                            }
                        }

                        session.phase = ManualEnrollPhase::WaitingForLift;
                        EnrollAck::ImageOk
                    }
                }
            }

            // ------------------------------------------------------------------
            // Phase 2: waiting for the user to lift their finger
            // ------------------------------------------------------------------
            ManualEnrollPhase::WaitingForLift => {
                match self.driver.get_enroll_image() {
                    // No finger — lift confirmed.
                    Err(FingerprintError::SensorError(2)) => {
                        let pass = session.current_pass;
                        let passes = session.passes;
                        log::info!("enroll: pass {} lift ok", pass);

                        if pass == passes {
                            // Last pass done — move to finalisation.
                            session.phase = ManualEnrollPhase::Finalizing;
                        } else {
                            session.current_pass += 1;
                            session.phase =
                                ManualEnrollPhase::WaitingForFinger { announced: false };
                        }
                        EnrollAck::LiftOk
                    }

                    // Finger still present, or transient read error — keep waiting.
                    _ => EnrollAck::Pending,
                }
            }

            // ------------------------------------------------------------------
            // Phase 3: merge + store the final template
            // ------------------------------------------------------------------
            ManualEnrollPhase::Finalizing => {
                let slot = session.slot;
                log::info!("enroll: finalizing — reg_model + store_char slot={}", slot);

                if let Err(e) = self.driver.reg_model() {
                    log::warn!("enroll: reg_model error: {:?}", e);
                    self.enroll_session = None;
                    return EnrollAck::Failed;
                }

                if let Err(e) = self.driver.store_char(1, slot as u16) {
                    log::warn!("enroll: store_char error: {:?}", e);
                    self.enroll_session = None;
                    return EnrollAck::Failed;
                }

                self.enroll_session = None;
                EnrollAck::Done
            }
        }
    }

    /// Erase the entire template library on the sensor (`PS_Empty`).
    ///
    /// Returns `true` on success. Safe to call even if the sensor is in an unknown state.
    pub fn empty_template_library(&mut self) -> bool {
        if !self.ready {
            return false;
        }
        self.reactivate_sensor("empty_template_library");
        match self.driver.empty_template_library() {
            Ok(()) => {
                log::info!("Fingerprint: template library cleared");
                true
            }
            Err(e) => {
                log::warn!("Fingerprint: empty_template_library error: {:?}", e);
                false
            }
        }
    }

    /// Delete one or more stored templates from the sensor.
    ///
    /// Returns `true` on success.
    /// NOTE: Does NOT call reactivate_sensor() — assumes the sensor is already awake.
    pub fn delete_template(&mut self, page_id: u8, count: u16) -> bool {
        if !self.ready {
            return false;
        }
        match self.driver.delete_template(page_id as u16, count) {
            Ok(()) => true,
            Err(e) => {
                log::warn!(
                    "fp: delete_template error slot={} count={} -> {:?}",
                    page_id,
                    count,
                    e
                );
                false
            }
        }
    }

    /// Re-arm the sensor for autonomous finger detection.
    ///
    /// Call after enrollment or after any sequence that leaves the sensor in
    /// a non-autonomous state. Mirrors what `begin_enroll` does up-front.
    pub fn reactivate(&mut self) {
        self.driver.drain_rx();
        // Ensure we are in Active Mode (1) before starting the smart poll window.
        let _ = self.driver.set_work_mode(1);
        // Skip activate() (which keeps the sensor awake but deaf for ~10s) and go straight
        // to a smart-poll window so a freshly enrolled finger can be tested immediately.
        self.smart_poll_until =
            Some(std::time::Instant::now() + std::time::Duration::from_secs(10));
        log::info!("fp: reactivate -> smart poll armed for 10s after enrollment");
    }

    /// Put the sensor into a low-power standby state.
    ///
    /// Enables "Timed Sleep" mode (automatic sleep after 10s of inactivity).
    pub fn standby(&mut self) {
        if !self.ready {
            return;
        }
        log::info!("fp: entering standby");
        self.smart_poll_until = None;

        self.driver.drain_rx();
        // Turn off the LED ring before sleeping to avoid unnecessary current draw.
        let _ = self.driver.set_led(LedMode::Off, LedColor::Off, 0);
        let _ = self.driver.set_work_mode(0); // 0 = Timed Sleep
        let _ = self.driver.set_sleep_time(10);
    }

    /// Wake the sensor from standby or refresh the active polling window.
    pub fn wake(&mut self) {
        if !self.ready {
            return;
        }

        // If already polling and we have more than 5 seconds left, just keep going
        // to avoid redundant UART traffic on every button press.
        if let Some(until) = self.smart_poll_until {
            if until > std::time::Instant::now() + std::time::Duration::from_secs(5) {
                return;
            }
        }

        log::info!("fp: waking up / refreshing poll window");
        self.driver.drain_rx();

        // Switch back to "Active Mode" (always-on, ready for commands).
        let _ = self.driver.set_work_mode(1);
        // Restore default idle LED (Blue breathing).
        let _ = self.driver.set_led(LedMode::Breathing, LedColor::Blue, 0);

        self.smart_poll_until =
            Some(std::time::Instant::now() + std::time::Duration::from_secs(30));
    }

    fn execute_auto_identify(&mut self, via: &str) -> Option<IdentifyResult> {
        self.driver.drain_rx();

        FreeRtos::delay_ms(50);

        log::info!("fp: [{via}] sending auto_identify...");
        let result = match self.driver.auto_identify(SECURITY_LEVEL) {
            Ok((id, score)) => {
                log::info!("fp: [{via}] match id={} score={}", id, score);
                Some(IdentifyResult::Match(id))
            }
            Err(FingerprintError::NoMatch) => {
                log::info!("fp: [{via}] no match");
                Some(IdentifyResult::NoMatch)
            }
            Err(e) => {
                log::warn!("fp: [{via}] identify error {:?}", e);
                None
            }
        };

        // The sensor drives its result LED (green/red) autonomously; no need to force it off.
        // main.rs delays 2 s after this call, so the LED stays visible long enough.

        // Arm smart polling for the next 30 seconds
        self.smart_poll_until =
            Some(std::time::Instant::now() + std::time::Duration::from_secs(30));
        log::info!("fp: smart poll window start (30s)");

        result
    }

    /// Non-blocking poll. Returns Some(result) when a finger was placed and identified.
    ///
    /// Returns None immediately when no finger is on the pad (WouldBlock).
    pub fn poll(&mut self) -> Option<IdentifyResult> {
        if !self.ready {
            return None;
        }

        // 1. SMART POLLING WINDOW (if active)
        if let Some(until) = self.smart_poll_until {
            if std::time::Instant::now() > until {
                // Window expired — transition to Timed Sleep (mode 0) so the sensor
                // can eventually sleep and trigger a Wakeup event on touch.
                self.smart_poll_until = None;
                log::info!("fp: smart poll window expired, entering Timed Sleep");
                let _ = self.driver.set_work_mode(0);
                let _ = self.driver.set_sleep_time(10);
            } else {
                self.driver.drain_rx();
                match self.driver.get_image() {
                    Ok(()) => {
                        log::info!("fp: smart poll -> finger detected");
                        return self.execute_auto_identify("smart_poll");
                    }
                    Err(FingerprintError::SensorError(2)) => {
                        // No finger — keep polling silently
                        return None;
                    }
                    Err(e) => {
                        log::warn!("fp: Smart Polling err {:?}", e);
                        return None;
                    }
                }
            }
        }

        // 2. AUTONOMOUS WAKEUP
        match self.driver.poll_event() {
            Ok(DriverEvent::Wakeup) => {
                log::info!("fp: autonomous wakeup");
                FreeRtos::delay_ms(100);
                self.execute_auto_identify("wakeup")
            }
            Ok(DriverEvent::Ack { confirm, .. }) => {
                log::info!("fp: unsolicited ack confirm=0x{:02X}", confirm);
                None
            }
            Err(nb::Error::WouldBlock) => None,
            Err(nb::Error::Other(e)) => {
                log::warn!("fp poll error: {:?}", e);
                None
            }
        }
    }
}
