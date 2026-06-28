//! Gameboy PPU and LCD handling
//!
//! # Overview
//!
//! ## Background
//!
//! The Gameboy screen buffer holds 256x256 pixels, or 32x32 *tiles*.
//! However, the LCD only displays 160x144 pixels at a time.
//!
//! `SCROLLX` and `SCROLLY` registers contain the location of the background at the
//! upper left of the screen (i.e., it is scrollable). Note that the background
//! wraps around the screen edges.
//!
//! The *Background Tile Map* contains 32 rows of 32 bytes each. Each byte
//! maps to a tile number. Tiles are stored in the *Tile Data Table* in VRAM
//! in one of these two regions:
//!
//! * 0x8000-0x8FFF (tile number = 0 to 255)
//! * 0x8800-0x97FF (tile number = -128 to 127, 0th tile at 0x9000)
//!
//! The region is set/modified using the LCDC register.
//!
//! *BG Display Data*, or the actual content of the background (256 x 256 pixels),
//! is stored at either:
//!
//! * 0x9800-0x9BFF
//! * 0x9C00-0x9FFF
//!
//! The region is set using bit 3 of the LCDC register.
//!
//! The aforementioned scroll registers determine which area of the BG is displayed
//! on the 160x144 LCD.
//!
//! ## Window
//!
//! WX and WY control where the window is displayed on the LCD. Note that the window
//! does not wrap and is not scrollable.
//!
//! ## LCD
//!
//! Each row of 160 pixels takes 108.7 us to display. If you multiply that by 144 rows,
//! the total display time is ~15.66 ms.
//!
//! Once the frame is displayed, the VBLANK period lasts 10 lines, which maps to ~1.09 ms.
//! This is when VRAM data can be accessed.
//!
//! The combination of these two periods nets us ~60 fps.
use crate::gbc::cpu::Interrupt;
use crate::gbc::memory::{MemoryRead, MemoryWrite};

pub const LCD_WIDTH: usize = 160;
pub const LCD_HEIGHT: usize = 144;

static COLOR_SCALE_LUT: [u8; 32] = [
    0, 8, 16, 24, 32, 41, 49, 57, 65, 74, 82, 90, 98, 106, 115, 123,
    131, 139, 148, 156, 164, 172, 180, 189, 197, 205, 213, 222, 230, 238, 246, 255
];

#[derive(Clone, Copy, Debug)]
pub struct GameboyRgb {
    pub red: u8,
    pub green: u8,
    pub blue: u8,
}

impl GameboyRgb {
    pub fn white() -> Self {
        Self {
            red: 0xFF,
            green: 0xFF,
            blue: 0xFF,
        }
    }

    /// Scale this color to regular RGB (0-255).
    ///
    /// Uses precomputed LUT for maximum speed.
    #[inline(always)]
    pub fn scale_to_rgb(&mut self) {
        self.red = COLOR_SCALE_LUT[self.red as usize];
        self.green = COLOR_SCALE_LUT[self.green as usize];
        self.blue = COLOR_SCALE_LUT[self.blue as usize];
    }

    pub fn save_to_bytes(&self, buf: &mut Vec<u8>) {
        buf.push(self.red);
        buf.push(self.green);
        buf.push(self.blue);
    }

    pub fn load_from_bytes(data: &[u8], pos: &mut usize) -> crate::gbc::error::Result<Self> {
        let red = data[*pos]; *pos += 1;
        let green = data[*pos]; *pos += 1;
        let blue = data[*pos]; *pos += 1;
        Ok(Self { red, green, blue })
    }
}

// Basic DMG/monochrome color palette
static DMG_PALETTE: [GameboyRgb; 4] = [
    // White
    GameboyRgb {
        red: 0xE0,
        green: 0xF8,
        blue: 0xD0,
    },
    // Light gray
    GameboyRgb {
        red: 0x88,
        green: 0xC0,
        blue: 0x70,
    },
    // Dark gray
    GameboyRgb {
        red: 0x34,
        green: 0x68,
        blue: 0x56,
    },
    // Black
    GameboyRgb {
        red: 0x08,
        green: 0x18,
        blue: 0x20,
    },
];

/// Buffer that holds pixel data for a single frame.
pub struct FrameBuffer {
    pub data: Box<[GameboyRgb]>,
    pub(crate) ready: bool,
}

impl FrameBuffer {
    pub fn new() -> Self {
        Self {
            data: Box::new([GameboyRgb::white(); LCD_WIDTH * LCD_HEIGHT]),
            ready: false,
        }
    }

    /// Read a single pixel from the buffer.
    ///
    /// `x` is the "column", `y` is the "row".
    #[inline(always)]
    pub fn read(&self, x: usize, y: usize) -> GameboyRgb {
        self.data[y * LCD_WIDTH + x]
    }

    /// Write a single pixel to the buffer.
    ///
    /// `x` is the "column", `y` is the "row".
    #[inline(always)]
    pub fn write(&mut self, x: usize, y: usize, pixel: GameboyRgb) {
        self.data[y * LCD_WIDTH + x] = pixel;
    }

    pub fn save_to_bytes(&self, buf: &mut Vec<u8>) {
        buf.extend_from_slice(&(self.data.len() as u32).to_le_bytes());
        for pixel in self.data.iter() {
            pixel.save_to_bytes(buf);
        }
        buf.push(self.ready as u8);
    }

    pub fn load_from_bytes(data: &[u8], pos: &mut usize) -> crate::gbc::error::Result<Self> {
        let data_len = u32::from_le_bytes([data[*pos], data[*pos+1], data[*pos+2], data[*pos+3]]); *pos += 4;
        let mut pixels = Vec::with_capacity(data_len as usize);
        for _ in 0..data_len {
            pixels.push(GameboyRgb::load_from_bytes(data, pos)?);
        }
        let ready = data[*pos] != 0; *pos += 1;
        Ok(Self { data: pixels.into_boxed_slice(), ready })
    }
}


const BANK_SIZE: usize = 0x2000;
const TILE_DATA_SIZE: usize = 0x1800;
const TILES_PER_BANK: usize = TILE_DATA_SIZE / 16;
const TILE_CACHE_STRIDE: usize = 64;

pub struct Vram {
    bank0: Box<[u8; BANK_SIZE]>,
    bank1: Box<[u8; BANK_SIZE]>,
    tile_cache: Box<[u8]>,
    pub active_bank: u8,
    cgb: bool,
}

impl Vram {
    pub const BASE_ADDR: u16 = 0x8000;
    pub const LAST_ADDR: u16 = 0x9FFF;
    pub const BANK_SELECT_ADDR: u16 = 0xFF4F;

    pub fn new(cgb: bool) -> Self {
        let cache_size = TILES_PER_BANK * TILE_CACHE_STRIDE * 2;
        Self {
            bank0: Box::new([0u8; BANK_SIZE]),
            bank1: Box::new([0u8; BANK_SIZE]),
            tile_cache: vec![0u8; cache_size].into_boxed_slice(),
            active_bank: 0,
            cgb,
        }
    }

    pub fn update_bank(&mut self, bank: u8) {
        let bank = bank & 0x1;

        if !self.cgb {
            return;
        }

        self.active_bank = bank;
    }

    #[inline(always)]
    pub fn read_bank(&self, bank: u8, addr: u16) -> u8 {
        let idx = (addr - Self::BASE_ADDR) as usize;
        match bank {
            0 => self.bank0[idx],
            _ => self.bank1[idx],
        }
    }

    #[allow(dead_code)]
    pub fn get_bank_slice(&self, bank: u8, start_addr: u16, length: usize) -> &[u8] {
        let start = (start_addr - Self::BASE_ADDR) as usize;
        match bank {
            0 => &self.bank0[start..start + length],
            _ => &self.bank1[start..start + length],
        }
    }

    #[inline(always)]
    pub fn read_tile_pixel(&self, bank: u8, tile_index: usize, tile_x: u8, tile_y: u8) -> u8 {
        // Optimized bitwise calculations for cache indexing
        let idx = ((bank as usize) * TILES_PER_BANK << 6)
            + (tile_index << 6)
            + ((tile_y as usize) << 3)
            + (tile_x as usize);
        self.tile_cache[idx]
    }

    fn decode_tile(&mut self, bank: u8, tile_index: usize) {
        let bank_data: &[u8; BANK_SIZE] = match bank {
            0 => &self.bank0,
            _ => &self.bank1,
        };
        let tile_base = tile_index * 16;
        let cache_base = ((bank as usize) * TILES_PER_BANK << 6) + (tile_index << 6);

        for row in 0..8u8 {
            let low = bank_data[tile_base + ((row as usize) << 1)];
            let high = bank_data[tile_base + ((row as usize) << 1) + 1];
            for col in 0..8u8 {
                let bit = 7 - col;
                let color_index = ((high >> bit) & 1) << 1 | ((low >> bit) & 1);
                self.tile_cache[cache_base + ((row as usize) << 3) + col as usize] = color_index;
            }
        }
    }

    fn rebuild_cache(&mut self) {
        for bank in 0..2u8 {
            for tile in 0..TILES_PER_BANK {
                self.decode_tile(bank, tile);
            }
        }
    }

    pub fn save_to_bytes(&self, buf: &mut Vec<u8>) {
        buf.extend_from_slice(&self.bank0[..]);
        buf.extend_from_slice(&self.bank1[..]);
        buf.push(self.active_bank);
        buf.push(self.cgb as u8);
    }

    pub fn load_from_bytes(data: &[u8], pos: &mut usize) -> crate::gbc::error::Result<Self> {
        let mut bank0 = Box::new([0u8; BANK_SIZE]);
        bank0.copy_from_slice(&data[*pos..*pos + BANK_SIZE]); *pos += BANK_SIZE;
        let mut bank1 = Box::new([0u8; BANK_SIZE]);
        bank1.copy_from_slice(&data[*pos..*pos + BANK_SIZE]); *pos += BANK_SIZE;
        let active_bank = data[*pos]; *pos += 1;
        let cgb = data[*pos] != 0; *pos += 1;

        let cache_size = TILES_PER_BANK * TILE_CACHE_STRIDE * 2;
        let mut vram = Self {
            bank0,
            bank1,
            tile_cache: vec![0u8; cache_size].into_boxed_slice(),
            active_bank,
            cgb,
        };
        vram.rebuild_cache();
        Ok(vram)
    }
}

impl MemoryRead<u16, u8> for Vram {
    #[inline(always)]
    fn read(&self, addr: u16) -> u8 {
        let idx = (addr - Self::BASE_ADDR) as usize;
        match self.active_bank {
            0 => self.bank0[idx],
            _ => self.bank1[idx],
        }
    }
}

impl MemoryWrite<u16, u8> for Vram {
    #[inline(always)]
    fn write(&mut self, addr: u16, value: u8) {
        let idx = (addr - Self::BASE_ADDR) as usize;
        let bank = self.active_bank;
        match bank {
            0 => self.bank0[idx] = value,
            _ => self.bank1[idx] = value,
        }
        if idx < TILE_DATA_SIZE {
            self.decode_tile(bank, idx / 16);
        }
    }
}

#[derive(Clone, Copy)]
struct LcdControl {
    /// Raw register value
    pub raw: u8,
}

impl LcdControl {
    pub fn new(boot_rom: bool) -> Self {
        Self { raw: if boot_rom { 0 } else { 0x91 } }
    }

    pub fn lcd_display_enable(&self) -> bool {
        self.raw & (1 << 7) != 0
    }

    pub fn window_tile_map(&self) -> u16 {
        if self.raw & (1 << 6) == 0 {
            0x9800
        } else {
            0x9C00
        }
    }

    pub fn window_display_enable(&self) -> bool {
        self.raw & (1 << 5) != 0
    }

    pub fn bg_tile_data_select(&self) -> bool {
        self.raw & (1 << 4) == 0
    }

    pub fn bg_tile_map(&self) -> u16 {
        if self.raw & (1 << 3) == 0 {
            0x9800
        } else {
            0x9C00
        }
    }

    pub fn sprite_size(&self) -> bool {
        self.raw & (1 << 2) != 0
    }

    pub fn sprite_enable(&self) -> bool {
        self.raw & (1 << 1) != 0
    }

    pub fn bg_priority(&self) -> bool {
        self.raw & (1 << 0) != 0
    }
}

#[derive(Clone, Copy, PartialEq)]
#[repr(u8)]
pub enum StatMode {
    Hblank = 0,
    Vblank,
    OamScan,
    OamRead,
}

/// LCD STAT register
#[derive(Clone, Copy, PartialEq)]
struct LcdStat {
    /// Raw register value
    pub raw: u8,
}

impl LcdStat {
    pub fn new() -> Self {
        Self { raw: 0 }
    }

    pub fn ly_enabled(&self) -> bool {
        self.raw & (1 << 6) != 0
    }

    pub fn oam_enabled(&self) -> bool {
        self.raw & (1 << 5) != 0
    }

    pub fn vblank_enabled(&self) -> bool {
        self.raw & (1 << 4) != 0
    }

    pub fn hblank_enabled(&self) -> bool {
        self.raw & (1 << 3) != 0
    }

    #[allow(dead_code)]
    pub fn coincidence(&self) -> bool {
        self.raw & (1 << 2) != 0
    }

    pub fn mode(&self) -> StatMode {
        match self.raw & 0x3 {
            0 => StatMode::Hblank,
            1 => StatMode::Vblank,
            2 => StatMode::OamScan,
            3 => StatMode::OamRead,
            _ => unreachable!(),
        }
    }
}

/// Contains raw data for a single sprite in OAM
///
/// Note: y and x coordinates need to be converted
struct Sprite {
    pub y: u8,
    pub x: u8,
    pub tile_number: u8,
    pub attr: u8,
}


pub struct Ppu {
    /// Video RAM (0x8000 - 0x9FFF)
    pub vram: Vram,

    /// OAM (0xFE00-0xFE9F, 160 bytes)
    pub oam: Box<[u8]>,

    /// LCD control register (0xFF40)
    lcdc: LcdControl,

    /// LCD status register (0xFF41)
    stat: LcdStat,

    /// Background position registers (0xFF42, 0xFF43)
    scy: u8,
    scx: u8,

    /// LCD line registers (0xFF44, 0xFF45)
    ly: u8,
    lyc: u8,

    /// OAM DMA (0xFF46)
    oam_dma: u8,
    pub oam_dma_active: bool,

    /// Monochrome palette registers (0xFF47-0xFF49)
    bgp: u8,
    obp0: u8,
    obp1: u8,

    /// Window position registers (0xFF4A, 0xFF4B)
    wy: u8,
    wx: u8,

    /// Internal window line counter
    window_line_counter: u8,

    /// Color palette index registers (0xFF68-0xFF6B)
    bcps: u8,
    ocps: u8,

    /// Sprite/object priority mode (0xFF6C)
    opri: u8,

    /// BG color palette RAM
    bg_palette_ram: Box<[u8]>,

    /// Sprite color palette RAM
    sprite_palette_ram: Box<[u8]>,

    /// Buffer for the current frame
    frame_buffer: FrameBuffer,

    /// Sprites that are visible on this scanline
    sprites: Vec<Sprite>,

    /// Current dot being rendered in this scanline
    dot: u16,

    /// Previous STAT interrupt state
    prev_stat_interrupt: bool,

    /// If `true`, operate in CGB mode
    cgb: bool,
}

impl Ppu {
    pub const OAM_START_ADDR: u16 = 0xFE00;
    pub const OAM_LAST_ADDR: u16 = 0xFE9F;

    // Register addresses
    const LCDC_ADDR: u16 = 0xFF40;
    const STAT_ADDR: u16 = 0xFF41;
    const SCY_ADDR: u16 = 0xFF42;
    const SCX_ADDR: u16 = 0xFF43;
    const LY_ADDR: u16 = 0xFF44;
    const LYC_ADDR: u16 = 0xFF45;
    const WY_ADDR: u16 = 0xFF4A;
    const WX_ADDR: u16 = 0xFF4B;

    const DOTS_PER_LINE: u16 = 456;
    const VBLANK_START_LINE: u8 = 144;
    const TOTAL_LINES: u8 = 154;
    const OAM_SCAN_DOTS: u16 = 80;
    const OAM_READ_DOTS: u16 = 172;
    const HBLANK_DOTS: u16 = 204;
    const VBLANK_DOTS: u16 =
        Self::DOTS_PER_LINE * (Self::TOTAL_LINES - Self::VBLANK_START_LINE) as u16;

    pub fn new(cgb: bool, boot_rom: bool) -> Self {
        Self {
            vram: Vram::new(cgb),
            oam: Box::new([0u8; 160]),
            lcdc: LcdControl::new(boot_rom),
            stat: LcdStat::new(),
            scy: 0,
            scx: 0,
            ly: 0,
            lyc: 0,
            oam_dma: 0,
            oam_dma_active: false,
            bgp: 0xFC,
            obp0: 0xFF,
            obp1: 0xFF,
            wy: 0,
            wx: 0,
            window_line_counter: 0,
            bcps: 0,
            ocps: 0,
            opri: 0,
            bg_palette_ram: Box::new([0xFF; 64]),
            sprite_palette_ram: Box::new([0xFF; 64]),
            frame_buffer: FrameBuffer::new(),
            sprites: Vec::with_capacity(10),
            dot: 0,
            prev_stat_interrupt: false,
            cgb,
        }
    }

    /// Returns the next: (dot, scanline, STAT mode)
    fn get_next_dot(&self, cycles: u16, speed: bool) -> (u16, u8, StatMode) {
        let mut line = self.ly;
        let mut dot = self.dot;

        // Figure out the number of pixels to render in this step
        let dots = if speed {
            cycles / 2
        } else {
            cycles
        };

        dot += dots;

        if dot >= Self::DOTS_PER_LINE {
            line += 1;
            dot -= Self::DOTS_PER_LINE;
        }

        if line == Self::TOTAL_LINES {
            line = 0;
        }

        let mode = if line < Self::VBLANK_START_LINE {
            if dot < Self::OAM_SCAN_DOTS {
                StatMode::OamScan
            } else if dot >= Self::OAM_SCAN_DOTS && dot < Self::OAM_SCAN_DOTS + Self::OAM_READ_DOTS
            {
                StatMode::OamRead
            } else {
                StatMode::Hblank
            }
        } else {
            StatMode::Vblank
        };

        (dot, line, mode)
    }

    pub fn step(&mut self, cycles: u16, speed: bool, interrupts: &mut Vec<Interrupt>) {
        if !self.lcdc.lcd_display_enable() {
            return;
        }

        let (dot, line, mode) = self.get_next_dot(cycles, speed);

        self.dot = dot;
        self.ly = line;

        let stat_mode_change = self.update_status(mode, interrupts);

        if stat_mode_change {
            self.render();
        }
    }

    fn update_status(&mut self, mode: StatMode, interrupts: &mut Vec<Interrupt>) -> bool {
        let ly_coincidence = self.ly == self.lyc;

        let mut stat = mode as u8;
        if ly_coincidence {
            stat |= 1 << 2;
        }

        let prev_mode = self.stat.mode();
        let stat_mode_change = prev_mode != mode;

        if stat_mode_change && mode == StatMode::Vblank {
            interrupts.push(Interrupt::Vblank);
        }

        let stat_interrupt = (ly_coincidence && self.stat.ly_enabled()) || {
            match mode {
                StatMode::Hblank => self.stat.hblank_enabled(),
                StatMode::Vblank => self.stat.vblank_enabled(),
                StatMode::OamScan | StatMode::OamRead => self.stat.oam_enabled(),
            }
        };

        if !self.prev_stat_interrupt && stat_interrupt {
            interrupts.push(Interrupt::LcdStat);
        }

        self.stat.raw = self.stat.raw & 0xF8 | stat;
        self.prev_stat_interrupt = stat_interrupt;

        stat_mode_change
    }

    pub fn next_mode(&self, cycles: u16, speed: bool) -> (StatMode, u16) {
        let (.., mode) = self.get_next_dot(cycles, speed);

        let dots = match mode {
            StatMode::OamScan => Self::OAM_SCAN_DOTS,
            StatMode::OamRead => Self::OAM_READ_DOTS,
            StatMode::Hblank => Self::HBLANK_DOTS,
            StatMode::Vblank => Self::VBLANK_DOTS,
        };

        let cycles_in_mode = if speed { dots * 2 } else { dots };
        (mode, cycles_in_mode as u16)
    }

    fn render(&mut self) {
        match self.stat.mode() {
            StatMode::OamRead => {
                self.sprites.clear();
                self.find_visible_sprites();
            }
            StatMode::Hblank => {
                self.render_scanline();

                let window_drawn = self.lcdc.window_display_enable()
                    && self.ly >= self.wy
                    && 159 >= self.wx.wrapping_sub(7);

                if window_drawn {
                    self.window_line_counter += 1;
                }
            }
            StatMode::Vblank => {
                self.frame_buffer.ready = true;
                self.window_line_counter = 0;
            }
            _ => (),
        }
    }

    fn find_visible_sprites(&mut self) {
        let scanline = self.ly;
        let size = if self.lcdc.sprite_size() { 16 } else { 8 };

        for chunk in self.oam.chunks_exact(4) {
            let y = chunk[0];
            let x = chunk[1];
            let tile_number = chunk[2];
            let attr = chunk[3];

            let sprite_start = y.wrapping_sub(16);
            let sprite_end = sprite_start.wrapping_add(size);

            let visible = if sprite_start < sprite_end {
                sprite_start <= scanline && scanline < sprite_end
            } else {
                scanline < sprite_end
            };

            if visible && self.sprites.len() < 10 {
                self.sprites.push(Sprite { y, x, tile_number, attr });
            }
        }

        if !self.cgb || (self.opri & 1 != 0) {
            self.sprites.sort_by(|a, b| a.x.cmp(&b.x));
        }
    }

    /// Optimized: Hoisted state out of loop. Eliminated `render_pixel` call overhead.
    fn render_scanline(&mut self) {
        let scanline = self.ly;

        // Hoist all register evaluation
        let bg_tile_map_base = self.lcdc.bg_tile_map();
        let window_tile_map_base = self.lcdc.window_tile_map();
        let bg_priority_lcdc = self.lcdc.bg_priority();
        let sprite_enable = self.lcdc.sprite_enable();
        let window_enable = self.lcdc.window_display_enable();
        let wx_adj = self.wx.wrapping_sub(7);

        for pixel in 0..LCD_WIDTH as u8 {
            let mut pixel_data;
            let mut bg_priority = false;
            let mut bg_color_index = 0;

            if bg_priority_lcdc || self.cgb {
                let in_window = window_enable && scanline >= self.wy && pixel >= wx_adj;

                let (data, priority, color_index) = if !in_window {
                    let bg_pixel_x = pixel.wrapping_add(self.scx);
                    let bg_pixel_y = scanline.wrapping_add(self.scy);
                    self.fetch_bg_pixel_data(bg_pixel_x, bg_pixel_y, bg_tile_map_base)
                } else {
                    let pixel_x = pixel - wx_adj;
                    let pixel_y = self.window_line_counter;
                    self.fetch_bg_pixel_data(pixel_x, pixel_y, window_tile_map_base)
                };

                pixel_data = Some(data);
                bg_priority = bg_priority_lcdc && priority;
                bg_color_index = color_index;
            } else {
                pixel_data = Some(DMG_PALETTE[0]);
            }

            if sprite_enable {
                if let Some((data, priority)) = self.fetch_sprite_pixel_data(pixel) {
                    if bg_color_index == 0 || (!bg_priority && priority) || pixel_data.is_none() {
                        pixel_data = Some(data);
                    }
                }
            }

            if let Some(data) = pixel_data {
                self.frame_buffer.write(pixel as usize, scanline as usize, data);
            }
        }
    }

    #[inline(always)]
    fn fetch_bg_pixel_data(
        &self,
        bg_pixel_x: u8,
        bg_pixel_y: u8,
        tile_map_base: u16,
    ) -> (GameboyRgb, bool, u8) {
        let tile_data_index_signed = self.lcdc.bg_tile_data_select();

        // Optimized Bitwise shifts instead of division/modulo
        let bg_tile_x = bg_pixel_x >> 3;
        let bg_tile_y = bg_pixel_y >> 3;
        let tile_map_index = ((bg_tile_y as u16) << 5) | bg_tile_x as u16;

        let tile_number = self.vram.read_bank(0, tile_map_base + tile_map_index);
        let tile_data_attr = if self.cgb {
            self.vram.read_bank(1, tile_map_base + tile_map_index)
        } else {
            0
        };

        let tile_palette_num = tile_data_attr & 0x07;
        let tile_data_bank = (tile_data_attr & (1 << 3)) >> 3;
        let horizontal_flip = (tile_data_attr & (1 << 5)) != 0;
        let vertical_flip = (tile_data_attr & (1 << 6)) != 0;
        let bg_priority = if self.cgb {
            (tile_data_attr & (1 << 7)) != 0
        } else {
            false
        };

        let tile_index = if tile_data_index_signed && tile_number > 127 {
            tile_number as usize
        } else if tile_data_index_signed {
            tile_number as usize + 256
        } else {
            tile_number as usize
        };

        let mut tile_pixel_x = bg_pixel_x & 7;
        let mut tile_pixel_y = bg_pixel_y & 7;

        if horizontal_flip {
            tile_pixel_x = 7 - tile_pixel_x;
        }
        if vertical_flip {
            tile_pixel_y = 7 - tile_pixel_y;
        }

        let color_index = self.vram.read_tile_pixel(tile_data_bank as u8, tile_index, tile_pixel_x, tile_pixel_y);
        let pixel_data = self.fetch_pixel_from_color_index(color_index, tile_palette_num, false);

        (pixel_data, bg_priority, color_index)
    }

    #[inline(always)]
    fn fetch_sprite_pixel_data(&self, pixel: u8) -> Option<(GameboyRgb, bool)> {
        let size = if self.lcdc.sprite_size() { 16 } else { 8 };
        let scanline = self.ly;

        for sprite in &self.sprites {
            let sprite_start = sprite.x.wrapping_sub(8);
            let sprite_end = sprite.x;
            let visible = if sprite_start < sprite_end {
                sprite_start <= pixel && pixel < sprite_end
            } else {
                pixel < sprite_end
            };

            if !visible {
                continue;
            }

            let tile_y = sprite.y.wrapping_sub(16);
            let tile_x = sprite.x.wrapping_sub(8);

            let tile_number = sprite.tile_number;
            let attr = sprite.attr;
            let palette_num;
            let vram_bank;

            if self.cgb {
                palette_num = attr & 0x07;
                vram_bank = (attr & 1 << 3) >> 3;
            } else {
                palette_num = (attr & 1 << 4) >> 4;
                vram_bank = 0;
            };

            let horizontal_flip = (attr & 1 << 5) != 0;
            let vertical_flip = (attr & 1 << 6) != 0;
            let priority = (attr & 1 << 7) == 0;

            let mut tile_pixel_x = pixel.wrapping_sub(tile_x);
            let mut tile_pixel_y = scanline.wrapping_sub(tile_y);

            if vertical_flip {
                tile_pixel_y = (size - 1) - tile_pixel_y;
            }

            if horizontal_flip {
                tile_pixel_x = 7 - tile_pixel_x;
            }

            let lower_tile = tile_pixel_y >= 8;
            if lower_tile {
                tile_pixel_y -= 8;
            }

            let tile_index = if size == 8 {
                tile_number as usize
            } else if !lower_tile {
                tile_number as usize & 0xFE
            } else {
                tile_number as usize | 0x01
            };

            let color_index = self.vram.read_tile_pixel(vram_bank, tile_index, tile_pixel_x, tile_pixel_y);

            if color_index != 0 {
                let pixel_data = self.fetch_pixel_from_color_index(color_index, palette_num, true);
                return Some((pixel_data, priority));
            }
        }

        None
    }

    #[inline(always)]
    fn fetch_pixel_from_color_index(&self, color_index: u8, tile_palette_num: u8, sprite: bool) -> GameboyRgb {
        if self.cgb {
            // Optimized bitwise calculations for index
            let palette_index = ((tile_palette_num << 3) + (color_index << 1)) as usize;
            let palette_ram = if sprite {
                &self.sprite_palette_ram
            } else {
                &self.bg_palette_ram
            };

            let pixel_color =
                (palette_ram[palette_index + 1] as u16) << 8 | palette_ram[palette_index] as u16;

            let red = (pixel_color & 0x001F) as u8;
            let green = ((pixel_color & 0x03E0) >> 5) as u8;
            let blue = ((pixel_color & 0x7C00) >> 10) as u8;

            let mut pixel_data = GameboyRgb { red, blue, green };
            pixel_data.scale_to_rgb();
            pixel_data
        } else {
            let palette_reg = if !sprite {
                self.bgp
            } else {
                match tile_palette_num {
                    0 => self.obp0,
                    _ => self.obp1,
                }
            };

            let palette_index = match color_index {
                0 => palette_reg & 0b00000011,
                1 => (palette_reg & 0b00001100) >> 2,
                2 => (palette_reg & 0b00110000) >> 4,
                3 => (palette_reg & 0b11000000) >> 6,
                _ => unreachable!(),
            };

            DMG_PALETTE[palette_index as usize]
        }
    }

    fn palette_write(&mut self, value: u8, sprite: bool) {
        let auto_increment;
        let index;
        let palette_ram;

        if !sprite {
            auto_increment = self.bcps & (1 << 7) != 0;
            index = (self.bcps & 0x3F) as usize;
            palette_ram = &mut self.bg_palette_ram;
        } else {
            auto_increment = self.ocps & (1 << 7) != 0;
            index = (self.ocps & 0x3F) as usize;
            palette_ram = &mut self.sprite_palette_ram;
        }

        palette_ram[index] = value;

        if auto_increment {
            let reg = if !sprite {
                &mut self.bcps
            } else {
                &mut self.ocps
            };

            let mut index = (*reg & 0x3F) + 1;

            if index > 0x3F {
                index = 0x00;
            }

            *reg = (*reg & !0x3F) | index;
        }
    }

    fn palette_read(&self, sprite: bool) -> u8 {
        let index = if !sprite {
            (self.bcps & 0x3F) as usize
        } else {
            (self.ocps & 0x3F) as usize
        };
        if !sprite {
            self.bg_palette_ram[index]
        } else {
            self.sprite_palette_ram[index]
        }
    }

    fn vram_locked(&self) -> bool {
        self.lcdc.lcd_display_enable()
            && match self.stat.mode() {
                StatMode::OamRead => true,
                _ => false,
            }
    }

    pub fn oam_locked(&self) -> bool {
        self.lcdc.lcd_display_enable()
            && match self.stat.mode() {
                StatMode::OamScan | StatMode::OamRead => true,
                StatMode::Vblank | StatMode::Hblank => false,
            }
    }

    #[allow(dead_code)]
    pub fn vram(&self) -> &Vram {
        &self.vram
    }

    pub fn vram_mut(&mut self) -> &mut Vram {
        &mut self.vram
    }

    pub fn frame_buffer(&mut self) -> Option<&FrameBuffer> {
        if self.frame_buffer.ready {
            self.frame_buffer.ready = false;
            Some(&self.frame_buffer)
        } else {
            None
        }
    }

    pub fn is_frame_ready(&self) -> bool {
        self.frame_buffer.ready
    }

    /// Returns the raw LCDC register value (0xFF40).
    pub fn lcdc(&self) -> u8 {
        self.lcdc.raw
    }

    /// Returns the current LY (scanline) value.
    pub fn ly(&self) -> u8 {
        self.ly
    }

    /// Returns a numeric representation of the current STAT mode:
    /// 0 = Hblank, 1 = Vblank, 2 = OamScan, 3 = OamRead
    pub fn stat_mode(&self) -> u8 {
        self.stat.mode() as u8
    }

    pub fn save_to_bytes(&self, buf: &mut Vec<u8>) {
        self.vram.save_to_bytes(buf);
        buf.extend_from_slice(&self.oam);
        buf.push(self.lcdc.raw);
        buf.push(self.stat.raw);
        buf.push(self.scy);
        buf.push(self.scx);
        buf.push(self.ly);
        buf.push(self.lyc);
        buf.push(self.oam_dma);
        buf.push(self.oam_dma_active as u8);
        buf.push(self.bgp);
        buf.push(self.obp0);
        buf.push(self.obp1);
        buf.push(self.wy);
        buf.push(self.wx);
        buf.push(self.window_line_counter);
        buf.push(self.bcps);
        buf.push(self.ocps);
        buf.push(self.opri);
        buf.extend_from_slice(&self.bg_palette_ram);
        buf.extend_from_slice(&self.sprite_palette_ram);
        self.frame_buffer.save_to_bytes(buf);
        buf.extend_from_slice(&self.dot.to_le_bytes());
        buf.push(self.prev_stat_interrupt as u8);
        buf.push(self.cgb as u8);
    }

    pub fn load_from_bytes(data: &[u8], pos: &mut usize) -> crate::gbc::error::Result<Self> {
        let vram = Vram::load_from_bytes(data, pos)?;
        let mut oam = vec![0u8; 160];
        oam.copy_from_slice(&data[*pos..*pos + 160]); *pos += 160;
        let lcdc_raw = data[*pos]; *pos += 1;
        let stat_raw = data[*pos]; *pos += 1;
        let scy = data[*pos]; *pos += 1;
        let scx = data[*pos]; *pos += 1;
        let ly = data[*pos]; *pos += 1;
        let lyc = data[*pos]; *pos += 1;
        let oam_dma = data[*pos]; *pos += 1;
        let oam_dma_active = data[*pos] != 0; *pos += 1;
        let bgp = data[*pos]; *pos += 1;
        let obp0 = data[*pos]; *pos += 1;
        let obp1 = data[*pos]; *pos += 1;
        let wy = data[*pos]; *pos += 1;
        let wx = data[*pos]; *pos += 1;
        let window_line_counter = data[*pos]; *pos += 1;
        let bcps = data[*pos]; *pos += 1;
        let ocps = data[*pos]; *pos += 1;
        let opri = data[*pos]; *pos += 1;
        let mut bg_palette_ram = vec![0u8; 64];
        bg_palette_ram.copy_from_slice(&data[*pos..*pos + 64]); *pos += 64;
        let mut sprite_palette_ram = vec![0u8; 64];
        sprite_palette_ram.copy_from_slice(&data[*pos..*pos + 64]); *pos += 64;
        let frame_buffer = FrameBuffer::load_from_bytes(data, pos)?;
        let dot = u16::from_le_bytes([data[*pos], data[*pos + 1]]); *pos += 2;
        let prev_stat_interrupt = data[*pos] != 0; *pos += 1;
        let cgb = data[*pos] != 0; *pos += 1;

        let lcdc = { let mut c = LcdControl::new(false); c.raw = lcdc_raw; c };
        let stat = { let mut s = LcdStat::new(); s.raw = stat_raw; s };

        Ok(Self {
            vram,
            oam: oam.into_boxed_slice(),
            lcdc,
            stat,
            scy, scx, ly, lyc,
            oam_dma, oam_dma_active,
            bgp, obp0, obp1, wy, wx,
            window_line_counter,
            bcps, ocps, opri,
            bg_palette_ram: bg_palette_ram.into_boxed_slice(),
            sprite_palette_ram: sprite_palette_ram.into_boxed_slice(),
            frame_buffer,
            sprites: Vec::with_capacity(10),
            dot, prev_stat_interrupt, cgb,
        })
    }
}

impl MemoryRead<u16, u8> for Ppu {
    #[inline(always)]
    fn read(&self, addr: u16) -> u8 {
        match addr {
            Vram::BASE_ADDR..=Vram::LAST_ADDR => {
                if !self.vram_locked() {
                    self.vram.read(addr)
                } else {
                    eprintln!("Blocked VRAM read from 0x{:X}", addr);
                    0xFF
                }
            }
            Vram::BANK_SELECT_ADDR => {
                let bank = self.vram.active_bank;
                bank | 0xFE
            }
            Self::OAM_START_ADDR..=Self::OAM_LAST_ADDR => {
                let idx = (addr - Self::OAM_START_ADDR) as usize;
                self.oam[idx]
            }
            Self::LCDC_ADDR => self.lcdc.raw,
            Self::STAT_ADDR => self.stat.raw,
            Self::SCY_ADDR => self.scy,
            Self::SCX_ADDR => self.scx,
            Self::LY_ADDR => self.ly,
            Self::LYC_ADDR => self.lyc,
            0xFF46 => self.oam_dma,
            0xFF47 => self.bgp,
            0xFF48 => self.obp0,
            0xFF49 => self.obp1,
            Self::WY_ADDR => self.wy,
            Self::WX_ADDR => self.wx,
            0xFF68 => self.bcps,
            0xFF69 => self.palette_read(false),
            0xFF6A => self.ocps,
            0xFF6B => self.palette_read(true),
            0xFF6C => self.opri,
            _ => panic!("Unexpected read from addr {}", addr),
        }
    }
}

impl MemoryWrite<u16, u8> for Ppu {
    #[inline(always)]
    fn write(&mut self, addr: u16, value: u8) {
        match addr {
            Vram::BASE_ADDR..=Vram::LAST_ADDR => {
                self.vram.write(addr, value);
            }
            Vram::BANK_SELECT_ADDR => self.vram.update_bank(value),
            Self::OAM_START_ADDR..=Self::OAM_LAST_ADDR => {
                if !self.oam_locked() {
                    let idx = (addr - Self::OAM_START_ADDR) as usize;
                    self.oam[idx] = value;
                } else {
                    eprintln!("Blocked OAM write to 0x{:X}: 0x{:X}", addr, value);
                }
            }
            Self::LCDC_ADDR => {
                self.lcdc.raw = value;
                if value & 1 << 7 == 0 {
                    self.ly = 0;
                    self.dot = 0;
                    self.stat.raw = self.stat.raw & 0xF8;
                }
            }
            Self::STAT_ADDR => {
                let value = value & 0xF8;
                self.stat.raw = value | self.stat.raw & 0x07;
            }
            Self::SCY_ADDR => self.scy = value,
            Self::SCX_ADDR => self.scx = value,
            Self::LYC_ADDR => self.lyc = value,
            Self::WY_ADDR => self.wy = value,
            Self::WX_ADDR => self.wx = value,
            Self::LY_ADDR => {
                if self.ly & (1 << 7) != 0 {
                    if value & (1 << 7) == 0 {
                        self.ly = 0;
                    }
                } else {
                    self.ly = value;
                }
            }
            0xFF46 => {
                self.oam_dma = value;
                self.oam_dma_active = true;
            }
            0xFF47 => self.bgp = value,
            0xFF48 => self.obp0 = value,
            0xFF49 => self.obp1 = value,
            0xFF68 => self.bcps = value,
            0xFF69 => self.palette_write(value, false),
            0xFF6A => self.ocps = value,
            0xFF6B => self.palette_write(value, true),
            0xFF6C => {
                self.opri = value;
            }
            _ => panic!("Unexpected write to addr {} value {}", addr, value),
        }
    }
}