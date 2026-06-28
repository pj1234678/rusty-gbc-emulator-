use std::time::{SystemTime, UNIX_EPOCH};

use crate::gbc::cpu::Cpu;
use crate::gbc::error::Result;

fn now_seconds() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_secs()
}

#[derive(Clone, Copy, Debug)]
struct RtcTime {
    seconds: u8,
    minutes: u8,
    hours: u8,
    days: u16,
    halt: bool,
    carry: bool,
}

impl RtcTime {
    fn new() -> Self {
        Self {
            seconds: 0,
            minutes: 0,
            hours: 0,
            days: 0,
            halt: false,
            carry: false,
        }
    }
}

#[derive(Debug)]
struct RtcState {
    current: RtcTime,
    latched: RtcTime,
    latch_started: bool,
    timestamp: u64,
    tick_cycle: u64,
    cycle: u64,
    selected: u8,
}

impl RtcState {
    fn new() -> Self {
        Self {
            current: RtcTime::new(),
            latched: RtcTime::new(),
            latch_started: false,
            timestamp: now_seconds(),
            tick_cycle: 0,
            cycle: 0,
            selected: 0,
        }
    }

    fn step(&mut self, cycles: u16, speed: bool) {
        let cycle_time = Cpu::cycle_time(speed) as u64;

        if self.current.halt {
            self.timestamp = now_seconds();
            return;
        }

        self.cycle += cycles as u64;

        if (self.cycle - self.tick_cycle) / cycle_time >= Rtc::TICK_INTERVAL {
            self.tick();
        }
    }

    fn tick(&mut self) {
        let now = now_seconds();
        let seconds = now - self.timestamp;
        self.current.seconds += seconds as u8;

        if self.current.seconds > 0x3B {
            self.current.seconds -= 60;
            self.current.minutes += 1;
        }

        if self.current.minutes > 0x3B {
            self.current.minutes = 0;
            self.current.hours += 1;
        }

        if self.current.hours > 0x17 {
            self.current.hours = 0;
            self.current.days += 1;
        }

        if self.current.days > 0x1FF {
            self.current.days = 0;
            self.current.carry = true;
        }

        self.timestamp = now;
        self.tick_cycle = self.cycle;
    }

    fn select(&mut self, register: u8) {
        self.selected = register;
    }

    fn latch(&mut self, value: u8) {
        if value == 0 {
            self.latch_started = true;
        } else if self.latch_started {
            self.latched = self.current;
            self.latch_started = false;
        }
    }

    fn read(&self) -> u8 {
        match self.selected {
            0x08 => self.latched.seconds,
            0x09 => self.latched.minutes,
            0x0A => self.latched.hours,
            0x0B => (self.latched.days & 0xFF) as u8,
            0x0C => {
                let mut value = (self.latched.days & 1 << 8 >> 8) as u8;
                let halt_bit = if self.latched.halt { 1 } else { 0 };
                let carry_bit = if self.latched.carry { 1 } else { 0 };

                value |= halt_bit << 6;
                value |= carry_bit << 7;

                value
            }
            _ => unreachable!(),
        }
    }

    fn write(&mut self, value: u8) {
        match self.selected {
            0x08 => self.current.seconds = value,
            0x09 => self.current.minutes = value,
            0x0A => self.current.hours = value,
            0x0B => self.current.days |= value as u16,
            0x0C => {
                self.current.days &= !(1 << 8);
                self.current.days |= (value as u16 & 1) << 8;
                self.current.halt = value & 1 << 6 != 0;
                self.current.carry = value & 1 << 7 != 0;
            }
            _ => unreachable!(),
        }
    }

    fn advance(&mut self) {
        let now = now_seconds();

        if self.current.halt {
            self.timestamp = now;
            return;
        }

        let seconds = now - self.timestamp;
        let minutes = seconds / 60;
        let hours = minutes / 60;
        let days = hours / 24;

        self.current.seconds = ((self.current.seconds as u64 + seconds) % 60) as u8;
        self.current.minutes = ((self.current.minutes as u64 + minutes) % 60) as u8;
        self.current.hours = ((self.current.hours as u64 + hours) % 24) as u8;
        self.current.days = ((self.current.days as u64 + days) % 512) as u16;

        self.timestamp = now;
    }
}

pub struct Rtc {
    state: RtcState,
}

impl Rtc {
    const FREQUENCY: f32 = 32768.0;
    const TICK_INTERVAL: u64 = (1e9 / Self::FREQUENCY) as u64;

    pub fn new() -> Self {
        Self {
            state: RtcState::new(),
        }
    }

    pub fn from_bytes(data: &[u8]) -> Result<Self> {
        let state = RtcState::deserialize(data, &mut 0)?;
        Ok(Self { state })
    }

    pub fn dump(&self) -> Vec<u8> {
        let mut buf = Vec::new();
        self.state.serialize(&mut buf);
        buf
    }

    pub fn step(&mut self, cycles: u16, speed: bool) {
        self.state.step(cycles, speed);
    }

    pub fn select(&mut self, register: u8) {
        self.state.select(register);
    }

    pub fn latch(&mut self, value: u8) {
        self.state.latch(value);
    }

    pub fn read(&self) -> u8 {
        self.state.read()
    }

    pub fn write(&mut self, value: u8) {
        self.state.write(value);
    }

    pub fn advance(&mut self) {
        self.state.advance()
    }
}

impl RtcTime {
    fn serialize(&self, buf: &mut Vec<u8>) {
        buf.push(self.seconds);
        buf.push(self.minutes);
        buf.push(self.hours);
        buf.extend_from_slice(&self.days.to_le_bytes());
        buf.push(self.halt as u8);
        buf.push(self.carry as u8);
    }

    fn deserialize(data: &[u8], pos: &mut usize) -> Result<Self> {
        let seconds = data[*pos]; *pos += 1;
        let minutes = data[*pos]; *pos += 1;
        let hours = data[*pos]; *pos += 1;
        let days = u16::from_le_bytes([data[*pos], data[*pos + 1]]); *pos += 2;
        let halt = data[*pos] != 0; *pos += 1;
        let carry = data[*pos] != 0; *pos += 1;
        Ok(Self { seconds, minutes, hours, days, halt, carry })
    }
}

impl RtcState {
    fn serialize(&self, buf: &mut Vec<u8>) {
        self.current.serialize(buf);
        self.latched.serialize(buf);
        buf.push(self.latch_started as u8);
        buf.extend_from_slice(&self.timestamp.to_le_bytes());
        buf.extend_from_slice(&self.tick_cycle.to_le_bytes());
        buf.extend_from_slice(&self.cycle.to_le_bytes());
        buf.push(self.selected);
    }

    fn deserialize(data: &[u8], pos: &mut usize) -> Result<Self> {
        let current = RtcTime::deserialize(data, pos)?;
        let latched = RtcTime::deserialize(data, pos)?;
        let latch_started = data[*pos] != 0; *pos += 1;
        let timestamp = u64::from_le_bytes([
            data[*pos], data[*pos + 1], data[*pos + 2], data[*pos + 3],
            data[*pos + 4], data[*pos + 5], data[*pos + 6], data[*pos + 7],
        ]); *pos += 8;
        let tick_cycle = u64::from_le_bytes([
            data[*pos], data[*pos + 1], data[*pos + 2], data[*pos + 3],
            data[*pos + 4], data[*pos + 5], data[*pos + 6], data[*pos + 7],
        ]); *pos += 8;
        let cycle = u64::from_le_bytes([
            data[*pos], data[*pos + 1], data[*pos + 2], data[*pos + 3],
            data[*pos + 4], data[*pos + 5], data[*pos + 6], data[*pos + 7],
        ]); *pos += 8;
        let selected = data[*pos]; *pos += 1;
        Ok(Self { current, latched, latch_started, timestamp, tick_cycle, cycle, selected })
    }
}
