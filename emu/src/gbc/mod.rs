pub mod apu;
pub mod cartridge;
mod cpu;
mod dma;
pub mod error;
mod instructions;
pub mod joypad;
mod memory;
pub mod ppu;
mod registers;
mod rtc;
mod timer;

#[cfg(feature = "debug")]
pub mod debug;

#[cfg(test)]
mod tests;

use std::time::{Duration, Instant};

use cartridge::{Cartridge, Controller};
pub use cpu::Cpu;
use cpu::Interrupt;
pub use error::{Error, Result};
use joypad::JoypadEvent;
use memory::{MemoryRead, MemoryWrite};
use ppu::FrameBuffer;
use registers::{Reg16, RegisterOps};

pub struct GameboyState<'a> {
    pub ram: Option<&'a [u8]>,
    pub rtc: Option<Vec<u8>>,
}

/// Gameboy
pub struct Gameboy {
    cpu: Cpu,
    verbose: bool,

    #[cfg(feature = "debug")]
    debugger: debug::Debugger,
}

#[allow(dead_code)]
impl Gameboy {
    const FRAME_FREQUENCY: f64 = 59.7; // Hz

    /// Frame duration, in ns
    pub const FRAME_DURATION: u64 = ((1f64 / Self::FRAME_FREQUENCY) * 1e9) as u64;

    /// Initialize the emulator from a `Cartridge`.
    pub fn init(cartridge: Cartridge, trace: bool) -> Result<Self> {
        let cpu = Cpu::from_cartridge(cartridge, trace)?;

        #[cfg(feature = "debug")]
        let gameboy = Self {
            cpu,
            verbose: false,
            debugger: debug::Debugger::new(),
        };

        #[cfg(not(feature = "debug"))]
        let gameboy = Self {
            cpu,
            verbose: false,
        };

        Ok(gameboy)
    }

    /// Enable or disable verbose debug logging.
    pub fn set_verbose(&mut self, verbose: bool) {
        self.verbose = verbose;
    }

    /// Run the Gameboy for a single step.
    ///
    /// Returns the number of cycles consumed by the CPU.
    pub fn step(&mut self) -> u32 {
        let speed = self.cpu.speed;

        #[cfg(feature = "debug")]
        // If the debugger is triggered, step into the REPL.
        if self.debugger.triggered(&self.cpu) {
            self.debugger.repl(&mut self.cpu);
        }

        // Execute a step of the CPU
        //
        // This handles interrupt processing and DMA internally.
        let (cycles_taken, _inst) = self.cpu.step();

        let mut interrupts = Vec::new();

        // Update the memory bus
        //
        // Internally, this executes a step for each of:
        //
        // 1. PPU
        // 2. Timer
        // 3. Serial
        // 4. RTC (if present)
        self.cpu.memory.step(cycles_taken, speed, &mut interrupts);

        // Trigger any pending interrupts
        for interrupt in interrupts {
            self.cpu.trigger_interrupt(interrupt);
        }

        if self.cpu.stopped {
            // Reset DIV on speed switch
            self.cpu.memory.write(0xFF04u16, 0u8);
            self.cpu.stopped = false;
        }

        cycles_taken as u32
    }

    /// Run the Gameboy until a frame is ready (i.e., start of VBLANK).
    ///
    /// Returns a pointer to the frame buffer.
    pub fn frame(&mut self, joypad_events: Option<&[JoypadEvent]>) -> &FrameBuffer {
        let frame_start = Instant::now();
        let mut step_count: u64 = 0;
        let mut last_debug_pc = self.cpu.registers.PC;
        let mut stuck_pc_count: u64 = 0;

        while !self.cpu.memory.ppu().is_frame_ready() {
            self.step();
            step_count += 1;

            if self.verbose && step_count % 100_000 == 0 {
                let elapsed = frame_start.elapsed();
                let total_steps = step_count;

                // Check if we've been spinning for > 500ms without a frame (likely freeze)
                if elapsed > Duration::from_millis(500) {
                    let pc = self.cpu.registers.PC;
                    let sp = self.cpu.registers.read(Reg16::SP);
                    let lcdc = self.cpu.memory.ppu().lcdc();
                    let ly = self.cpu.memory.ppu().ly();
                    let stat_mode = self.cpu.memory.ppu().stat_mode();
                    let ie = self.cpu.memory.read(0xFFFFu16);
                    let intf = self.cpu.memory.read(0xFF0Fu16);
                    let halted = self.cpu.halted;
                    let stopped = self.cpu.stopped;

                    eprintln!(
                        "[DEBUG] Frame not ready after {:.1}s ({} steps) PC={:#06x} SP={:#04x} \
                         LCDC={:#04x}(enable={}) LY={} STAT={} IE={:#04x} IF={:#04x} \
                         halted={} stopped={}",
                        elapsed.as_secs_f64(),
                        total_steps,
                        pc,
                        sp,
                        lcdc,
                        (lcdc >> 7) & 1,
                        ly,
                        stat_mode,
                        ie,
                        intf,
                        halted,
                        stopped,
                    );

                    // Detect if PC hasn't moved (stuck in a loop or waiting for interrupt)
                    if pc == last_debug_pc {
                        stuck_pc_count += 100_000;
                        if stuck_pc_count >= 500_000 {
                            eprintln!(
                                "[DEBUG]  ** PC stuck at {:#06x} for {} steps - possible infinite loop or HALT",
                                pc, total_steps
                            );
                            stuck_pc_count = 0;
                        }
                    } else {
                        stuck_pc_count = 0;
                    }
                    last_debug_pc = pc;

                    // Detect LCD disabled — PPU will never produce VBLANK
                    if lcdc >> 7 & 1 == 0 {
                        eprintln!(
                            "[DEBUG]  ** LCD DISABLED - PPU will never produce a frame! \
                             The game may be waiting for VBLANK interrupt with LCD off."
                        );
                    }

                    // Detect HALT with interrupts not enabled — the CPU will sleep forever
                    if halted {
                        if ie & intf == 0 || !self.cpu.ime {
                            eprintln!(
                                "[DEBUG]  ** HALTED with no pending/enabled interrupts - CPU will sleep forever"
                            );
                        } else {
                            eprintln!(
                                "[DEBUG]  ** CPU is HALTED, waiting for interrupt (IE={:#04x} IF={:#04x} IME={})",
                                ie, intf, self.cpu.ime
                            );
                        }
                    }
                }
            }
        }

        if self.verbose {
            let elapsed = frame_start.elapsed();
            if elapsed > Duration::from_millis(100) {
                eprintln!(
                    "[DEBUG] Frame completed after {:.1}ms ({} steps)",
                    elapsed.as_secs_f64() * 1000.0,
                    step_count
                );
            }
        }

        self.update_joypad(joypad_events);

        // This is a clear-on-read operation. That is, the frame will be marked as
        // "not ready" within this method.
        self.cpu.memory.ppu_mut().frame_buffer().unwrap()
    }

    pub fn update_joypad(&mut self, joypad_events: Option<&[JoypadEvent]>) {
        if let Some(events) = joypad_events {
            for event in events {
                if self.cpu.memory.joypad().handle_event(event) {
                    self.cpu.trigger_interrupt(Interrupt::Joypad);
                }
            }
        }
    }

    /// Insert a new cartridge and reset the emulator
    pub fn insert(&mut self, cartridge: Cartridge) -> Result<()> {
        self.cpu = Cpu::from_cartridge(cartridge, false)?;
        Ok(())
    }

    /// Load a Gameboy from a save state and a `Cartridge`.
    pub fn load(save_data: &[u8], cartridge: Cartridge) -> Result<Self> {
        if save_data.len() < 8 || &save_data[0..4] != b"GBCS" {
            return Err(Error::InvalidValue("Invalid save data header".into()));
        }
        let mut pos = 8;
        let cpu = Cpu::load_from_bytes(save_data, &mut pos)?;
        #[cfg(feature = "debug")]
        let mut gameboy = Gameboy { cpu, verbose: false, debugger: debug::Debugger::new() };
        #[cfg(not(feature = "debug"))]
        let mut gameboy = Gameboy { cpu, verbose: false };
        gameboy.cpu.memory.controller_mut().load_rom(cartridge.data);
        Ok(gameboy)
    }

    /// Save the current state of this Gameboy to a byte `Vec`.
    pub fn save(&self) -> Result<Vec<u8>> {
        let mut data = Vec::new();
        data.extend_from_slice(b"GBCS");
        data.extend_from_slice(&1u32.to_le_bytes());
        self.cpu.save_to_bytes(&mut data);
        Ok(data)
    }

    /// Reset the emulator
    pub fn reset(&mut self) {
        // Reset the CPU
        self.cpu.reset();
    }

    pub fn cpu(&mut self) -> &mut Cpu {
        &mut self.cpu
    }

    pub fn controller(&mut self) -> &mut Controller {
        self.cpu.memory.controller_mut()
    }

    #[inline]
    pub fn is_persist_required(&self) -> bool {
        let controller = &self.cpu.memory.controller();
        let ram = &controller.ram;
        let rtc = &controller.rtc;
        ram.is_some() || rtc.is_some()
    }

    #[inline]
    pub fn is_persist_ram(&self) -> bool {
        let controller = &self.cpu.memory.controller();
        let ram = &controller.ram;
        ram.is_some()
    }

    #[inline]
    pub fn is_persist_rtc(&self) -> bool {
        let controller = &self.cpu.memory.controller();
        let rtc = &controller.rtc;
        rtc.is_some()
    }

    /// Returns raw persisted state for this Gameboy (i.e., RAM and/or RTC).
    ///
    /// This data can be used by the emulator to "persist" state across runs
    /// of a ROM. Note that it should be sufficient to call this method once
    /// per frame.
    ///
    /// For cartridge RAM, the contents of the RAM will only be returned if
    /// a write has occurred since the last frame.
    pub fn persist(&mut self) -> Option<GameboyState<'_>> {
        if !self.is_persist_required() {
            return None;
        }

        let (mut ram_data, mut rtc_data) = (None, None);
        let controller = self.cpu.memory.controller_mut();

        if let Some(ram) = &mut controller.ram {
            if ram.is_dirty {
                ram.is_dirty = false;
                ram_data = Some(ram.data());
            }
        }

        if let Some(rtc) = &controller.rtc {
            rtc_data = Some(rtc.dump());
        }

        let state = GameboyState {
            ram: ram_data,
            rtc: rtc_data,
        };

        Some(state)
    }

    /// Load persisted state into this `Gameboy`.
    pub fn unpersist<T, U>(&mut self, ram: Option<T>, rtc: Option<U>) -> Result<()>
    where
        T: AsRef<[u8]>,
        U: AsRef<[u8]>,
    {
        if !self.is_persist_required() {
            return Ok(());
        }

        if let Some(ram) = ram {
            self.cpu.memory.controller_mut().load_ram(ram.as_ref())?;
        }

        if let Some(rtc) = rtc {
            self.cpu.memory.controller_mut().load_rtc(rtc.as_ref())?;
        }

        Ok(())
    }

    /// Drain accumulated audio samples (stereo interleaved i16).
    pub fn drain_audio(&mut self) -> Vec<i16> {
        self.cpu.memory.io_mut().apu.drain_samples()
    }

    /// Returns a String containing the serial output of this Gameboy _so far_.
    ///
    /// In other words, this output is cumulative and contains every character
    /// logged to serial since the start of Gameboy.
    pub fn serial_output(&self) -> String {
        self.cpu.memory.io().serial_buffer().into_iter().collect()
    }
}
