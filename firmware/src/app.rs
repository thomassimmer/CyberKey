//! Main event loop — BLE state, buttons, fingerprint, enrollment, screen timeout.

use std::sync::{atomic::Ordering, Arc, Mutex};

use esp_idf_svc::hal::{
    delay::FreeRtos,
    gpio::{InputPin, Output, OutputPin, PinDriver},
    i2c::I2cDriver,
};

use crate::{ble_hid, buttons::ButtonEvent, cli, config_store, display, fingerprint, rtc};

/// Boot-time check: if Button A is held for 5 s, prompt for a second press to confirm,
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
    const HOLD_TARGET: u32 = 5_000 / POLL_MS; // 250 polls = 5 s

    let mut hold: u32 = 0;
    while buttons.is_a_down() && hold < HOLD_TARGET {
        hold += 1;
        FreeRtos::delay_ms(POLL_MS);
    }
    if hold < HOLD_TARGET {
        return;
    }

    display::show_status_2line(disp, sb, "Factory Reset?", "Press A again");

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
        display::show_status(disp, sb, "Resetting...");
        fp.empty_template_library();
        log::info!("Factory reset: fingerprint templates cleared");
        {
            let mut guard = nvs.lock().unwrap();
            for slot in 0u32..10 {
                let _ = guard.0.remove(&format!("slot_{slot}"));
                let _ = guard.0.remove(&format!("label_{slot}"));
            }
        }
        log::warn!("Factory reset: complete — rebooting");
        display::show_reset_ok(disp, sb);
        FreeRtos::delay_ms(2000);
        unsafe { esp_idf_svc::sys::esp_restart() }
    }
}

#[allow(clippy::too_many_arguments)]
pub fn run<D, A, B, C, P, BL, F>(
    ble: &ble_hid::BleHid,
    disp: &mut D,
    mut buttons: crate::buttons::Buttons<'_, A, B, C>,
    passkey: u32,
    mut power_pin: PinDriver<'_, P, Output>,
    mut backlight: PinDriver<'_, BL, Output>,
    fp: &mut fingerprint::FingerprintSensor<'_>,
    nvs: Arc<Mutex<config_store::SharedNvs>>,
    enroll_queue: cli::EnrollQueue,
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
    let mut last_connected = false;
    let mut pending_bond_clear = false;

    let mut battery: Option<u8> = read_battery();
    let mut last_battery_tick: u32 = 0;
    let mut last_minute: u8 = 255; // force topbar draw on first iteration
    let mut tick: u32 = 0;

    const SCREEN_TIMEOUT_TICKS: u32 = 1_500; // 30 s at 20 ms/tick
    let mut inactivity_ticks: u32 = 0;
    let mut screen_on = true;

    let sb_boot = display::StatusBar::unknown();
    check_boot_factory_reset(&buttons, disp, &sb_boot, fp, &nvs);

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
            if connected {
                display::show_status(disp, &sb, "Connected");
            } else {
                display::show_pin(disp, &sb, passkey);
            }
        }

        let btn_event = buttons.poll();
        if btn_event.is_some() {
            inactivity_ticks = 0;
            if !screen_on {
                backlight.set_high().ok();
                screen_on = true;
            }
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
                    if connected {
                        display::show_status(disp, &sb, "Connected");
                    } else {
                        display::show_pin(disp, &sb, passkey);
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
                display::show_status(disp, &sb, "Powering off...");
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
            display::show_status(disp, &sb, "Clearing...");
            FreeRtos::delay_ms(500);
            ble_hid::clear_bonds_and_reboot();
        }

        // CLI-driven factory reset: erase fingerprint templates then reboot.
        if cli::FACTORY_RESET.load(Ordering::Relaxed) {
            log::warn!("Factory reset: triggered via CLI");
            display::show_status(disp, &sb, "Resetting...");
            log::info!("Factory reset: clearing fingerprint templates...");
            fp.empty_template_library();
            log::info!("Factory reset: fingerprint templates cleared");
            log::warn!("Factory reset: complete — rebooting");
            display::show_reset_ok(disp, &sb);
            FreeRtos::delay_ms(2000);
            unsafe { esp_idf_svc::sys::esp_restart() }
        }

        // CLI-driven enrollment: pick up a pending EnrollRequest from the CLI task.
        if let Ok(mut eq) = enroll_queue.try_lock() {
            if let Some(request) = eq.take() {
                drop(eq); // release lock before the blocking enrollment loop
                inactivity_ticks = 0;
                if !screen_on {
                    backlight.set_high().ok();
                    screen_on = true;
                }
                const PASSES: u8 = 3;
                display::show_status_2line(disp, &sb, "CLI Enroll", "Place finger");
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
                                        &sb,
                                        "Lift + replace",
                                        &format!("pass {}/{}", pass + 1, PASSES),
                                    );
                                } else {
                                    display::show_status(disp, &sb, "Processing...");
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
                        FreeRtos::delay_ms(20);
                    }
                } else {
                    let _ = request.reply.send(cli::EnrollResp::Failed);
                    display::show_status_2line(disp, &sb, "Enroll", "Failed");
                }
                fp.reactivate();
                FreeRtos::delay_ms(2000);
                if connected {
                    display::show_status(disp, &sb, "Connected");
                } else {
                    display::show_pin(disp, &sb, passkey);
                }
            }
        }

        // Fingerprint — non-blocking poll; blocks ~20 ms only when a finger is detected.
        match fp.poll() {
            Some(fingerprint::IdentifyResult::Match(id)) => {
                inactivity_ticks = 0;
                if !screen_on {
                    backlight.set_high().ok();
                    screen_on = true;
                }
                display::show_auth_ok(disp, &sb, id);

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
                            display::show_totp(disp, &sb, code);
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
                    display::show_status(disp, &sb, "Connected");
                } else {
                    display::show_pin(disp, &sb, passkey);
                }
            }
            Some(fingerprint::IdentifyResult::NoMatch) => {
                inactivity_ticks = 0;
                if !screen_on {
                    backlight.set_high().ok();
                    screen_on = true;
                }
                display::show_no_match(disp, &sb);
                FreeRtos::delay_ms(2000);
                if connected {
                    display::show_status(disp, &sb, "Connected");
                } else {
                    display::show_pin(disp, &sb, passkey);
                }
            }
            None => {}
        }

        inactivity_ticks = inactivity_ticks.saturating_add(1);
        if screen_on && inactivity_ticks >= SCREEN_TIMEOUT_TICKS {
            backlight.set_low().ok();
            screen_on = false;
        }

        FreeRtos::delay_ms(20);
    }
}
