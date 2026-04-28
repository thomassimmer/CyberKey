//! `menu` — interactive TUI menu for the CyberKey CLI.
//!
//! Renders a [`dialoguer`]-based main menu that loops until the user chooses
//! "Exit". Each menu item delegates to a dedicated action function that
//! communicates with the firmware through [`crate::device::Device`].

use dialoguer::{Confirm, Input, Password, Select, theme::ColorfulTheme};

use crate::device::Device;
use crate::display;
use crate::protocol::{Command, DeviceMessage, EnrollState};

// ── Menu entry-point ──────────────────────────────────────────────────────────

/// Runs the interactive menu loop until the user selects "Exit".
///
/// Each iteration shows the top-level `Select` prompt and dispatches to the
/// corresponding action handler. Errors returned by individual actions are
/// printed to stderr and the loop continues rather than exiting — the user
/// should not lose their session over a single failed command.
pub fn run(device: &mut Device) -> anyhow::Result<()> {
    let theme = ColorfulTheme::default();

    loop {
        let items = [
            "List configured fingers",
            "Add a new finger",
            "Remove a finger",
            "Sync device clock",
            "Allow BLE pairing",
            "Factory reset",
            "Exit",
        ];

        let choice = Select::with_theme(&theme)
            .with_prompt("What do you want to do?")
            .items(items)
            .default(0)
            .interact()?;

        println!();

        let result = match choice {
            0 => action_list(device),
            1 => action_add(device),
            2 => action_remove(device),
            3 => action_sync_clock(device),
            4 => action_allow_pairing(device),
            5 => action_factory_reset(device),
            6 => break,
            _ => unreachable!("Select returned an out-of-range index"),
        };

        if let Err(e) = result {
            eprintln!("  ✗ {e}");
        }

        println!();
    }

    Ok(())
}

// ── Action: List configured fingers ──────────────────────────────────────────

fn action_list(device: &mut Device) -> anyhow::Result<()> {
    match device.call(&Command::ListEntries)? {
        DeviceMessage::EntryList { entries } => {
            println!("{}", display::render_entries_table(&entries));

            if !entries.is_empty() {
                println!();
                println!(
                    "  Slot numbers are internal identifiers assigned automatically — they are not"
                );
                println!("  meaningful to the user. The associated finger is the only selector at");
                println!("  authentication time.");
            }
        }
        DeviceMessage::Error { error } => {
            println!("  ✗ {error}");
        }
        other => {
            anyhow::bail!("unexpected response from list_entries: {other:?}");
        }
    }

    Ok(())
}

// ── Action: Add a new finger ──────────────────────────────────────────────────

/// Maximum number of application-level enrollment retries before giving up.
const MAX_ENROLL_RETRIES: u8 = 3;

fn action_add(device: &mut Device) -> anyhow::Result<()> {
    let theme = ColorfulTheme::default();

    // ── Collect entry metadata ────────────────────────────────────────────────

    let label: String = Input::with_theme(&theme)
        .with_prompt("  Service name")
        .interact_text()?;

    let secret: String = Password::with_theme(&theme)
        .with_prompt("  TOTP secret (base32)")
        .interact()?;

    // Show the first 4 chars of the typed secret as a visual confirmation that
    // the user entered what they intended, without printing the full secret.
    println!("  → Secret preview: {}", display::mask_secret(&secret));
    println!("  → Slot assigned automatically (next available).");
    println!("  → Place your finger on the sensor when ready...");

    // ── Enrollment loop with up to MAX_ENROLL_RETRIES application retries ─────

    let mut assigned_slot: Option<u8> = None;

    for attempt in 1..=MAX_ENROLL_RETRIES {
        if attempt > 1 {
            let retry = Confirm::with_theme(&theme)
                .with_prompt("  Retry enrollment?")
                .default(true)
                .interact()?;

            if !retry {
                println!("  Enrollment cancelled.");
                return Ok(());
            }

            println!("  → Place your finger on the sensor when ready...");
        }

        match device.enroll(&label, &secret, &mut |step, total, state| {
            let state_label = match state {
                EnrollState::PlaceFinger => "Place finger",
                EnrollState::LiftFinger => "Lift finger ",
            };
            let bar = render_progress_bar(step, total, 10);
            println!("     [{step}/{total}] {state_label}  {bar}");
        }) {
            Ok(slot) => {
                assigned_slot = Some(slot);
                break;
            }
            Err(e) if attempt < MAX_ENROLL_RETRIES => {
                println!(
                    "  ✗ Enrollment failed ({e}). You have {} attempt(s) left.",
                    MAX_ENROLL_RETRIES - attempt
                );
            }
            Err(e) => {
                println!("  ✗ Enrollment failed after {MAX_ENROLL_RETRIES} attempts: {e}");
                return Ok(());
            }
        }
    }

    if let Some(slot) = assigned_slot {
        println!();
        println!("  ✓ Enrollment successful. \"{label}\" bound to slot {slot}.");
    }

    Ok(())
}

// ── Action: Remove a finger ───────────────────────────────────────────────────

fn action_remove(device: &mut Device) -> anyhow::Result<()> {
    let theme = ColorfulTheme::default();

    // Fetch the current entry list so the user can pick from it.
    let entries = match device.call(&Command::ListEntries)? {
        DeviceMessage::EntryList { entries } => entries,
        DeviceMessage::Error { error } => {
            println!("  ✗ {error}");
            return Ok(());
        }
        other => anyhow::bail!("unexpected response from list_entries: {other:?}"),
    };

    if entries.is_empty() {
        println!("  (no entries configured — nothing to remove)");
        return Ok(());
    }

    let labels: Vec<String> = entries
        .iter()
        .map(|e| format!("[{}] {}", e.slot, e.label))
        .collect();

    let idx = Select::with_theme(&theme)
        .with_prompt("  Select entry to remove")
        .items(&labels)
        .default(0)
        .interact()?;

    let chosen = &entries[idx];

    let confirmed = Confirm::with_theme(&theme)
        .with_prompt(format!(
            "  Remove \"{}\" (slot {})?",
            chosen.label, chosen.slot
        ))
        .default(false)
        .interact()?;

    if !confirmed {
        println!("  Cancelled.");
        return Ok(());
    }

    match device.call(&Command::RemoveEntry {
        label: chosen.label.clone(),
    })? {
        DeviceMessage::Ok => {
            println!("  ✓ \"{}\" removed successfully.", chosen.label);
        }
        DeviceMessage::Error { error } => {
            println!("  ✗ {error}");
        }
        other => {
            anyhow::bail!("unexpected response from remove_entry: {other:?}");
        }
    }

    Ok(())
}

// ── Action: Sync device clock ─────────────────────────────────────────────────

fn action_sync_clock(device: &mut Device) -> anyhow::Result<()> {
    let timestamp = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .context("system clock is before UNIX epoch")?
        .as_secs();

    let tz_offset_secs = chrono::Local::now().offset().local_minus_utc();

    match device.call(&Command::SyncClock {
        timestamp,
        tz_offset_secs,
    })? {
        DeviceMessage::Ok => {
            println!(
                "  ✓ Device clock synced (Unix timestamp {timestamp}, UTC offset {tz_offset_secs:+}s)."
            );
        }
        DeviceMessage::Error { error } => {
            println!("  ✗ {error}");
        }
        other => {
            anyhow::bail!("unexpected response from sync_clock: {other:?}");
        }
    }

    Ok(())
}

// ── Action: Factory reset ─────────────────────────────────────────────────────

fn action_factory_reset(device: &mut Device) -> anyhow::Result<()> {
    let theme = ColorfulTheme::default();

    println!();
    println!("  ! This will permanently erase all fingerprints and TOTP secrets.");
    println!();

    let confirmation: String = Input::with_theme(&theme)
        .with_prompt("  Type \"RESET\" to confirm")
        .interact_text()?;

    if confirmation != "RESET" {
        println!("  Cancelled (confirmation text did not match).");
        return Ok(());
    }

    match device.call(&Command::FactoryReset {
        confirm: "RESET".to_string(),
    })? {
        DeviceMessage::Ok => {
            println!("  ✓ Factory reset complete. The device is rebooting.");
            println!("  Exiting CLI.");
            // The device reboots immediately after this response; continuing the
            // session would cause every subsequent command to fail. Exit cleanly.
            std::process::exit(0);
        }
        DeviceMessage::Error { error } => {
            println!("  ✗ {error}");
        }
        other => {
            anyhow::bail!("unexpected response from factory_reset: {other:?}");
        }
    }

    Ok(())
}

// ── Action: Allow BLE pairing ─────────────────────────────────────────────────

fn action_allow_pairing(device: &mut Device) -> anyhow::Result<()> {
    match device.call(&Command::AllowPairing)? {
        DeviceMessage::Ok => {
            println!("  ✓ Pairing window is open.");
            println!("  Pair your host machine with the CyberKey device within 60 seconds.");
            println!("  The window closes automatically after one successful pairing.");
        }
        DeviceMessage::Error { error } => {
            println!("  ✗ {error}");
        }
        other => {
            anyhow::bail!("unexpected response from allow_pairing: {other:?}");
        }
    }

    Ok(())
}

// ── Helpers ───────────────────────────────────────────────────────────────────

/// Renders a simple block-character progress bar.
///
/// `step` is the 1-based current capture pass (≥ 1).
/// `total` is the total number of passes (> 0).
/// `width` is the number of characters in the bar.
///
/// Example: `step=1, total=3, width=10` → `"███░░░░░░░"`
fn render_progress_bar(step: u8, total: u8, width: usize) -> String {
    if total == 0 {
        return "░".repeat(width);
    }
    let filled = (step as usize * width) / total as usize;
    let empty = width.saturating_sub(filled);
    format!("{}{}", "█".repeat(filled), "░".repeat(empty))
}

// ── `use` for anyhow::Context in action_sync_clock ───────────────────────────
use anyhow::Context;

// ── Tests ─────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    // ── render_progress_bar ───────────────────────────────────────────────────

    #[test]
    fn progress_bar_first_step_of_three() {
        // step 1/3, width 10 → ceil(1*10/3) = 3 filled, 7 empty
        let bar = render_progress_bar(1, 3, 10);
        assert_eq!(
            bar.len(),
            10 * 3,
            "bar must be exactly `width` code-points × 3 bytes per char"
        );
        // Count filled (█) and empty (░) segments.
        let filled: usize = bar.chars().filter(|&c| c == '█').count();
        let empty: usize = bar.chars().filter(|&c| c == '░').count();
        assert_eq!(filled + empty, 10, "total char count must equal width");
        assert!(filled > 0, "at least one filled segment at step 1/3");
        assert!(empty > 0, "at least one empty segment at step 1/3");
    }

    #[test]
    fn progress_bar_last_step_fully_filled() {
        // step == total → fully filled bar.
        let bar = render_progress_bar(3, 3, 10);
        let filled: usize = bar.chars().filter(|&c| c == '█').count();
        assert_eq!(filled, 10, "bar must be completely full at final step");
    }

    #[test]
    fn progress_bar_zero_total_returns_empty_bar() {
        // Guard against division by zero when total is 0.
        let bar = render_progress_bar(0, 0, 8);
        let empty: usize = bar.chars().filter(|&c| c == '░').count();
        assert_eq!(empty, 8);
    }

    #[test]
    fn progress_bar_width_zero() {
        let bar = render_progress_bar(1, 3, 0);
        assert!(bar.is_empty());
    }

    #[test]
    fn progress_bar_monotonically_increases() {
        // For each successive step the filled portion must be ≥ the previous.
        let total = 3u8;
        let width = 10;
        let mut prev_filled = 0usize;
        for step in 1..=total {
            let bar = render_progress_bar(step, total, width);
            let filled: usize = bar.chars().filter(|&c| c == '█').count();
            assert!(
                filled >= prev_filled,
                "filled count must not decrease: step {step}/{total}"
            );
            prev_filled = filled;
        }
    }
}
