# Hardware

## Board: M5StickC Plus 2

The ESP32-PICO-V3-02 is the SoC: dual-core Xtensa LX6 at 240 MHz, 8 MB flash, 2 MB PSRAM.
Everything else — display, RTC, battery, buttons — is integrated on the M5StickC Plus 2 carrier board.

---

## GPIO Map

| Peripheral | Signal | GPIO | Notes |
|-----------|--------|------|-------|
| UART0 (USB-C) | TX | 1 | CLI protocol, 115.2k bps |
| UART0 (USB-C) | RX | 3 | |
| UART1 (Grove) | TX | 32 | Fingerprint sensor |
| UART1 (Grove) | RX | 33 | |
| SPI2 display | CLK | 13 | ST7789V2, 40 MHz |
| SPI2 display | MOSI | 15 | |
| SPI2 display | CS | 5 | |
| Display | DC | 14 | data/command select |
| Display | RST | 12 | |
| Display | backlight | 27 | PWM or GPIO high/low |
| I2C0 (RTC) | SDA | 21 | BM8563, 400 kHz, addr 0x51 |
| I2C0 (RTC) | SCL | 22 | |
| Button A | input | 37 | active-low, external pull-up |
| Button B | input | 39 | active-low, external pull-up |
| Button C (power) | input | 35 | active-low, external pull-up |
| Battery ADC | ADC1 | 38 | ÷2 voltage divider |
| Power hold | output | 4 | must stay high or board shuts off |

GPIO 37, 39, and 35 are input-only pins on the ESP32 — no internal pull-up available. The board provides external pull-ups. Always configure them as plain `Input` (not `InputPullup`).

---

## Peripherals

### ST7789V2 Display (135×240, SPI)

Landscape orientation (`Deg90` in mipidsi). Colors are RGB565.

The library is `mipidsi` + `embedded-graphics`. The display is not double-buffered in hardware — every `draw_*` call goes directly to the controller. To avoid flickering, clear only what changed rather than full-screen fills on each frame.

The firmware reserves the top 20 px as a status bar (time + battery) and uses the remaining 115 px for content.

### BM8563 RTC (I2C)

The BM8563 keeps time when the ESP32 is unpowered. It has a VL ("voltage low") flag that is set when the backup power has been lost — the firmware checks this at boot and falls back to the compile-time `BUILD_TIME` constant if set.

The I2C bus is owned by the main loop. The CLI task cannot call I2C directly; it writes the desired Unix timestamp into a `PENDING_RTC_WRITE` mutex, and the main loop flushes it on the next tick.

### M5Stack Fingerprint2 Sensor (UART1)

Connects over Grove HY2.0 on UART1 (TX=32, RX=33). The sensor contains an STM32 MCU that runs the matching algorithm internally; the ESP32 only sends commands and receives results.

See [fingerprint2-rs driver docs](../crates/fingerprint2-rs/src/driver.rs) for the packet protocol. The key point: when a finger is detected autonomously, the sensor emits a fixed 12-byte wakeup packet before the ESP32 has issued any command. The firmware checks for this exact sequence before attempting to parse a response.

---

## Button Polling (not interrupts)

Buttons are polled every 20 ms from the main loop. A timer tracks how long a button has been held — 1.5 seconds triggers a long-press event.

**Why polling and not GPIO interrupts?**

Interrupts would seem faster, but for buttons they create subtle problems:

1. **Debounce**: physical buttons bounce electrically for ~5 ms. An ISR fires on every edge, producing multiple events from a single press. Debouncing in an ISR requires a timer, which means more state and more complexity.
2. **Long-press detection**: detecting "held for 3 seconds" in an ISR requires either a FreeRTOS timer or signaling a task, which is equivalent to polling with extra steps.
3. **Race conditions**: GPIO ISRs run in interrupt context (no FreeRTOS primitives allowed) and must communicate with the main loop via volatile flags or queues — more error-prone than simply reading the pin in the main loop.

At 20 ms, the polling rate is faster than any deliberate human action. The complexity cost of interrupts is not worth it here.

---

## Power Management

### Budget

| State | ESP32 | Fingerprint sensor | Total | Notes |
|-------|-------|--------------------|-------|-------|
| Light sleep | ~0.8 mA | ~14 mA | ~15 mA | UART wakeup enabled |
| Active (BLE + display) | ~80 mA | ~14 mA | ~95 mA | Brief bursts (~2 s) |
| Charging | — | — | — | 500 mA max via USB-C |

**Battery: ~200 mAh.** At 15 mA idle, that is roughly 13 hours of standby.

The fingerprint sensor cannot be put to sleep independently in the current hardware configuration (no GPIO power control on Grove). It consumes ~14 mA continuously and dominates the idle budget.

### Light Sleep vs. Deep Sleep

The ESP32 has two low-power modes:

- **Light sleep**: CPU halts, RAM retained, peripherals optionally active. Wakeup sources include UART RX activity. Current: ~0.8 mA (ESP32 core alone).
- **Deep sleep**: CPU + RAM off, only the RTC domain is active. Current: ~0.01 mA (ESP32 core alone). Wakeup is limited to GPIO edges, timers, and ULP programs — **not UART activity**.

**Why light sleep (not deep sleep)?**

The fingerprint sensor sends a wakeup packet over UART1 when it detects a finger autonomously. That requires UART RX to remain active, which rules out deep sleep. Light sleep with UART wakeup is the simplest way to stay responsive to the sensor without burning full active-mode power between authentication attempts.

Deep sleep would save ~0.8 mA on the ESP32 side, but the fingerprint sensor still draws 14 mA, making the net saving marginal (~5%). The added complexity (GPIO edge wakeup, state reconstruction after wake) is not worth it for v0.1.
