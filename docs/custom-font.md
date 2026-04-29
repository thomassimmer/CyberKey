# Custom Fonts for CyberKey

This document explains how to convert a vector font (`.ttf`) into a proportional font usable by the firmware via `embedded-graphics`.

## 1. Prerequisites
You need the following tools on your machine (macOS):
*   **otf2bdf**: To transform TTF into bitmap (`brew install otf2bdf`).
*   **Python 3**: To transform BDF into Rust code.

## 2. Conversion Process

### Step A: TTF to BDF
The font must be rasterized at a specific pixel size.
```bash
# Example for a size of 12 pixels
otf2bdf -p 12 Orbitron-Regular.ttf -o orbitron_12.bdf
```

### Step B: BDF to Rust (Proportional)
Use the `tools/gen_prop_font.py` script included in the `firmware/` directory. Unlike standard tools that force a grid (monospacing), this script extracts the actual width of each character.

```bash
# Run from the firmware directory
python3 tools/gen_prop_font.py orbitron_12.bdf src/fonts/orbitron_font.rs
```

## 3. System Structure
The project currently uses three sizes:
1.  **Mini (size 10)**: `orbitron_mini.rs` - Used for the status bar.
2.  **Regular (size 12)**: `orbitron_font.rs` - Used for titles and messages.
3.  **Large (size 20)**: `orbitron_large.rs` - Used for security codes (PIN and TOTP).

## 4. Usage in Rust
Generated fonts are not standard `MonoFont` objects but use a custom drawing function to handle proportional spacing.

```rust
use crate::orbitron_font::{draw_text_prop, get_text_width};

// Draw text
draw_text_prop(display, "HELLO", Point::new(x, y), color)?;

// Calculate width for centering
let width = get_text_width("HELLO");
```

## 5. Maintenance
If you want to change the font:
1.  Obtain the `.ttf` file.
2.  Repeat steps A and B for the three sizes (p=10, p=12, p=20).
3.  Compile and flash.
