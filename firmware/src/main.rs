//! CyberKey firmware — M5StickC Plus 2
//!
//! BLE HID keyboard via esp32-nimble (NimBLE over ESP-IDF).
//! The NimBLE host task runs inside FreeRTOS; `fn main()` is the user task.

use std::{sync::atomic::Ordering, time::Duration};

use esp_idf_svc::{
    hal::{
        adc::{
            attenuation,
            oneshot::{
                config::{AdcChannelConfig, Calibration},
                AdcChannelDriver, AdcDriver,
            },
        },
        delay::{Delay, FreeRtos},
        gpio::{AnyIOPin, PinDriver},
        i2c::{I2cConfig, I2cDriver},
        peripherals::Peripherals,
        spi::{config::Config as SpiConfig, SpiDeviceDriver, SpiDriver, SpiDriverConfig},
        uart::{config::Config as UartConfig, UartDriver},
        units::Hertz,
    },
    sys::{
        esp_pm_config_esp32_t, esp_pm_configure, esp_sleep_enable_uart_wakeup, link_patches,
        uart_port_t_UART_NUM_1, uart_set_wakeup_threshold,
    },
};
use mipidsi::{
    interface::SpiInterface,
    options::{ColorInversion, ColorOrder, Orientation, Rotation},
};

mod app;
mod ble_hid;
mod board;
mod buttons;
mod cli;
mod config_store;
mod display;
mod fingerprint;
mod fonts;
mod rtc;

fn main() -> anyhow::Result<()> {
    link_patches();
    esp_idf_svc::log::EspLogger::initialize_default();

    let peripherals = Peripherals::take()?;

    // Power hold — GPIO4 must be driven high or the board shuts off after the button is released.
    let mut power_pin = PinDriver::output(peripherals.pins.gpio4)?;
    power_pin.set_high()?;

    unsafe {
        let pm_cfg = esp_pm_config_esp32_t {
            max_freq_mhz: 160,
            min_freq_mhz: 80,
            light_sleep_enable: true,
        };
        esp_pm_configure(&pm_cfg as *const _ as *const core::ffi::c_void);
    }

    // Battery — GPIO38 / ADC1 with a ÷2 voltage divider (M5Unified: _adc_ratio = 2.0).
    // Line-fitting calibration (esp_adc_cal) corrects ESP32 ADC non-linearity.
    // Formula mirrors M5Unified getBatteryLevel: map 3300–4100 mV → 0–100 %.
    let bat_adc = AdcDriver::new(peripherals.adc1)?;
    let mut bat_ch = AdcChannelDriver::new(
        bat_adc,
        peripherals.pins.gpio38,
        &AdcChannelConfig {
            attenuation: attenuation::DB_11,
            calibration: Calibration::Line,
            ..Default::default()
        },
    )?;
    let mut read_battery = || -> Option<u8> {
        match bat_ch.read() {
            Ok(mv_adc) => {
                let mv_bat = mv_adc as f32 * board::BAT_ADC_DIVIDER;
                log::debug!("Battery: mv_adc={} mv_bat={:.0}", mv_adc, mv_bat);
                let level = (mv_bat - board::BAT_MV_MIN) * 100.0 / board::BAT_MV_RANGE;
                Some(level.clamp(0.0, 100.0) as u8)
            }
            Err(e) => {
                log::warn!("Battery ADC read failed: {:?}", e);
                None
            }
        }
    };

    // NVS init — wrapped in Arc<Mutex> so the CLI task can share it.
    let nvs = config_store::init()?;

    // Restore UTC offset persisted by the last sync_clock call so local time
    // displays correctly at boot without needing a host connection.
    if let Ok(guard) = nvs.lock() {
        if let Ok(Some(offset)) = guard.0.get_i32("tz_offset") {
            rtc::UTC_OFFSET_SECS.store(offset, Ordering::Relaxed);
            log::info!("Restored UTC offset: {} s", offset);
        }
    }

    // Enrollment channel — CLI task sends a request; main loop receives it.
    let (enroll_tx, enroll_rx) = std::sync::mpsc::sync_channel::<cli::EnrollRequest>(1);
    // Fingerprint-verify channel — CLI task sends an unlock request; main loop verifies.
    let (verify_tx, verify_rx) = std::sync::mpsc::sync_channel::<cli::VerifyRequest>(1);
    // Fingerprint-delete channel — CLI task requests a single template slot deletion.
    let (delete_tx, delete_rx) = std::sync::mpsc::sync_channel::<cli::DeleteRequest>(1);

    // UART0 (USB-serial, GPIO1=TX / GPIO3=RX) — CLI wire protocol listener.
    // Safety: transmute to 'static is valid because the peripheral registers
    // exist for the entire program lifetime on bare-metal.
    let uart_cfg = UartConfig::new().baudrate(Hertz(board::UART_BAUD));
    let uart0 = UartDriver::new(
        peripherals.uart0,
        peripherals.pins.gpio1,
        peripherals.pins.gpio3,
        Option::<AnyIOPin>::None,
        Option::<AnyIOPin>::None,
        &uart_cfg,
    )?;
    let uart0: esp_idf_svc::hal::uart::UartDriver<'static> = unsafe { core::mem::transmute(uart0) };
    cli::spawn(
        uart0,
        nvs.clone(),
        cli::Senders {
            enroll_tx,
            verify_tx,
            delete_tx,
        },
    )?;

    // RTC init (I2C0 on GPIO21/22)
    let config = I2cConfig::new().baudrate(Hertz(board::I2C_FREQ_HZ));
    let mut i2c_driver = I2cDriver::new(
        peripherals.i2c0,
        peripherals.pins.gpio21,
        peripherals.pins.gpio22,
        &config,
    )?;
    let _ = rtc::init(&mut i2c_driver);

    // SPI2: CLK=GPIO13, MOSI=GPIO15, CS=GPIO5 / DC=GPIO14, RST=GPIO12, BL=GPIO27
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

    let spi_driver = SpiDriver::new(
        peripherals.spi2,
        peripherals.pins.gpio13,
        peripherals.pins.gpio15,
        None::<AnyIOPin>,
        &SpiDriverConfig::new(),
    )?;

    let spi_config = SpiConfig::new()
        .baudrate(Hertz(board::DISP_SPI_MHZ * 1_000_000))
        .data_mode(embedded_hal::spi::MODE_0);

    let spi = SpiDeviceDriver::new(spi_driver, Some(peripherals.pins.gpio5), &spi_config)?;
    log::info!("SPI init OK");

    let dc = PinDriver::output(dc_pin)?;
    let mut di_buf = [0u8; 512];
    let di = SpiInterface::new(spi, dc, &mut di_buf);

    log::info!("Display init starting...");
    let mut delay = Delay::new_default();
    let mut disp = mipidsi::Builder::new(mipidsi::models::ST7789, di)
        .display_size(board::DISP_WIDTH, board::DISP_HEIGHT)
        .display_offset(board::DISP_OFFSET_X, board::DISP_OFFSET_Y)
        .invert_colors(ColorInversion::Inverted)
        .color_order(ColorOrder::Rgb)
        .orientation(Orientation::new().rotate(Rotation::Deg90))
        .init(&mut delay)
        .map_err(|e| anyhow::anyhow!("{:?}", e))?;

    // Backlight on after init so the panel is ready before it becomes visible.
    let mut backlight = PinDriver::output(bl_pin)?;
    backlight.set_high()?;

    let sb = display::StatusBar::unknown();
    display::show_status(&mut disp, &sb, "CyberKey");

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
        display::show_status_2line(&mut disp, &sb, "Fingerprint", "Sensor OK");
    } else {
        log::warn!("Fingerprint sensor not found — check Grove cable");
        display::show_status_2line(&mut disp, &sb, "Fingerprint", "No sensor");
    }
    FreeRtos::delay_ms(2000);

    unsafe {
        uart_set_wakeup_threshold(uart_port_t_UART_NUM_1, 3);
        esp_sleep_enable_uart_wakeup(uart_port_t_UART_NUM_1 as i32);
    }

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

    app::run(
        &ble,
        &mut disp,
        buttons,
        passkey,
        power_pin,
        backlight,
        &mut fp,
        nvs,
        enroll_rx,
        verify_rx,
        delete_rx,
        &mut i2c_driver,
        &mut read_battery,
    )?;

    Ok(())
}
