//! Main event loop — BLE state, buttons, fingerprint, enrollment, screen timeout.

use std::sync::{atomic::Ordering, Arc, Mutex};

use esp_idf_svc::hal::{
    delay::FreeRtos,
    gpio::{InputPin, Output, OutputPin, PinDriver},
    i2c::I2cDriver,
};

use crate::{
    ble_hid,
    buttons::{ButtonEvent, POLL_MS},
    cli, config_store, display, fingerprint, rtc,
};

/// Erase all user data and reboot.
///
/// Clears: fingerprint templates, TOTP slots + labels, timezone offset, BLE bonds.
fn do_factory_reset<D>(
    trigger: &str,
    disp: &mut D,
    sb: &display::StatusBar<'_>,
    fp: &mut fingerprint::FingerprintSensor<'_>,
    nvs: &Arc<Mutex<config_store::SharedNvs>>,
) -> !
where
    D: embedded_graphics::draw_target::DrawTarget<Color = embedded_graphics::pixelcolor::Rgb565>,
{
    log::warn!("Factory reset: triggered via {trigger}");
    display::show_status(disp, sb, "Resetting...");
    fp.empty_template_library();
    log::info!("Factory reset: fingerprint templates cleared");
    {
        let mut guard = config_store::lock_nvs(nvs);
        for slot in 0u32..10 {
            let _ = guard.0.remove(&format!("slot_{slot}"));
            let _ = guard.0.remove(&format!("label_{slot}"));
        }
        let _ = guard.0.remove("tz_offset");
    }
    ble_hid::clear_bonds();
    log::warn!("Factory reset: complete — rebooting");
    display::show_reset_ok(disp, sb);
    FreeRtos::delay_ms(2000);
    unsafe { esp_idf_svc::sys::esp_restart() }
}

/// Boot-time check: if Button A is held for 2 s, prompt for a second press to confirm,
/// then erase all fingerprint templates and NVS slots before rebooting.
fn check_boot_factory_reset<D, A, B, C>(
    buttons: &crate::buttons::Buttons<'_, A, B, C>,
    disp: &mut D,
    sb: &display::StatusBar<'_>,
    fp: &mut fingerprint::FingerprintSensor<'_>,
    nvs: &Arc<Mutex<config_store::SharedNvs>>,
) where
    D: embedded_graphics::draw_target::DrawTarget<Color = embedded_graphics::pixelcolor::Rgb565>,
    A: InputPin,
    B: InputPin,
    C: InputPin,
{
    const POLL_MS: u32 = 20;
    const HOLD_MS: u128 = 2_000;

    let start = std::time::Instant::now();
    let held = loop {
        if !buttons.is_a_down() {
            break false;
        }
        if start.elapsed().as_millis() >= HOLD_MS {
            break true;
        }
        FreeRtos::delay_ms(POLL_MS);
    };
    if !held {
        return;
    }

    display::show_status_2line(disp, sb, "Factory Reset?", "Press A again");

    while buttons.is_a_down() {
        FreeRtos::delay_ms(POLL_MS);
    }

    let confirm_start = std::time::Instant::now();
    let mut confirmed = false;
    while confirm_start.elapsed().as_millis() < 10_000 {
        FreeRtos::delay_ms(POLL_MS);
        if buttons.is_a_down() {
            confirmed = true;
            break;
        }
    }

    if confirmed {
        do_factory_reset("physical button (Button A hold at boot)", disp, sb, fp, nvs);
    }
}

/// Wake the screen if it is off, resetting the inactivity counter.
///
/// Centralises the repeated "ensure screen is on" pattern used throughout the
/// main event loop (button press, BLE event, fingerprint result, CLI request…).
fn wake_screen_if_off<BL: OutputPin>(
    screen_on: &mut bool,
    inactivity_ticks: &mut u32,
    backlight: &mut PinDriver<'_, BL, Output>,
    fp: &mut fingerprint::FingerprintSensor<'_>,
) {
    *inactivity_ticks = 0;
    if !*screen_on {
        backlight.set_high().ok();
        *screen_on = true;
    }
    // Always ensure the sensor is in Active Mode and polling when there is activity.
    // fp.wake() is optimized to skip redundant UART traffic if already polling.
    fp.wake();
}

/// Restore the main idle screen after any action that temporarily takes over the display.
///
/// Shows the active-clients count, the pairing PIN, or the "Press B to pair" prompt
/// depending on the current BLE state — the three branches are the same everywhere
/// in the main loop so this helper eliminates 5× copies of the same code.
fn restore_idle_screen<D>(
    disp: &mut D,
    sb: &display::StatusBar<'_>,
    connected: u32,
    pairing_open: bool,
    passkey: u32,
) where
    D: embedded_graphics::draw_target::DrawTarget<Color = embedded_graphics::pixelcolor::Rgb565>,
{
    if connected > 0 {
        display::show_status(disp, sb, &format!("ACTIVE CLIENTS: {}", connected));
    } else if pairing_open {
        display::show_pin(disp, sb, passkey, connected);
    } else {
        display::show_status(disp, sb, "Press B to pair");
    }
}

/// Generate a fresh random passkey, open a 60-second pairing window, and
/// set `pairing_open` + `pairing_auto_close_at` accordingly.
///
/// Returns the new passkey so the caller can pass it to `show_pin`.
fn open_fresh_pairing_window(pairing_open: &mut bool, pairing_auto_close_at: &mut u64) -> u32 {
    let passkey = unsafe { esp_idf_svc::sys::esp_random() } % 1_000_000;
    ble_hid::open_pairing_window(passkey);
    *pairing_open = true;
    *pairing_auto_close_at = unsafe { esp_idf_svc::sys::time(std::ptr::null_mut()) } as u64 + 60;
    passkey
}

#[allow(clippy::too_many_arguments)]
pub fn run<D, A, B, C, P, BL, F>(
    ble: &ble_hid::BleHid,
    disp: &mut D,
    mut buttons: crate::buttons::Buttons<'_, A, B, C>,
    mut passkey: u32,
    mut power_pin: PinDriver<'_, P, Output>,
    mut backlight: PinDriver<'_, BL, Output>,
    fp: &mut fingerprint::FingerprintSensor<'_>,
    nvs: Arc<Mutex<config_store::SharedNvs>>,
    enroll_rx: std::sync::mpsc::Receiver<cli::EnrollRequest>,
    verify_rx: std::sync::mpsc::Receiver<cli::VerifyRequest>,
    i2c: &mut I2cDriver<'_>,
    read_battery: &mut F,
) -> anyhow::Result<()>
where
    D: embedded_graphics::draw_target::DrawTarget<Color = embedded_graphics::pixelcolor::Rgb565>,
    A: InputPin,
    B: InputPin,
    C: InputPin,
    P: OutputPin,
    BL: OutputPin,
    F: FnMut() -> Option<u8>,
{
    let mut last_connected = 0u32;
    let mut pending_bond_clear = false;

    let mut battery: Option<u8> = read_battery();
    let mut last_battery_tick: u32 = 0;
    let mut last_minute: u8 = 255; // force topbar draw on first iteration
    let mut tick: u32 = 0;

    const SCREEN_TIMEOUT_TICKS: u32 = 1_500; // 30 s at POLL_MS/tick
    const IDLE_POLL_MS: u32 = 100; // sleep interval when screen is off
    let mut inactivity_ticks: u32 = 0;
    let mut screen_on = true;

    let sb_boot = display::StatusBar::unknown();
    check_boot_factory_reset(&buttons, disp, &sb_boot, fp, &nvs);

    // Open the pairing window automatically on first boot (no bonds).
    // With bonds, start closed and require an explicit button/CLI trigger.
    // pairing_auto_close_at == 0 means no expiry (first-boot window stays
    // open until the first connection closes it).
    let has_bonds_at_boot = ble_hid::has_bonds();
    let mut pairing_open = true; // Always start open to allow reconnect or first-pair
    let mut pairing_auto_close_at: u64 = 0;

    if has_bonds_at_boot {
        // Silent background sync for 15 seconds.
        pairing_auto_close_at = unsafe { esp_idf_svc::sys::time(std::ptr::null_mut()) } as u64 + 15;
        ble_hid::start_background_sync();
        display::show_status(disp, &sb_boot, "Press B to pair");
    } else {
        // First boot or no bonds: stay open until first connection.
        ble_hid::open_pairing_window(passkey);
        display::show_pin(disp, &sb_boot, passkey, 0);
    }

    loop {
        tick = tick.wrapping_add(1);

        // Drain any pending RTC write requested by the CLI task.
        if let Ok(mut guard) = rtc::PENDING_RTC_WRITE.lock() {
            if let Some(ts) = guard.take() {
                rtc::write(i2c, ts);
            }
        }

        // Refresh battery every 30 s (1 500 × 20 ms ticks).
        if tick.wrapping_sub(last_battery_tick) >= 1_500 {
            battery = read_battery();
            last_battery_tick = tick;
        }

        let time_str = rtc::format_time();
        let sb = display::StatusBar {
            time: &time_str,
            battery,
        };

        // Refresh the top bar when the minute changes (no content repaint).
        let cur_minute =
            (unsafe { esp_idf_svc::sys::time(std::ptr::null_mut()) } as u64 / 60 % 60) as u8;
        if cur_minute != last_minute {
            last_minute = cur_minute;
            display::update_topbar(disp, &sb);
        }

        let connected = ble_hid::CONNECTED.load(Ordering::Relaxed);

        if connected != last_connected {
            last_connected = connected;
            pending_bond_clear = false;

            if pairing_open {
                if ble_hid::PAIRING_ALLOWED.load(Ordering::Relaxed) {
                    display::show_pin(disp, &sb, passkey, connected);
                } else if connected > 0 {
                    display::show_status(disp, &sb, &format!("ACTIVE CLIENTS: {}", connected));
                } else {
                    display::show_status(disp, &sb, "Press B to pair");
                }
            } else if connected > 0 {
                display::show_status(disp, &sb, &format!("ACTIVE CLIENTS: {}", connected));
            } else {
                display::show_status(disp, &sb, "Press B to pair");
            }
        }

        // Auto-close the pairing window after the configured timeout.
        if pairing_open && pairing_auto_close_at > 0 {
            let now_ts = unsafe { esp_idf_svc::sys::time(std::ptr::null_mut()) } as u64;
            if now_ts >= pairing_auto_close_at {
                ble_hid::close_pairing_window();
                pairing_open = false;
                pairing_auto_close_at = 0;
                if connected == 0 {
                    display::show_status(disp, &sb, "Press B to pair");
                } else {
                    display::show_status(disp, &sb, &format!("ACTIVE CLIENTS: {}", connected));
                }
            }
        }

        // CLI allow_pairing: generate a fresh passkey and open a 60-second window.
        if ble_hid::OPEN_PAIRING_REQUESTED.swap(false, Ordering::Relaxed) {
            passkey = open_fresh_pairing_window(&mut pairing_open, &mut pairing_auto_close_at);
            wake_screen_if_off(&mut screen_on, &mut inactivity_ticks, &mut backlight, fp);
            display::show_pin(disp, &sb, passkey, connected);
        }

        let btn_event = buttons.poll();
        if btn_event.is_some() || (!screen_on && buttons.is_any_down()) {
            wake_screen_if_off(&mut screen_on, &mut inactivity_ticks, &mut backlight, fp);
        }
        match btn_event {
            Some(ButtonEvent::ALongPress) => {
                if pending_bond_clear {
                    display::show_status(disp, &sb, "Clearing...");
                    FreeRtos::delay_ms(500);
                    ble_hid::clear_bonds_and_reboot();
                } else {
                    pending_bond_clear = true;
                    display::show_status_2line(disp, &sb, "Clear bond?", "Hold A again");
                }
            }
            Some(ButtonEvent::AShortPress) => {
                if pending_bond_clear {
                    pending_bond_clear = false;
                    restore_idle_screen(disp, &sb, connected, pairing_open, passkey);
                }
            }
            Some(ButtonEvent::BLongPress) => {
                pending_bond_clear = false;
                // Reserved for future use
            }
            Some(ButtonEvent::BShortPress) => {
                pending_bond_clear = false;
                if pairing_open {
                    ble_hid::close_pairing_window();
                    pairing_open = false;
                    pairing_auto_close_at = 0;
                    restore_idle_screen(disp, &sb, connected, pairing_open, passkey);
                } else {
                    passkey =
                        open_fresh_pairing_window(&mut pairing_open, &mut pairing_auto_close_at);
                    display::show_pin(disp, &sb, passkey, connected);
                }
            }
            Some(ButtonEvent::CPowerLongPress) => {
                log::warn!("Powering off sequence initiated...");
                display::show_power_off(disp, &sb);
                FreeRtos::delay_ms(1500);
                power_pin.set_low()?;
                loop {
                    FreeRtos::delay_ms(100);
                }
            }
            None => {}
        }

        // Honour bond-clear flag (can also be set by future CLI command).
        if ble_hid::CLEAR_BONDS.load(Ordering::Relaxed) {
            display::show_status(disp, &sb, "Clearing...");
            FreeRtos::delay_ms(500);
            ble_hid::clear_bonds_and_reboot();
        }

        // CLI-driven factory reset.
        if cli::FACTORY_RESET.load(Ordering::Relaxed) {
            do_factory_reset("CLI", disp, &sb, fp, &nvs);
        }

        // CLI-driven enrollment: pick up a pending EnrollRequest from the CLI task.
        if let Ok(request) = enroll_rx.try_recv() {
            wake_screen_if_off(&mut screen_on, &mut inactivity_ticks, &mut backlight, fp);
            const PASSES: u8 = 3;
            display::show_status_2line(disp, &sb, "Place finger", &format!("pass 1/{}", PASSES));
            if fp.begin_enroll(request.slot, PASSES) {
                let mut pass = 0u8;
                loop {
                    match fp.poll_enroll_ack() {
                        fingerprint::EnrollAck::StartCapture => {
                            // Stage 0x01: sensor is waiting for a finger.
                            let _ = request.reply.send(cli::EnrollResp::PlaceFinger {
                                step: pass + 1,
                                total: PASSES,
                            });
                        }
                        fingerprint::EnrollAck::ImageOk => {
                            // Stage 0x02: image taken successfully.
                            pass += 1;
                            let _ = request.reply.send(cli::EnrollResp::LiftFinger {
                                step: pass,
                                total: PASSES,
                            });
                            if pass < PASSES {
                                display::show_status_2line(
                                    disp,
                                    &sb,
                                    "Lift finger",
                                    &format!("pass {}/{}", pass, PASSES),
                                );
                            } else {
                                display::show_status(disp, &sb, "Processing...");
                            }
                        }
                        fingerprint::EnrollAck::LiftOk => {
                            // Stage 0x03: finger lift detected.
                            if pass < PASSES {
                                display::show_status_2line(
                                    disp,
                                    &sb,
                                    "Place finger",
                                    &format!("pass {}/{}", pass + 1, PASSES),
                                );
                            }
                        }
                        fingerprint::EnrollAck::Done => {
                            let _ = request.reply.send(cli::EnrollResp::Done);
                            display::show_enroll_ok(disp, &sb, request.slot);
                            break;
                        }
                        fingerprint::EnrollAck::Failed => {
                            let _ = request.reply.send(cli::EnrollResp::Failed);
                            display::show_status_2line(disp, &sb, "Enroll", "Failed");
                            break;
                        }
                        fingerprint::EnrollAck::Pending => {}
                    }
                    FreeRtos::delay_ms(POLL_MS);
                }
            } else {
                let _ = request.reply.send(cli::EnrollResp::Failed);
                display::show_status_2line(disp, &sb, "Enroll", "Failed");
            }
            fp.reactivate();
            FreeRtos::delay_ms(2000);
            restore_idle_screen(disp, &sb, connected, pairing_open, passkey);
        }

        // CLI-driven fingerprint verify: pick up a pending VerifyRequest from the CLI task.
        if let Ok(request) = verify_rx.try_recv() {
            wake_screen_if_off(&mut screen_on, &mut inactivity_ticks, &mut backlight, fp);
            display::show_status_2line(disp, &sb, "CLI Auth", "Place finger");
            let deadline = std::time::Instant::now() + std::time::Duration::from_secs(30);
            let matched = loop {
                if std::time::Instant::now() > deadline {
                    break false;
                }
                match fp.poll() {
                    Some(fingerprint::IdentifyResult::Match(_)) => break true,
                    Some(fingerprint::IdentifyResult::NoMatch) => break false,
                    None => FreeRtos::delay_ms(POLL_MS),
                }
            };
            let _ = request.reply.send(matched);
            if matched {
                display::show_status(disp, &sb, "CLI Unlocked");
            } else {
                display::show_status(disp, &sb, "Auth Failed");
            }
            FreeRtos::delay_ms(1500);
            restore_idle_screen(disp, &sb, connected, pairing_open, passkey);
        }

        // Fingerprint — non-blocking poll; blocks ~20 ms only when a finger is detected.
        match fp.poll() {
            Some(fingerprint::IdentifyResult::Match(id)) => {
                wake_screen_if_off(&mut screen_on, &mut inactivity_ticks, &mut backlight, fp);

                let key = format!("slot_{}", id);
                let mut buf = [0u8; 65];
                let totp_result = {
                    let guard = config_store::lock_nvs(&nvs);
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
                            display::show_totp(disp, &sb, code);
                            if connected > 0 {
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
                restore_idle_screen(disp, &sb, connected, pairing_open, passkey);
            }
            Some(fingerprint::IdentifyResult::NoMatch) => {
                wake_screen_if_off(&mut screen_on, &mut inactivity_ticks, &mut backlight, fp);
                display::show_no_match(disp, &sb);
                FreeRtos::delay_ms(2000);
                restore_idle_screen(disp, &sb, connected, pairing_open, passkey);
            }
            None => {}
        }

        inactivity_ticks = inactivity_ticks.saturating_add(1);
        if screen_on && inactivity_ticks >= SCREEN_TIMEOUT_TICKS {
            backlight.set_low().ok();
            screen_on = false;
            fp.standby();
        }

        FreeRtos::delay_ms(if screen_on { POLL_MS } else { IDLE_POLL_MS });
    }
}
