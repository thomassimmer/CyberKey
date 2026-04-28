//! Fingerprint sensor integration — M5Stack Unit Fingerprint2 (U203) via Grove.
//!
//! Grove port pin assignment (M5StickC Plus 2):
//!   GPIO32 = Yellow = TX (MCU → sensor)
//!   GPIO33 = White  = RX (sensor → MCU)
//! Source: Grove UART convention (Pin1=TX, Pin2=RX from host perspective)
//! confirmed against M5StickC Plus 2 docs (docs.m5stack.com).

use esp_idf_svc::hal::{
    delay::FreeRtos,
    gpio::AnyIOPin,
    peripheral::Peripheral,
    uart::{config::Config as UartConfig, Uart, UartDriver},
    units::Hertz,
};
use fingerprint2_rs::{
    commands::AutoEnrollFlags, DriverEvent, Fingerprint2Driver, FingerprintError,
};

const BAUD: u32 = 115_200;
const SECURITY_LEVEL: u8 = 3;

pub enum IdentifyResult {
    Match(u16),
    NoMatch,
}

/// Progress event returned by [`FingerprintSensor::poll_enroll_ack`].
pub enum EnrollAck {
    /// No data available yet, or an intermediate stage (GET_IMAGE, GEN_CHAR…).
    Pending,
    /// CHECK_LIFT (stage 0x03): one capture is complete — show "lift finger" UI.
    CaptureOk,
    /// STORE_TEMPLATE (stage 0x06): all captures merged and stored — done.
    Done,
    /// Sensor returned a non-zero confirm code — enrollment failed.
    Failed,
}

pub struct FingerprintSensor<'d> {
    driver: Fingerprint2Driver<UartDriver<'d>, FreeRtos>,
    ready: bool,
    smart_poll_until: Option<std::time::Instant>,
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
        })
    }

    /// Initialise the sensor: drain RX → activate → handshake.
    pub fn init(&mut self) -> bool {
        self.driver.drain_rx();
        log::info!("Fingerprint: activating...");
        match self.driver.activate() {
            Ok(()) => log::info!("Fingerprint: activate OK"),
            Err(e) => log::warn!("Fingerprint: activate = {:?} (continuing)", e),
        }
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

    /// Begin enrollment on `slot` with `count` capture passes.
    /// Returns false if the sensor is not ready or the command fails.
    /// Call `poll_enroll_ack` once per pass to track progress.
    pub fn begin_enroll(&mut self, slot: u16, count: u8) -> bool {
        if !self.ready {
            return false;
        }
        // The sensor drifts back to autonomous wakeup mode between operations.
        // Re-activate before enrollment or it immediately rejects the command (0xFE).
        self.driver.drain_rx();
        if let Err(e) = self.driver.activate() {
            log::warn!("begin_enroll: re-activate error: {:?}", e);
        }

        // Give the sensor's UART time to recover before sending the next command.
        // Without this, the sensor often drops bytes of the auto-enroll command
        // and returns SensorError(1) "Error when receiving data package".
        FreeRtos::delay_ms(100);

        log::info!("begin_enroll slot={} count={}", slot, count);
        self.driver
            .begin_auto_enroll(
                slot,
                count,
                AutoEnrollFlags {
                    allow_overwrite: true,
                },
            )
            .map_err(|e| log::warn!("begin_enroll error: {:?}", e))
            .is_ok()
    }

    /// Non-blocking poll for one enrollment ACK.
    ///
    /// Each capture produces three intermediate ACKs (GET_IMAGE → GEN_CHAR →
    /// CHECK_LIFT). Only CHECK_LIFT and STORE_TEMPLATE are meaningful to the
    /// caller; everything else is `Pending`.
    pub fn poll_enroll_ack(&mut self) -> EnrollAck {
        match self.driver.poll_event() {
            Err(nb::Error::WouldBlock) => EnrollAck::Pending,
            Err(nb::Error::Other(e)) => {
                log::warn!("enroll poll error: {:?}", e);
                EnrollAck::Failed
            }
            Ok(DriverEvent::Wakeup) => {
                log::info!("enroll: unexpected wakeup");
                EnrollAck::Pending
            }
            Ok(DriverEvent::Ack { confirm: 0, data }) => {
                let stage = data.get(1).copied().unwrap_or(0);
                log::info!(
                    "enroll ack stage=0x{:02X} data={:02X?}",
                    stage,
                    data.as_slice()
                );
                match stage {
                    0x03 => EnrollAck::CaptureOk,
                    0x06 => EnrollAck::Done,
                    _ => EnrollAck::Pending,
                }
            }
            Ok(DriverEvent::Ack { confirm, data }) => {
                log::warn!(
                    "enroll ack error confirm=0x{:02X} data={:02X?}",
                    confirm,
                    data.as_slice()
                );
                EnrollAck::Failed
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
        self.driver.drain_rx();
        if let Err(e) = self.driver.activate() {
            log::warn!("empty_template_library: re-activate error: {:?}", e);
        }
        FreeRtos::delay_ms(100);
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

    /// Re-arm the sensor for autonomous finger detection.
    ///
    /// Call after enrollment or after any sequence that leaves the sensor in
    /// a non-autonomous state. Mirrors what `begin_enroll` does up-front.
    pub fn reactivate(&mut self) {
        self.driver.drain_rx();
        // Skip activate() (which keeps the sensor awake but deaf for ~10s) and go straight
        // to a smart-poll window so a freshly enrolled finger can be tested immediately.
        self.smart_poll_until =
            Some(std::time::Instant::now() + std::time::Duration::from_secs(10));
        log::info!("fp: reactivate -> smart poll armed for 10s after enrollment");
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
                // Window expired — let the sensor go to sleep on its own
                self.smart_poll_until = None;
                log::info!("fp: smart poll window expired, sensor will sleep in ~10s");
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
