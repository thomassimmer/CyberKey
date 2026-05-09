# CyberKey: What I learned building an embedded project coming from web development

I'm a fullstack web developer with 6 years of experience. Python, Rust, JS, databases, and APIs. That's my day job. I had never touched electronics.

A few weeks ago, I decided to build CyberKey. The itch came from something boring at work: my VPN disconnects when I lock my computer, and I have to type a TOTP code several times a day. Unlock my phone, open the authenticator app, read the code, type it before it expires. Every time. CyberKey is a small device that eliminates that friction. Place the right finger on it, and the code is typed automatically over Bluetooth. No app, no phone, no copy-paste. Just a finger, and the code appears in the input field.

The project runs on an M5StickC Plus 2, an ESP32 microcontroller the size of a lighter. It's made by M5Stack, a company that produces ESP32-based modules with a screen, a battery, and built-in connectors, as well as a range of compatible sensors and peripherals. The fingerprint sensor in CyberKey is one of theirs. It's a reasonable entry point for software developers who don't want to deal with breadboards and soldering. The firmware (the program that runs directly on the chip, with no operating system underneath) is written in Rust. I was heavily assisted by Claude throughout the development, and most of the work happened during my baby's nap times, roughly two hours a day. Without that help, I couldn't have pulled this off in a reasonable amount of time.

This is not a tutorial. It's a synthesis of the concepts I had never encountered in six years of web development, and a walkthrough of the firmware architecture, for web developers curious about what's on the other side.

---

## The hardware: when your code talks to physics

The first thing that caught me off guard is how different a microcontroller is from anything I had worked with before. A server has an operating system under it: a scheduler, a filesystem, a networking stack, a memory allocator. You write code on top of all that. A microcontroller has none of it. You get a chip, some flash memory, and a few hundred kilobytes of RAM. Whatever your program needs to do, it has to set up itself.

The most concrete expression of this is the **GPIO** pins (General Purpose Input/Output). These are the physical legs of the chip. Each pin is connected to a wire on the board, and your code can set it high (3.3V) or low (0V), or read its current state. A boolean, but made of electricity. Turning on an LED is literally setting a pin to `true`. Reading a button press is reading a pin's value in a loop.

To make chips talk to each other, the embedded world uses a small set of standard protocols. The three I used in CyberKey are **UART**, **I2C**, and **SPI**. The analogy that clicked for me is that they fill roughly the same role as different network protocols in web development: each is a tradeoff between simplicity, speed, and the number of devices you can connect.

UART is the simplest: two wires, two devices, no shared clock. Both sides agree in advance on a speed (baud rate) and just send bits. It's what's behind the USB serial port you use to flash and debug a board. I2C uses only two wires but supports many devices on the same bus, each with an address, like an IP. It's slower, but perfect for sensors and clocks that don't need high throughput. SPI is the fastest: four wires, a dedicated clock, and it can push data at 80 MHz, which is why it's used for displays.

What surprised me most was the hierarchy behind SPI. It has four wires: CLK (clock), MOSI (data out), MISO (data in), and CS (chip select). The first three form the **bus**, physically shared between all components like a single cable soldered to multiple chips at once. When the ESP32 sends a clock signal, every connected chip sees it simultaneously. The fourth wire, CS, is what makes one chip respond and not the others: when CS is pulled low for a specific chip, that chip listens; the rest ignore it.

In code, this maps to a clean hierarchy: a **driver** manages the three shared wires, and a **device** wraps that driver together with one specific CS pin to represent a single component. If you had a display and an SD card on the same bus, you'd have one driver and two devices, one per CS pin. The word "bus" is no accident. It's the same metaphor as a city bus, a shared route that multiple passengers can board, each getting off at their own stop.

---

## The firmware architecture

A web server entry point does a lot before it starts serving requests: it connects to the database, registers middleware, sets up routes, starts a background job queue. But underneath all of that, there's an operating system managing memory and scheduling, a runtime handling I/O, a framework providing the event loop. You're building on top of layers that already exist.

In embedded, those layers don't exist. `main()` is not a starting point. It's the entire program. In CyberKey's firmware, `main()` is a long sequential initialization function:

- **Power pin**: a GPIO that must be held high immediately, or the board shuts off when you release the button
- **Battery ADC**: the analog-to-digital converter that reads the battery voltage; the chip only understands numbers, not voltages, so it needs hardware to translate
- **UART for the CLI**: the serial connection to a laptop, used to enroll fingerprints and sync the clock over USB
- **I2C for the real-time clock**: so the device knows what time it is to generate valid TOTP codes
- **SPI for the display**: the screen that shows the current status, the TOTP code, and the BLE pairing PIN
- **BLE**: Bluetooth Low Energy, the wireless protocol that makes the device appear as a keyboard to a computer
- **Fingerprint sensor**: connected via UART on the Grove port (a standardized connector from M5Stack that carries power and a communication protocol in a single plug, no soldering required)

Each component needs its own protocol configuration, its own pins, its own driver. Only once everything is initialized does control pass to the main loop.

The order matters. Initializing the display controller before completing its hardware reset sequence produces a black screen. Powering up BLE before the SPI bus is ready causes a crash. There's no framework catching your mistakes. If the sequence is wrong, the device just doesn't work, often without any error message.

After initialization, the firmware runs a loop that never exits. It checks the buttons, handles BLE events, listens for fingerprint matches, updates the display, reads the battery level. This is the event loop you write yourself.

One thing that has no equivalent in web development is power management. A device running on a 200 mAh battery drains fast, and every component you leave running costs you runtime. The ESP32 has sleep modes that should bring power down dramatically between uses. I spent a fair amount of time trying to make them work — and couldn't. For reasons I never fully pinned down, the chip never actually slept. The simplest solution that did work: pressing button C cuts power entirely by driving a GPIO low. The board shuts off instantly, draws nothing, and reconnects to bonded hosts automatically in a few seconds on next boot.

---

## Rust in embedded: between C++ and nothing

Rust is not the dominant language in embedded. C and C++ are. The official ESP32 SDK from Espressif (called ESP-IDF) is written in C. M5Stack's official drivers are in C++. Most of the community, the tutorials, the examples: C and C++.

What makes Rust usable here is a layer of crates that wrap the ESP-IDF C code and expose it with a Rust API. When I call a function to read the battery voltage, it's Rust on the front but C in the back. You get the safety guarantees of Rust, but you're still standing on a foundation written in C.

The practical consequence is that reading C++ code became part of the workflow. To understand how a component behaves, the most reliable source is often the official C++ driver: which pin to toggle, in what order, with what timing. For the fingerprint sensor, M5Stack publishes an Arduino driver in C++. I read it, understood the UART communication protocol it implements, and rewrote it from scratch in Rust. Not because Rust required it, but because no Rust driver existed for that sensor.

This gave me a crate I called `fingerprint2-rs`, compiled with `no_std`. The `no_std` annotation tells the compiler this code cannot rely on the standard library, which assumes an operating system underneath. No OS, no standard library, no runtime overhead.

As for what Rust concretely brings: the compiler enforces error handling at every step, which matters when a failed I2C read can silently corrupt your state. Memory is managed explicitly, without a garbage collector. With a few hundred kilobytes to work with, that matters. I won't oversell it: Rust didn't make the hardware easier to understand. But it made the code easier to trust once it compiled.

---

## What web development doesn't prepare you for

**No hot reload.** Every change on the firmware follows the same cycle: edit the code, compile, flash the binary onto the device over USB, wait for it to boot, observe. A full iteration takes around a minute. You learn quickly to think before you type.

**A crash can brick the device.** In web development, an unhandled exception prints a stack trace and the process restarts. In embedded, bad firmware can leave the device in an infinite reboot loop with no output, or completely unresponsive. Bricking means the device becomes as useful as a brick: it won't boot, and recovering it requires a specific flashing procedure if it's even possible. It never happened to me on this project, but the possibility shapes how carefully you test each change.

**Memory is not elastic.** The ESP32 has 520 KB of internal RAM. No heap growth, no swap, no "just add more". Every allocation is a decision. This is where `no_std` earns its place: memory usage becomes explicit and predictable by design.

**The battery is always on your mind.** In web development, energy consumption is invisible, it's the cloud provider's problem. On a device running on a 200 mAh battery, every component you leave running costs you autonomy. It forces a different way of thinking about every architectural decision, one I hadn't anticipated at all coming from web.

---

## Developing with an AI as co-pilot

I mentioned it in the intro, but it's worth being specific: I relied heavily on Claude throughout this project. Not as a code generator I blindly trusted, but as a way to stay unblocked. When you have two hours before your baby wakes up, you can't afford to spend forty-five minutes figuring out why your I2C bus is hanging. Having something that can explain the concept, point you to the relevant part, and sketch a direction changes the pace completely.

What it doesn't replace is the decisions that require judgment. I drove the direction, the features, the architecture, the quality bar. When something didn't work, I tested on the device, read the terminal output, and went looking for answers in M5Stack's official documentation, their GitHub repositories, and a few open-source projects built on the same hardware. Several times that's what finally unblocked us, not the AI. And sometimes it was just intuition that turned out to be right.

The risk I ran into: moving too fast. At several points I implemented something that worked, shipped it, and moved on without fully understanding what I had just written. It caught up with me later when something broke and I had to re-read my own code like it was someone else's.

---

## Results and takeaways

The device works. Place an enrolled finger, the sensor matches it, the TOTP code appears on the screen and is typed over Bluetooth in under a second. BLE pairing, the display, the real-time clock, the USB CLI: all of it functions as intended. For a first embedded project, I'm happy with where it landed.

The one disappointment is battery life. With a 200 mAh battery, the device lasts around 3–4 hours. I tried a lot of things to improve that — sleep modes, sensor standby, various combinations, and none of it moved the needle in any meaningful way. I never fully understood why. For now, the pragmatic solution is a hard power-off: one press of button C cuts all power at the hardware level, and the device reconnects to bonded hosts automatically on next boot. Not elegant, but honest. If you have an idea that doesn't require hardware changes, I'd genuinely love to see a pull request.

If I did this again, I'd take more time upfront to understand the core concepts before writing any code. The moments where I got stuck hardest were always the moments where I had skipped the fundamentals. I'd also add logs from the very beginning, debugging embedded hardware without them means staring at a silent device and guessing.

The broader takeaway: embedded development is accessible to a web developer. The concepts are unfamiliar, but they're learnable, and the instincts for clean architecture and readable code transfer well. It's just a different kind of disorienting than picking up a new framework.

---

## One more thing: Cyberpunk 2077

Part of what drives this project is Cyberpunk 2077. I played it two years ago, before my kid was born and, it was the original inspiration. CyberKey only borrows the aesthetic: the UI typeface, the color palette. But the broader idea is to build things that exist in that world and don't exist yet in ours, with off-the-shelf hardware and a reasonable amount of work.

If there's a piece of technology from the game you'd want to see built for real, I'd be curious to know. That's where I'm going next.
