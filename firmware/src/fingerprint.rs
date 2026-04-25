//! Fingerprint sensor integration — M5Stack Unit Fingerprint2 (U203) via Grove.
//!
//! Grove port pin assignment (M5StickC Plus 2):
//!   GPIO32 = Yellow = TX (MCU → sensor)
//!   GPIO33 = White  = RX (sensor → MCU)
//! Source: Grove UART convention (Pin1=TX, Pin2=RX from host perspective)
//! confirmed against M5StickC Plus 2 docs (docs.m5stack.com).

use esp_idf_svc::hal::{
    gpio::AnyIOPin,
    peripheral::Peripheral,
    uart::{config::Config as UartConfig, Uart, UartDriver},
    units::Hertz,
};
use fingerprint2_rs::{DriverEvent, Fingerprint2Driver, FingerprintError};

const BAUD: u32 = 115_200;
const SECURITY_LEVEL: u8 = 3;

pub enum IdentifyResult {
    Match(u16),
    NoMatch,
}

pub struct FingerprintSensor<'d> {
    driver: Fingerprint2Driver<UartDriver<'d>>,
    ready: bool,
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
            driver: Fingerprint2Driver::new(uart_driver),
            ready: false,
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
                log::info!("Fingerprint: sensor online");
                true
            }
            Err(e) => {
                log::warn!("Fingerprint: handshake error = {:?}", e);
                false
            }
        }
    }

    /// Non-blocking poll. Returns Some(result) when a finger was placed and identified.
    ///
    /// Returns None immediately when no finger is on the pad (WouldBlock).
    pub fn poll(&mut self) -> Option<IdentifyResult> {
        if !self.ready {
            return None;
        }
        match self.driver.poll_event() {
            Ok(DriverEvent::Wakeup) => match self.driver.auto_identify(SECURITY_LEVEL) {
                Ok(id) => Some(IdentifyResult::Match(id)),
                // 0x09 = no match; 0xFE = library empty (no enrolled templates)
                Err(FingerprintError::NoMatch) | Err(FingerprintError::SensorError(0xFE)) => {
                    Some(IdentifyResult::NoMatch)
                }
                Err(e) => {
                    log::warn!("identify error: {:?}", e);
                    None
                }
            },
            Ok(DriverEvent::Ack { .. }) => None,
            Err(nb::Error::WouldBlock) => None,
            Err(nb::Error::Other(e)) => {
                log::warn!("fp poll error: {:?}", e);
                None
            }
        }
    }
}
