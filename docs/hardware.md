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
| SPI2 display | CLK | 13 | ST7789V2, 20 MHz |
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

### Current budget

The device draws roughly **~50 mA** in normal operation (ESP32 with BLE active + fingerprint sensor + display). With a 200 mAh battery, that gives around 3–4 hours of runtime.

### Power-off strategy

Pressing button C drives GPIO4 (power hold) low. The M5StickC Plus 2 uses GPIO4 as a self-hold latch — once it goes low, the board cuts power immediately. This is a hardware power cut, not a sleep mode.

NVS bonds survive the power cut (stored in flash). On next boot the firmware reconnects to bonded hosts automatically without user interaction.

### Contributing: improving battery life

Several approaches were attempted to reduce idle current (ESP-IDF light sleep, fingerprint sensor standby, various combinations) but none produced meaningful results. The root cause was never fully isolated.

If you have an idea that doesn't require hardware modifications, pull requests are welcome.
