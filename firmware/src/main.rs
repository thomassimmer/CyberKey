//! CyberKey firmware — M5StickC Plus 2
//!
//! BLE HID keyboard via esp32-nimble (NimBLE over ESP-IDF).
//! The NimBLE host task runs inside FreeRTOS; `fn main()` is the user task.

use std::{
    sync::{atomic::Ordering, Arc, Mutex},
    time::Duration,
};

use esp_idf_svc::{
    hal::{
        delay::{Delay, FreeRtos},
        gpio::{AnyIOPin, InputPin, Output, OutputPin, PinDriver},
        i2c::{I2cConfig, I2cDriver},
        peripherals::Peripherals,
        spi::{config::Config as SpiConfig, SpiDeviceDriver, SpiDriver, SpiDriverConfig},
        uart::{config::Config as UartConfig, UartDriver},
        units::Hertz,
    },
    nvs::{EspDefaultNvsPartition, EspNvs},
    sys::link_patches,
};
use mipidsi::{
    interface::SpiInterface,
    options::{ColorInversion, ColorOrder, Orientation, Rotation},
};

mod ble_hid;
mod buttons;
mod cli;
mod display;
mod fingerprint;
mod hid;

use buttons::ButtonEvent;

fn bcd2dec(bcd: u8) -> u8 {
    (bcd >> 4) * 10 + (bcd & 0x0F)
}

const MONTH_DAYS: [u32; 12] = [0, 31, 59, 90, 120, 151, 181, 212, 243, 273, 304, 334];
fn is_leap_year(year: u32) -> bool {
    year.is_multiple_of(4) && (!year.is_multiple_of(100) || year.is_multiple_of(400))
}
fn rtc_to_timestamp(year: u16, month: u8, day: u8, hour: u8, minute: u8, second: u8) -> u64 {
    let mut days = 0;
    for y in 1970..year {
        days += if is_leap_year(y.into()) { 366 } else { 365 };
    }
    days += MONTH_DAYS[month as usize - 1];
    if month > 2 && is_leap_year(year.into()) {
        days += 1;
    }
    days += (day - 1) as u32;

    (days as u64 * 86400) + (hour as u64 * 3600) + (minute as u64 * 60) + second as u64
}

fn init_rtc(i2c: &mut I2cDriver) -> anyhow::Result<()> {
    let mut buf = [0u8; 7];
    if let Err(e) = i2c.write_read(0x51, &[0x02], &mut buf, esp_idf_svc::hal::delay::BLOCK) {
        log::warn!("RTC read failed: {:?}, using compile time", e);
        return fallback_time();
    }

    let vl = buf[0] & 0x80;
    if vl != 0 {
        log::warn!("RTC voltage low (unset), using compile time");
        return fallback_time();
    }

    let sec = bcd2dec(buf[0] & 0x7F);
    let min = bcd2dec(buf[1] & 0x7F);
    let hour = bcd2dec(buf[2] & 0x3F);
    let day = bcd2dec(buf[3] & 0x3F);
    let month = bcd2dec(buf[5] & 0x1F);
    let year_offset = bcd2dec(buf[6]);
    let year = 2000 + year_offset as u16;

    let ts = rtc_to_timestamp(year, month, day, hour, min, sec);
    log::info!(
        "RTC: {:04}-{:02}-{:02} {:02}:{:02}:{:02} -> ts={}",
        year,
        month,
        day,
        hour,
        min,
        sec,
        ts
    );
    set_system_time(ts);
    Ok(())
}

fn fallback_time() -> anyhow::Result<()> {
    let ts: u64 = env!("BUILD_TIME").parse().unwrap_or(0);
    log::info!("Fallback to compile time: {}", ts);
    set_system_time(ts);
    Ok(())
}

fn set_system_time(ts: u64) {
    let tv = esp_idf_svc::sys::timeval {
        tv_sec: ts as _,
        tv_usec: 0,
    };
    unsafe {
        esp_idf_svc::sys::settimeofday(&tv, std::ptr::null());
    }
}

fn main() -> anyhow::Result<()> {
    link_patches();
    esp_idf_svc::log::EspLogger::initialize_default();

    let peripherals = Peripherals::take()?;

    // Power hold — GPIO4 must be driven high or the board shuts off after the button is released.
    let mut power_pin = PinDriver::output(peripherals.pins.gpio4)?;
    power_pin.set_high()?;

    // NVS init — wrapped in Arc<Mutex> so the CLI task can share it.
    let default_partition = EspDefaultNvsPartition::take()?;
    let nvs_inner = EspNvs::new(default_partition, "ck", true)?;
    let nvs = Arc::new(Mutex::new(cli::SharedNvs(nvs_inner)));

    // Enrollment IPC queue — CLI task posts a request here; main loop picks it up.
    let enroll_queue: cli::EnrollQueue = Arc::new(Mutex::new(None));

    // UART0 (USB-serial, GPIO1=TX / GPIO3=RX) — CLI wire protocol listener.
    // Safety: transmute to 'static is valid because the peripheral registers
    // exist for the entire program lifetime on bare-metal.
    let uart_cfg = UartConfig::new().baudrate(Hertz(115_200));
    let uart0 = UartDriver::new(
        peripherals.uart0,
        peripherals.pins.gpio1,
        peripherals.pins.gpio3,
        Option::<AnyIOPin>::None,
        Option::<AnyIOPin>::None,
        &uart_cfg,
    )?;
    let uart0: esp_idf_svc::hal::uart::UartDriver<'static> = unsafe { core::mem::transmute(uart0) };
    cli::spawn(uart0, nvs.clone(), enroll_queue.clone())?;

    // RTC init (I2C0 on GPIO21/22)
    let i2c = peripherals.i2c0;
    let sda = peripherals.pins.gpio21;
    let scl = peripherals.pins.gpio22;
    let config = I2cConfig::new().baudrate(Hertz(400_000));
    let mut i2c_driver = I2cDriver::new(i2c, sda, scl, &config)?;
    let _ = init_rtc(&mut i2c_driver);

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

    // Physical factory reset: hold Button A for 5 s at power-on, then press A again to confirm.
    {
        const POLL_MS: u32 = 20;
        const HOLD_TARGET: u32 = 5_000 / POLL_MS; // 250 polls = 5 s

        let mut hold: u32 = 0;
        while buttons.is_a_down() && hold < HOLD_TARGET {
            hold += 1;
            FreeRtos::delay_ms(POLL_MS);
        }

        if hold >= HOLD_TARGET {
            display::show_status_2line(&mut disp, "Factory Reset?", "Press A again");

            // Wait for release, then wait for a second press (10 s timeout).
            while buttons.is_a_down() {
                FreeRtos::delay_ms(POLL_MS);
            }

            let mut confirmed = false;
            for _ in 0..(10_000 / POLL_MS) {
                FreeRtos::delay_ms(POLL_MS);
                if buttons.is_a_down() {
                    confirmed = true;
                    break;
                }
            }

            if confirmed {
                log::warn!("Factory reset: triggered via physical button (Button A hold at boot)");
                display::show_status(&mut disp, "Resetting...");
                log::info!("Factory reset: clearing fingerprint templates...");
                fp.empty_template_library();
                log::info!("Factory reset: fingerprint templates cleared");
                log::info!("Factory reset: erasing NVS slots...");
                {
                    let mut guard = nvs.lock().unwrap();
                    for slot in 0u32..10 {
                        let _ = guard.0.remove(&format!("slot_{slot}"));
                        let _ = guard.0.remove(&format!("label_{slot}"));
                    }
                }
                log::info!("Factory reset: NVS erased");
                log::warn!("Factory reset: complete — rebooting");
                display::show_reset_ok(&mut disp);
                FreeRtos::delay_ms(2000);
                unsafe { esp_idf_svc::sys::esp_restart() }
            }
        }
    }

    main_loop(
        &ble,
        &mut disp,
        buttons,
        passkey,
        power_pin,
        backlight,
        &mut fp,
        nvs,
        enroll_queue,
    )?;

    Ok(())
}

#[allow(clippy::too_many_arguments)]
fn main_loop<D, A, B, C, P, BL>(
    ble: &ble_hid::BleHid,
    disp: &mut D,
    mut buttons: buttons::Buttons<'_, A, B, C>,
    passkey: u32,
    mut power_pin: PinDriver<'_, P, Output>,
    _backlight: PinDriver<'_, BL, Output>,
    fp: &mut fingerprint::FingerprintSensor<'_>,
    nvs: Arc<Mutex<cli::SharedNvs>>,
    enroll_queue: cli::EnrollQueue,
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
                // Enrollment is now driven exclusively by the CLI (add_entry command).
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

        // CLI-driven factory reset: erase fingerprint templates then reboot.
        if cli::FACTORY_RESET.load(Ordering::Relaxed) {
            log::warn!("Factory reset: triggered via CLI");
            display::show_status(disp, "Resetting...");
            log::info!("Factory reset: clearing fingerprint templates...");
            fp.empty_template_library();
            log::info!("Factory reset: fingerprint templates cleared");
            log::warn!("Factory reset: complete — rebooting");
            display::show_reset_ok(disp);
            FreeRtos::delay_ms(2000);
            unsafe { esp_idf_svc::sys::esp_restart() }
        }

        // CLI-driven enrollment: pick up a pending EnrollRequest from the CLI task.
        if let Ok(mut eq) = enroll_queue.try_lock() {
            if let Some(request) = eq.take() {
                drop(eq); // release lock before the blocking enrollment loop
                const PASSES: u8 = 3;
                display::show_status_2line(disp, "CLI Enroll", "Place finger");
                if fp.begin_enroll(request.slot, PASSES) {
                    let _ = request.reply.send(cli::EnrollResp::PlaceFinger {
                        step: 1,
                        total: PASSES,
                    });
                    let mut pass = 0u8;
                    loop {
                        match fp.poll_enroll_ack() {
                            fingerprint::EnrollAck::CaptureOk => {
                                pass += 1;
                                let _ = request.reply.send(cli::EnrollResp::LiftFinger {
                                    step: pass,
                                    total: PASSES,
                                });
                                if pass < PASSES {
                                    let _ = request.reply.send(cli::EnrollResp::PlaceFinger {
                                        step: pass + 1,
                                        total: PASSES,
                                    });
                                    display::show_status_2line(
                                        disp,
                                        "Lift + replace",
                                        &format!("pass {}/{}", pass + 1, PASSES),
                                    );
                                } else {
                                    display::show_status(disp, "Processing...");
                                }
                            }
                            fingerprint::EnrollAck::Done => {
                                let _ = request.reply.send(cli::EnrollResp::Done);
                                display::show_enroll_ok(disp, request.slot);
                                break;
                            }
                            fingerprint::EnrollAck::Failed => {
                                let _ = request.reply.send(cli::EnrollResp::Failed);
                                display::show_status_2line(disp, "Enroll", "Failed");
                                break;
                            }
                            fingerprint::EnrollAck::Pending => {}
                        }
                        FreeRtos::delay_ms(20);
                    }
                } else {
                    let _ = request.reply.send(cli::EnrollResp::Failed);
                    display::show_status_2line(disp, "Enroll", "Failed");
                }
                fp.reactivate();
                FreeRtos::delay_ms(2000);
                if connected {
                    display::show_status(disp, "Connected");
                } else {
                    display::show_pin(disp, passkey);
                }
            }
        }

        // Fingerprint — non-blocking poll; blocks ~20 ms only when a finger is detected.
        match fp.poll() {
            Some(fingerprint::IdentifyResult::Match(id)) => {
                display::show_auth_ok(disp, id);

                // Fetch secret from NVS
                let key = format!("slot_{}", id);
                let mut buf = [0u8; 65];
                let totp_result = {
                    let guard = nvs.lock().unwrap();
                    match guard.0.get_str(&key, &mut buf) {
                        Ok(Some(secret)) => {
                            let now =
                                unsafe { esp_idf_svc::sys::time(std::ptr::null_mut()) } as u64;
                            Some(cyberkey_core::generate_totp(secret, now))
                        }
                        Ok(None) => {
                            log::warn!("No secret found for slot {}", id);
                            None
                        }
                        Err(e) => {
                            log::warn!("NVS read error: {:?}", e);
                            None
                        }
                    }
                }; // guard dropped here

                if let Some(result) = totp_result {
                    match result {
                        Ok(code) => {
                            display::show_totp(disp, code);
                            if connected {
                                ble.type_digits(&format!("{:06}", code));
                                log::info!("TOTP typed: {:06}", code);
                            } else {
                                log::info!("TOTP generated (not connected): {:06}", code);
                            }
                        }
                        Err(e) => {
                            log::warn!("TOTP error: {:?}", e);
                        }
                    }
                }

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
