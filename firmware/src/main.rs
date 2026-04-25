//! CyberKey firmware — M5StickC Plus 2
//!
//! BLE HID keyboard via esp32-nimble (NimBLE over ESP-IDF).
//! The NimBLE host task runs inside FreeRTOS; `fn main()` is the user task.

use std::{sync::atomic::Ordering, time::Duration};

use esp_idf_svc::{
    hal::{
        delay::{Delay, FreeRtos},
        gpio::{AnyIOPin, InputPin, Output, OutputPin, PinDriver},
        peripherals::Peripherals,
        spi::{config::Config as SpiConfig, SpiDeviceDriver, SpiDriver, SpiDriverConfig},
        units::Hertz,
    },
    sys::link_patches,
};
use mipidsi::{
    interface::SpiInterface,
    options::{ColorInversion, ColorOrder, Orientation, Rotation},
};

mod ble_hid;
mod buttons;
mod display;
mod fingerprint;
mod hid;

use buttons::ButtonEvent;

fn main() -> anyhow::Result<()> {
    link_patches();
    esp_idf_svc::log::EspLogger::initialize_default();

    let peripherals = Peripherals::take()?;

    // Power hold — GPIO4 must be driven high or the board shuts off after the button is released.
    let mut power_pin = PinDriver::output(peripherals.pins.gpio4)?;
    power_pin.set_high()?;

    // SPI2: CLK=GPIO13, MOSI=GPIO15, CS=GPIO5
    let spi2 = peripherals.spi2;
    let mosi = peripherals.pins.gpio15;
    let clk = peripherals.pins.gpio13;
    let cs_pin = peripherals.pins.gpio5;

    // DC=GPIO14, RST=GPIO12
    let dc_pin = peripherals.pins.gpio14;
    let rst_pin = peripherals.pins.gpio12;
    let bl_pin = peripherals.pins.gpio27;

    // Random 6-digit passkey from hardware RNG.
    // esp_random() is always available on ESP32 and is cryptographically strong
    // once the RF subsystem (BLE) is active.  It's safe to call before BLE init.
    let passkey = unsafe { esp_idf_svc::sys::esp_random() } % 1_000_000;

    // ------------------------------------------------------------------
    // Display — ST7789V2, 135×240, landscape (Deg90)
    // ------------------------------------------------------------------

    // Hardware reset: RST low → 20 ms → high → 120 ms before mipidsi init.
    let mut rst = PinDriver::output(rst_pin)?;
    rst.set_low()?;
    std::thread::sleep(Duration::from_millis(20));
    rst.set_high()?;
    std::thread::sleep(Duration::from_millis(120));

    let spi_driver = SpiDriver::new(spi2, clk, mosi, None::<AnyIOPin>, &SpiDriverConfig::new())?;

    let spi_config = SpiConfig::new()
        .baudrate(Hertz(20 * 1_000_000))
        .data_mode(embedded_hal::spi::MODE_0);

    let spi = SpiDeviceDriver::new(spi_driver, Some(cs_pin), &spi_config)?;
    log::info!("SPI init OK");

    let dc = PinDriver::output(dc_pin)?; // DC
    let mut di_buf = [0u8; 512];
    let di = SpiInterface::new(spi, dc, &mut di_buf);

    log::info!("Display init starting...");
    let mut delay = Delay::new_default();
    let mut disp = mipidsi::Builder::new(mipidsi::models::ST7789, di)
        .display_size(135, 240)
        .display_offset(52, 40)
        .invert_colors(ColorInversion::Inverted)
        .color_order(ColorOrder::Bgr)
        .orientation(Orientation::new().rotate(Rotation::Deg90))
        .init(&mut delay)
        .map_err(|e| anyhow::anyhow!("{:?}", e))?;

    // Backlight on after init so the panel is ready before it becomes visible.
    let mut backlight = PinDriver::output(bl_pin)?;
    backlight.set_high()?;

    display::show_pin(&mut disp, passkey);

    // ------------------------------------------------------------------
    // BLE HID
    // ------------------------------------------------------------------
    let ble = ble_hid::init(passkey);

    // ------------------------------------------------------------------
    // Fingerprint sensor — Grove port (UART1)
    // Grove UART convention: Yellow=Pin1=TX (MCU→sensor), White=Pin2=RX (sensor→MCU)
    // M5StickC Plus 2 Grove: GPIO32=Yellow, GPIO33=White → TX=32, RX=33.
    // ------------------------------------------------------------------
    let mut fp = fingerprint::FingerprintSensor::new(
        peripherals.uart1,
        peripherals.pins.gpio32, // TX — Yellow Grove wire
        peripherals.pins.gpio33, // RX — White Grove wire
    )?;
    // Allow the sensor's STM32 MCU time to boot before the first handshake.
    FreeRtos::delay_ms(500);
    if fp.init() {
        log::info!("Fingerprint sensor ready");
        display::show_status_2line(&mut disp, "Fingerprint", "Sensor OK");
    } else {
        log::warn!("Fingerprint sensor not found — check Grove cable");
        display::show_status_2line(&mut disp, "Fingerprint", "No sensor");
    }
    FreeRtos::delay_ms(2000);
    display::show_pin(&mut disp, passkey);

    // ------------------------------------------------------------------
    // Buttons — GPIO37 (A), GPIO39 (B), GPIO35 (C/power); active-low.
    // GPIO35/37/39 are input-only on ESP32 silicon (no internal pull resistors;
    // the M5StickC Plus 2 board has external pull-ups on these lines).
    // ------------------------------------------------------------------
    let buttons = buttons::Buttons::new(
        PinDriver::input(peripherals.pins.gpio37)?,
        PinDriver::input(peripherals.pins.gpio39)?,
        PinDriver::input(peripherals.pins.gpio35)?,
    );

    main_loop(&ble, &mut disp, buttons, passkey, power_pin, backlight, &mut fp)?;

    Ok(())
}

fn main_loop<D, A, B, C, P, BL>(
    ble: &ble_hid::BleHid,
    disp: &mut D,
    mut buttons: buttons::Buttons<'_, A, B, C>,
    passkey: u32,
    mut power_pin: PinDriver<'_, P, Output>,
    _backlight: PinDriver<'_, BL, Output>,
    fp: &mut fingerprint::FingerprintSensor<'_>,
) -> anyhow::Result<()>
where
    D: embedded_graphics::draw_target::DrawTarget<Color = embedded_graphics::pixelcolor::Rgb565>,
    A: InputPin,
    B: InputPin,
    C: InputPin,
    P: OutputPin,
    BL: OutputPin,
{
    let mut last_connected = false;
    let mut pending_bond_clear = false;

    loop {
        let connected = ble_hid::CONNECTED.load(Ordering::Relaxed);

        if connected != last_connected {
            last_connected = connected;
            pending_bond_clear = false;
            if connected {
                display::show_status(disp, "Connected");
            } else {
                display::show_pin(disp, passkey);
            }
        }

        match buttons.poll() {
            Some(ButtonEvent::ALongPress) => {
                if pending_bond_clear {
                    display::show_status(disp, "Clearing...");
                    FreeRtos::delay_ms(500);
                    ble_hid::clear_bonds_and_reboot();
                } else {
                    pending_bond_clear = true;
                    display::show_status_2line(disp, "Clear bond?", "Hold A again");
                }
            }
            Some(ButtonEvent::AShortPress) => {
                if pending_bond_clear {
                    pending_bond_clear = false;
                    if connected {
                        display::show_status(disp, "Connected");
                    } else {
                        display::show_pin(disp, passkey);
                    }
                }
            }
            Some(ButtonEvent::BLongPress) => {
                pending_bond_clear = false;
                const ENROLL_SLOT: u16 = 0;
                const ENROLL_PASSES: u8 = 3;
                display::show_enroll_pass(disp, 1, ENROLL_PASSES);
                if fp.begin_enroll(ENROLL_SLOT, ENROLL_PASSES) {
                    let mut pass = 0u8;
                    let mut failed = false;

                    loop {
                        match fp.poll_enroll_ack() {
                            fingerprint::EnrollAck::CaptureOk => {
                                pass += 1;
                                if pass < ENROLL_PASSES {
                                    // Sensor confirmed finger lifted; prompt user to
                                    // reposition before the next capture.
                                    display::show_status_2line(disp, "Lift finger!", "reposition");
                                    FreeRtos::delay_ms(1500);
                                    display::show_enroll_pass(disp, pass + 1, ENROLL_PASSES);
                                } else {
                                    display::show_status(disp, "Processing...");
                                }
                            }
                            fingerprint::EnrollAck::Done => break,
                            fingerprint::EnrollAck::Failed => {
                                failed = true;
                                break;
                            }
                            fingerprint::EnrollAck::Pending => {}
                        }
                        FreeRtos::delay_ms(20);
                    }

                    if !failed {
                        display::show_enroll_ok(disp, ENROLL_SLOT);
                    } else {
                        display::show_status_2line(disp, "Enroll", "Failed");
                    }
                } else {
                    display::show_status_2line(disp, "Enroll", "Failed");
                }
                // Re-arm the sensor for autonomous detection after enrollment.
                fp.reactivate();
                FreeRtos::delay_ms(2000);
                if connected {
                    display::show_status(disp, "Connected");
                } else {
                    display::show_pin(disp, passkey);
                }
            }
            Some(ButtonEvent::BShortPress) => {
                pending_bond_clear = false;
                if connected {
                    ble.type_string("Hello!");
                }
            }
            Some(ButtonEvent::CPowerLongPress) => {
                display::show_status(disp, "Powering off...");
                FreeRtos::delay_ms(500);
                power_pin.set_low()?;
                loop {
                    FreeRtos::delay_ms(100);
                }
            }
            None => {}
        }

        // Honour bond-clear flag (can also be set by future CLI command).
        if ble_hid::CLEAR_BONDS.load(Ordering::Relaxed) {
            display::show_status(disp, "Clearing...");
            FreeRtos::delay_ms(500);
            ble_hid::clear_bonds_and_reboot();
        }

        // Fingerprint — non-blocking poll; blocks ~20 ms only when a finger is detected.
        match fp.poll() {
            Some(fingerprint::IdentifyResult::Match(id)) => {
                display::show_auth_ok(disp, id);
                FreeRtos::delay_ms(2000);
                if connected {
                    display::show_status(disp, "Connected");
                } else {
                    display::show_pin(disp, passkey);
                }
            }
            Some(fingerprint::IdentifyResult::NoMatch) => {
                display::show_no_match(disp);
                FreeRtos::delay_ms(2000);
                if connected {
                    display::show_status(disp, "Connected");
                } else {
                    display::show_pin(disp, passkey);
                }
            }
            None => {}
        }

        FreeRtos::delay_ms(20);
    }
}
