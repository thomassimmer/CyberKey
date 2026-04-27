/// BM8563 RTC — I2C address and bus frequency
pub const RTC_I2C_ADDR: u8 = 0x51;
pub const I2C_FREQ_HZ: u32 = 400_000;

/// UART0 (USB-serial) baud rate
pub const UART_BAUD: u32 = 115_200;

/// ST7789V2 display — 135×240, landscape (Deg90)
pub const DISP_WIDTH: u16 = 135;
pub const DISP_HEIGHT: u16 = 240;
pub const DISP_OFFSET_X: u16 = 52;
pub const DISP_OFFSET_Y: u16 = 40;
pub const DISP_SPI_MHZ: u32 = 20;

/// Battery ADC — ÷2 voltage divider; 3300–4100 mV maps to 0–100 %
pub const BAT_ADC_DIVIDER: f32 = 2.0;
pub const BAT_MV_MIN: f32 = 3300.0;
pub const BAT_MV_RANGE: f32 = 800.0;
