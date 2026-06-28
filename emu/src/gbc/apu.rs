use crate::gbc::error::Result;

const CPU_FREQ: u64 = 4_194_304;
const SAMPLE_RATE: u64 = 44_100;
const CYCLES_PER_SAMPLE: u64 = CPU_FREQ / SAMPLE_RATE;
const CYCLES_PER_SAMPLE_REM: u64 = CPU_FREQ % SAMPLE_RATE;
const FRAME_SEQ_CYCLES: u64 = CPU_FREQ / 512;

const DUTY_PATTERNS: [[u8; 8]; 4] = [
    [0, 0, 0, 0, 0, 0, 0, 1],
    [0, 0, 0, 0, 0, 0, 1, 1],
    [0, 0, 0, 0, 1, 1, 1, 1],
    [0, 0, 0, 1, 1, 1, 1, 1],
];

fn period_12(freq: u16) -> u16 {
    (2048 - freq) * 4
}

fn dac_enabled_ch1(nr12: u8) -> bool {
    nr12 & 0xF8 != 0
}

fn dac_enabled_ch4(nr42: u8) -> bool {
    nr42 & 0xF8 != 0
}

fn square_sample(duty: u8, wave_pos: u8) -> f32 {
    if DUTY_PATTERNS[duty as usize][wave_pos as usize] != 0 { 1.0 } else { -1.0 }
}

struct LengthCounter {
    enabled: bool,
    counter: u16,
}

impl LengthCounter {
    fn new() -> Self {
        Self { enabled: false, counter: 0 }
    }

    fn clock(&mut self, channel_enabled: &mut bool) {
        if !self.enabled { return; }
        if self.counter > 0 {
            self.counter -= 1;
            if self.counter == 0 {
                *channel_enabled = false;
            }
        }
    }

    fn trigger(&mut self, load: u8, max: u16) {
        self.counter = max - (load & 0x3F) as u16;
    }
}

struct VolumeEnvelope {
    initial_vol: u8,
    direction: bool,
    period: u8,
    timer: u8,
    cur_vol: u8,
}

impl VolumeEnvelope {
    fn new() -> Self {
        Self { initial_vol: 0, direction: false, period: 0, timer: 0, cur_vol: 0 }
    }

    fn clock(&mut self) {
        if self.period == 0 { return; }
        self.timer = self.timer.saturating_sub(1);
        if self.timer == 0 {
            self.timer = self.period;
            if self.direction {
                if self.cur_vol < 15 { self.cur_vol += 1; }
            } else {
                if self.cur_vol > 0 { self.cur_vol -= 1; }
            }
        }
    }

    fn trigger(&mut self) {
        self.cur_vol = self.initial_vol;
        self.timer = self.period;
    }
}

struct FrequencySweep {
    period: u8,
    direction: bool,
    shift: u8,
    timer: u8,
    shadow_freq: u16,
    enabled: bool,
}

impl FrequencySweep {
    fn new() -> Self {
        Self { period: 0, direction: false, shift: 0, timer: 0, shadow_freq: 0, enabled: false }
    }

    fn clock(&mut self, freq: &mut u16, channel_enabled: &mut bool) {
        if !self.enabled { return; }
        self.timer = self.timer.saturating_sub(1);
        if self.timer > 0 { return; }
        if self.period > 0 {
            self.timer = self.period;
        } else {
            return;
        }
        if self.shift == 0 { return; }

        let offset = self.shadow_freq >> self.shift;
        let new_freq = if self.direction {
            self.shadow_freq.wrapping_add(offset)
        } else {
            self.shadow_freq.wrapping_sub(offset)
        };

        if new_freq > 2047 {
            *channel_enabled = false;
            return;
        }

        self.shadow_freq = new_freq;
        *freq = new_freq;
    }

    fn trigger(&mut self, freq: u16, period: u8, direction: bool, shift: u8) {
        self.shadow_freq = freq;
        self.period = period;
        self.direction = direction;
        self.shift = shift;
        self.timer = if period > 0 { period } else { 8 };
        self.enabled = period > 0 || shift > 0;
    }
}

struct SquareChannel {
    duty: u8,
    length_data: u8,
    initial_vol: u8,
    env_direction: bool,
    env_period: u8,
    freq: u16,
    length_enable: bool,

    enabled: bool,
    length: LengthCounter,
    envelope: VolumeEnvelope,
    freq_timer: u16,
    wave_pos: u8,
}

impl SquareChannel {
    fn new() -> Self {
        Self {
            duty: 0, length_data: 0,
            initial_vol: 0, env_direction: false, env_period: 0,
            freq: 0, length_enable: false,
            enabled: false, length: LengthCounter::new(),
            envelope: VolumeEnvelope::new(),
            freq_timer: 0, wave_pos: 0,
        }
    }

    fn is_on(&self) -> bool {
        self.enabled && self.dac_enabled()
    }

    fn dac_enabled(&self) -> bool {
        self.initial_vol != 0 || self.env_direction
    }

    fn step(&mut self, cycles: u64) {
        if !self.enabled { return; }
        let period = period_12(self.freq) as u64;
        let mut c = cycles;
        let ft = self.freq_timer as u64;

        if c <= ft {
            self.freq_timer = (ft - c) as u16;
            return;
        }
        c -= ft;

        if period > 0 {
            let full = c / (period + 1);
            c -= full * (period + 1);

            if c > 0 {
                self.freq_timer = (period - (c - 1)) as u16;
                self.wave_pos = (self.wave_pos + (full + 1) as u8) & 7;
            } else {
                self.freq_timer = 0;
                self.wave_pos = (self.wave_pos + full as u8) & 7;
            }
        } else {
            self.freq_timer = 0;
            self.wave_pos = (self.wave_pos + c as u8) & 7;
        }
    }

    fn sample(&self) -> f32 {
        if !self.is_on() { return 0.0; }
        let amp = self.envelope.cur_vol as f32 / 15.0;
        square_sample(self.duty, self.wave_pos) * amp
    }

    fn trigger(&mut self) {
        self.enabled = true;
        self.length.trigger(self.length_data, 64);
        self.envelope.trigger();
        self.freq_timer = period_12(self.freq);
        self.wave_pos = 0;
    }

    pub fn save(&self, buf: &mut Vec<u8>) {
        buf.push(self.duty);
        buf.push(self.length_data);
        buf.push(self.initial_vol);
        buf.push(self.env_direction as u8);
        buf.push(self.env_period);
        buf.extend_from_slice(&self.freq.to_le_bytes());
        buf.push(self.length_enable as u8);
        buf.push(self.enabled as u8);
        buf.extend_from_slice(&self.length.counter.to_le_bytes());
        buf.push(self.length.enabled as u8);
        buf.push(self.envelope.cur_vol);
        buf.push(self.envelope.timer);
        buf.extend_from_slice(&self.freq_timer.to_le_bytes());
        buf.push(self.wave_pos);
    }

    pub fn load(&mut self, data: &[u8], pos: &mut usize) {
        self.duty = data[*pos]; *pos += 1;
        self.length_data = data[*pos]; *pos += 1;
        self.initial_vol = data[*pos]; *pos += 1;
        self.env_direction = data[*pos] != 0; *pos += 1;
        self.env_period = data[*pos]; *pos += 1;
        self.freq = u16::from_le_bytes([data[*pos], data[*pos + 1]]); *pos += 2;
        self.length_enable = data[*pos] != 0; *pos += 1;
        self.enabled = data[*pos] != 0; *pos += 1;
        self.length.counter = u16::from_le_bytes([data[*pos], data[*pos + 1]]); *pos += 2;
        self.length.enabled = data[*pos] != 0; *pos += 1;
        self.envelope.cur_vol = data[*pos]; *pos += 1;
        self.envelope.timer = data[*pos]; *pos += 1;
        self.freq_timer = u16::from_le_bytes([data[*pos], data[*pos + 1]]); *pos += 2;
        self.wave_pos = data[*pos]; *pos += 1;
    }
}

struct WaveChannel {
    dac_enabled: bool,
    length_data: u8,
    vol_code: u8,
    freq: u16,
    length_enable: bool,

    enabled: bool,
    length: LengthCounter,
    freq_timer: u16,
    sample_pos: u8,
    wave_ram: [u8; 16],
}

impl WaveChannel {
    fn new() -> Self {
        Self {
            dac_enabled: false, length_data: 0, vol_code: 0,
            freq: 0, length_enable: false,
            enabled: false, length: LengthCounter::new(),
            freq_timer: 0, sample_pos: 0, wave_ram: [0; 16],
        }
    }

    fn is_on(&self) -> bool {
        self.enabled && self.dac_enabled
    }

    fn wave_sample(&self) -> u8 {
        let byte_idx = (self.sample_pos / 2) as usize;
        let nibble = if self.sample_pos & 1 == 0 {
            self.wave_ram[byte_idx] >> 4
        } else {
            self.wave_ram[byte_idx] & 0x0F
        };
        nibble
    }

    fn step(&mut self, cycles: u64) {
        if !self.enabled { return; }
        let period = period_12(self.freq) as u64;
        let mut c = cycles;
        let ft = self.freq_timer as u64;

        if c <= ft {
            self.freq_timer = (ft - c) as u16;
            return;
        }
        c -= ft;

        if period > 0 {
            let full = c / (period + 1);
            c -= full * (period + 1);

            if c > 0 {
                self.freq_timer = (period - (c - 1)) as u16;
                self.sample_pos = (self.sample_pos + (full + 1) as u8) & 31;
            } else {
                self.freq_timer = 0;
                self.sample_pos = (self.sample_pos + full as u8) & 31;
            }
        } else {
            self.sample_pos = (self.sample_pos + c as u8) & 31;
            self.freq_timer = 0;
        }
    }

    fn sample(&self) -> f32 {
        if !self.is_on() { return 0.0; }
        let nibble = self.wave_sample();
        let vol = match self.vol_code {
            0 => 0,
            1 => nibble,
            2 => nibble >> 1,
            3 => nibble >> 2,
            _ => 0,
        };
        vol as f32 / 15.0
    }

    fn trigger(&mut self) {
        self.enabled = true;
        self.length.counter = 256 - self.length_data as u16;
        self.freq_timer = period_12(self.freq);
        self.sample_pos = 0;
    }

    pub fn save(&self, buf: &mut Vec<u8>) {
        buf.push(self.dac_enabled as u8);
        buf.push(self.length_data);
        buf.push(self.vol_code);
        buf.extend_from_slice(&self.freq.to_le_bytes());
        buf.push(self.length_enable as u8);
        buf.push(self.enabled as u8);
        buf.extend_from_slice(&self.length.counter.to_le_bytes());
        buf.push(self.length.enabled as u8);
        buf.extend_from_slice(&self.freq_timer.to_le_bytes());
        buf.push(self.sample_pos);
        buf.extend_from_slice(&self.wave_ram);
    }

    pub fn load(&mut self, data: &[u8], pos: &mut usize) {
        self.dac_enabled = data[*pos] != 0; *pos += 1;
        self.length_data = data[*pos]; *pos += 1;
        self.vol_code = data[*pos]; *pos += 1;
        self.freq = u16::from_le_bytes([data[*pos], data[*pos + 1]]); *pos += 2;
        self.length_enable = data[*pos] != 0; *pos += 1;
        self.enabled = data[*pos] != 0; *pos += 1;
        self.length.counter = u16::from_le_bytes([data[*pos], data[*pos + 1]]); *pos += 2;
        self.length.enabled = data[*pos] != 0; *pos += 1;
        self.freq_timer = u16::from_le_bytes([data[*pos], data[*pos + 1]]); *pos += 2;
        self.sample_pos = data[*pos]; *pos += 1;
        self.wave_ram.copy_from_slice(&data[*pos..*pos + 16]); *pos += 16;
    }
}

struct NoiseChannel {
    initial_vol: u8,
    env_direction: bool,
    env_period: u8,
    divisor_code: u8,
    clock_shift: u8,
    width_mode: bool,
    length_enable: bool,

    enabled: bool,
    length: LengthCounter,
    envelope: VolumeEnvelope,
    lfsr: u16,
    freq_timer: u16,
}

impl NoiseChannel {
    fn new() -> Self {
        Self {
            initial_vol: 0, env_direction: false, env_period: 0,
            divisor_code: 0, clock_shift: 0, width_mode: false, length_enable: false,
            enabled: false, length: LengthCounter::new(),
            envelope: VolumeEnvelope::new(),
            lfsr: 0x7FFF, freq_timer: 0,
        }
    }

    fn is_on(&self) -> bool {
        self.enabled && self.dac_enabled()
    }

    fn dac_enabled(&self) -> bool {
        self.initial_vol != 0 || self.env_direction
    }

    fn noise_period(&self) -> u16 {
        let base = match self.divisor_code {
            0 => 8, 1 => 16, 2 => 32, 3 => 48,
            4 => 64, 5 => 80, 6 => 96, 7 => 112,
            _ => unreachable!(),
        };
        (base as u32).wrapping_shl(self.clock_shift as u32) as u16
    }

    fn step(&mut self, cycles: u64) {
        if !self.enabled { return; }
        let period = self.noise_period() as u64;
        let mut c = cycles;
        let ft = self.freq_timer as u64;

        if c <= ft {
            self.freq_timer = (ft - c) as u16;
            return;
        }
        c -= ft;

        if period > 0 {
            let full = c / (period + 1);
            c -= full * (period + 1);

            self.advance_lfsr(full);

            if c > 0 {
                self.freq_timer = (period - (c - 1)) as u16;
                self.advance_lfsr(1);
            } else {
                self.freq_timer = 0;
            }
        } else {
            self.advance_lfsr(c);
            self.freq_timer = 0;
        }
    }

    fn advance_lfsr(&mut self, count: u64) {
        for _ in 0..count {
            let xor_bit = (self.lfsr & 1) ^ ((self.lfsr >> 1) & 1);
            self.lfsr >>= 1;
            self.lfsr |= xor_bit << 14;
            if self.width_mode {
                self.lfsr &= !(1 << 6);
                self.lfsr |= xor_bit << 6;
            }
        }
    }

    fn sample(&self) -> f32 {
        if !self.is_on() { return 0.0; }
        let amp = self.envelope.cur_vol as f32 / 15.0;
        if self.lfsr & 1 == 0 { amp } else { -amp }
    }

    fn trigger(&mut self) {
        self.enabled = true;
        self.length.trigger(0, 64);
        self.envelope.trigger();
        self.lfsr = 0x7FFF;
        self.freq_timer = self.noise_period();
    }

    pub fn save(&self, buf: &mut Vec<u8>) {
        buf.push(self.initial_vol);
        buf.push(self.env_direction as u8);
        buf.push(self.env_period);
        buf.push(self.divisor_code);
        buf.push(self.clock_shift);
        buf.push(self.width_mode as u8);
        buf.push(self.length_enable as u8);
        buf.push(self.enabled as u8);
        buf.extend_from_slice(&self.length.counter.to_le_bytes());
        buf.push(self.length.enabled as u8);
        buf.push(self.envelope.cur_vol);
        buf.push(self.envelope.timer);
        buf.extend_from_slice(&self.lfsr.to_le_bytes());
        buf.extend_from_slice(&self.freq_timer.to_le_bytes());
    }

    pub fn load(&mut self, data: &[u8], pos: &mut usize) {
        self.initial_vol = data[*pos]; *pos += 1;
        self.env_direction = data[*pos] != 0; *pos += 1;
        self.env_period = data[*pos]; *pos += 1;
        self.divisor_code = data[*pos]; *pos += 1;
        self.clock_shift = data[*pos]; *pos += 1;
        self.width_mode = data[*pos] != 0; *pos += 1;
        self.length_enable = data[*pos] != 0; *pos += 1;
        self.enabled = data[*pos] != 0; *pos += 1;
        self.length.counter = u16::from_le_bytes([data[*pos], data[*pos + 1]]); *pos += 2;
        self.length.enabled = data[*pos] != 0; *pos += 1;
        self.envelope.cur_vol = data[*pos]; *pos += 1;
        self.envelope.timer = data[*pos]; *pos += 1;
        self.lfsr = u16::from_le_bytes([data[*pos], data[*pos + 1]]); *pos += 2;
        self.freq_timer = u16::from_le_bytes([data[*pos], data[*pos + 1]]); *pos += 2;
    }
}

pub struct Apu {
    ch1: SquareChannel,
    ch1_sweep: FrequencySweep,
    ch2: SquareChannel,
    ch3: WaveChannel,
    ch4: NoiseChannel,

    nr50: u8,
    nr51: u8,
    nr52: u8,

    frame_step: u8,
    frame_cycle: u64,
    sample_cycle: u64,
    sample_frac: u64,
    buffer: Vec<i16>,

    pub ch1_nr10: u8,
    pub ch1_nr11: u8,
    pub ch1_nr12: u8,
    pub ch2_nr11: u8,
    pub ch2_nr12: u8,
    pub ch3_nr30: u8,
    pub ch3_nr31: u8,
    pub ch3_nr32: u8,
    pub ch4_nr42: u8,
    pub ch4_nr43: u8,
    
    // High-Pass Filter (HPF) state
    hpf_left: f32,
    hpf_right: f32,
    prev_left: f32,
    prev_right: f32,

    // Cached volume scalars (only recomputed on nr50 write)
    left_vol: f32,
    right_vol: f32,
}

impl Apu {
    pub fn new() -> Self {
        Self {
            ch1: SquareChannel::new(),
            ch1_sweep: FrequencySweep::new(),
            ch2: SquareChannel::new(),
            ch3: WaveChannel::new(),
            ch4: NoiseChannel::new(),
            nr50: 0, nr51: 0, nr52: 0,
            frame_step: 0, frame_cycle: 0, sample_cycle: 0,
            sample_frac: 0,
            buffer: Vec::with_capacity(2048),
            ch1_nr10: 0, ch1_nr11: 0, ch1_nr12: 0,
            ch2_nr11: 0, ch2_nr12: 0,
            ch3_nr30: 0, ch3_nr31: 0, ch3_nr32: 0,
            ch4_nr42: 0, ch4_nr43: 0,
            
            hpf_left: 0.0, hpf_right: 0.0,
            prev_left: 0.0, prev_right: 0.0,

            left_vol: 0.0, right_vol: 0.0,
        }
    }

    pub fn nr52(&self) -> u8 {
        let mut val = self.nr52 & 0x80;
        if self.ch1.enabled { val |= 0x01; }
        if self.ch2.enabled { val |= 0x02; }
        if self.ch3.enabled { val |= 0x04; }
        if self.ch4.enabled { val |= 0x08; }
        val
    }

    pub fn drain_samples(&mut self) -> Vec<i16> {
        std::mem::take(&mut self.buffer)
    }

    fn mix(&self) -> (f32, f32) {
        let ch1_out = self.ch1.sample();
        let ch2_out = self.ch2.sample();
        let ch3_out = self.ch3.sample();
        let ch4_out = self.ch4.sample();

        let mut left = 0.0f32;
        let mut right = 0.0f32;

        if self.nr51 & 0x01 != 0 { left += ch1_out; }
        if self.nr51 & 0x02 != 0 { left += ch2_out; }
        if self.nr51 & 0x04 != 0 { left += ch3_out; }
        if self.nr51 & 0x08 != 0 { left += ch4_out; }

        if self.nr51 & 0x10 != 0 { right += ch1_out; }
        if self.nr51 & 0x20 != 0 { right += ch2_out; }
        if self.nr51 & 0x40 != 0 { right += ch3_out; }
        if self.nr51 & 0x80 != 0 { right += ch4_out; }

        ((left / 4.0) * self.left_vol, (right / 4.0) * self.right_vol)
    }

    pub fn step(&mut self, cycles: u64) {
        if self.nr52 & 0x80 == 0 {
            // When APU is off, still generate 0-value samples to keep frontend synchronized!
            self.sample_cycle += cycles;
            while self.sample_cycle >= CYCLES_PER_SAMPLE {
                let needed_cycles = if self.sample_frac + CYCLES_PER_SAMPLE_REM >= SAMPLE_RATE {
                    CYCLES_PER_SAMPLE + 1
                } else {
                    CYCLES_PER_SAMPLE
                };
                
                if self.sample_cycle < needed_cycles { break; }
                self.sample_cycle -= needed_cycles;
                self.sample_frac += CYCLES_PER_SAMPLE_REM;
                if self.sample_frac >= SAMPLE_RATE {
                    self.sample_frac -= SAMPLE_RATE;
                }
                
                let raw_l = 0.0;
                let raw_r = 0.0;

                // Continue to run HPF so the residual offset decays to 0 without popping
                let r_factor = 0.995;
                self.hpf_left = raw_l - self.prev_left + r_factor * self.hpf_left;
                self.hpf_right = raw_r - self.prev_right + r_factor * self.hpf_right;
                self.prev_left = raw_l;
                self.prev_right = raw_r;

                let out_l = self.hpf_left.clamp(-1.0, 1.0);
                let out_r = self.hpf_right.clamp(-1.0, 1.0);

                self.buffer.push((out_l * 32767.0) as i16);
                self.buffer.push((out_r * 32767.0) as i16);
            }
            return;
        }

        self.frame_cycle += cycles;
        while self.frame_cycle >= FRAME_SEQ_CYCLES {
            self.frame_cycle -= FRAME_SEQ_CYCLES;

            if self.frame_step == 0 || self.frame_step == 2 ||
               self.frame_step == 4 || self.frame_step == 6 {
                self.ch1.length.clock(&mut self.ch1.enabled);
                self.ch2.length.clock(&mut self.ch2.enabled);
                self.ch3.length.clock(&mut self.ch3.enabled);
                self.ch4.length.clock(&mut self.ch4.enabled);
            }
            if self.frame_step == 2 || self.frame_step == 6 {
                self.ch1_sweep.clock(&mut self.ch1.freq, &mut self.ch1.enabled);
            }
            if self.frame_step == 4 || self.frame_step == 6 {
                self.ch1.envelope.clock();
                self.ch2.envelope.clock();
                self.ch4.envelope.clock();
            }

            self.frame_step = (self.frame_step + 1) & 7;
        }

        self.ch1.step(cycles);
        self.ch2.step(cycles);
        self.ch3.step(cycles);
        self.ch4.step(cycles);

        self.sample_cycle += cycles;
        while self.sample_cycle >= CYCLES_PER_SAMPLE {
            // Adjust to require an extra cycle when fractional remainder overflows to maintain perfect timing
            let needed_cycles = if self.sample_frac + CYCLES_PER_SAMPLE_REM >= SAMPLE_RATE {
                CYCLES_PER_SAMPLE + 1
            } else {
                CYCLES_PER_SAMPLE
            };
            
            if self.sample_cycle < needed_cycles { break; }
            self.sample_cycle -= needed_cycles;
            self.sample_frac += CYCLES_PER_SAMPLE_REM;
            if self.sample_frac >= SAMPLE_RATE {
                self.sample_frac -= SAMPLE_RATE;
            }

            let (raw_l, raw_r) = self.mix();

            // High-pass filter removes sudden DC voltage pops/static when channels are muted
            let r_factor = 0.995;
            self.hpf_left = raw_l - self.prev_left + r_factor * self.hpf_left;
            self.hpf_right = raw_r - self.prev_right + r_factor * self.hpf_right;
            self.prev_left = raw_l;
            self.prev_right = raw_r;

            let out_l = self.hpf_left.clamp(-1.0, 1.0);
            let out_r = self.hpf_right.clamp(-1.0, 1.0);

            self.buffer.push((out_l * 32767.0) as i16);
            self.buffer.push((out_r * 32767.0) as i16);
        }
    }

    pub fn read(&self, addr: u16) -> u8 {
        match addr {
            0xFF10 => 0x80 | (self.ch1_nr10 & 0x77),
            0xFF11 => 0x3F | (self.ch1_nr11 & 0xC0),
            0xFF12 => self.ch1_nr12,
            0xFF13 => 0xFF,
            0xFF14 => 0xBF | (if self.ch1.length_enable { 0x40 } else { 0 }),
            0xFF16 => 0x3F | (self.ch2_nr11 & 0xC0),
            0xFF17 => self.ch2_nr12,
            0xFF18 => 0xFF,
            0xFF19 => 0xBF | (if self.ch2.length_enable { 0x40 } else { 0 }),
            0xFF1A => self.ch3_nr30,
            0xFF1B => 0xFF,
            0xFF1C => self.ch3_nr32 & 0x60,
            0xFF1D => 0xFF,
            0xFF1E => 0xBF | (if self.ch3.length_enable { 0x40 } else { 0 }),
            0xFF20 => 0xFF,
            0xFF21 => self.ch4_nr42,
            0xFF22 => self.ch4_nr43,
            0xFF23 => 0xBF | (if self.ch4.length_enable { 0x40 } else { 0 }),
            0xFF24 => self.nr50,
            0xFF25 => self.nr51,
            0xFF26 => self.nr52(),
            0xFF30..=0xFF3F => {
                self.ch3.wave_ram[(addr - 0xFF30) as usize]
            }
            _ => 0xFF,
        }
    }

    pub fn write(&mut self, addr: u16, value: u8) {
        if addr == 0xFF26 {
            let was_on = self.nr52 & 0x80 != 0;
            let now_on = value & 0x80 != 0;
            if was_on && !now_on {
                self.reset_power();
            } else if !was_on && now_on {
                self.power_on();
            }
            self.nr52 = value & 0x80;
            return;
        }

        if self.nr52 & 0x80 == 0 {
            if addr == 0xFF24 { self.nr50 = value; self.left_vol = (value & 0x07) as f32 / 7.0; self.right_vol = ((value >> 4) & 0x07) as f32 / 7.0; }
            if addr == 0xFF25 { self.nr51 = value; }
            if (0xFF30..=0xFF3F).contains(&addr) { self.ch3.wave_ram[(addr - 0xFF30) as usize] = value; }
            return;
        }

        match addr {
            0xFF10 => {
                self.ch1_nr10 = value;
                self.ch1_sweep.period = (value >> 4) & 0x07;
                self.ch1_sweep.direction = value & 0x08 == 0;
                self.ch1_sweep.shift = value & 0x07;
            }
            0xFF11 => {
                self.ch1_nr11 = value;
                self.ch1.duty = (value >> 6) & 0x03;
                self.ch1.length_data = value & 0x3F;
            }
            0xFF12 => {
                self.ch1_nr12 = value;
                self.ch1.initial_vol = value >> 4;
                self.ch1.env_direction = value & 0x08 != 0;
                self.ch1.env_period = value & 0x07;
                self.ch1.envelope.initial_vol = value >> 4;
                self.ch1.envelope.direction = value & 0x08 != 0;
                self.ch1.envelope.period = value & 0x07;
                if !dac_enabled_ch1(value) { self.ch1.enabled = false; }
            }
            0xFF13 => {
                self.ch1.freq = (self.ch1.freq & 0x0700) | value as u16;
            }
            0xFF14 => {
                self.ch1.freq = (self.ch1.freq & 0x00FF) | ((value as u16 & 0x07) << 8);
                self.ch1.length_enable = value & 0x40 != 0;
                self.ch1.length.enabled = value & 0x40 != 0;
                if value & 0x80 != 0 {
                    self.ch1.trigger();
                    self.ch1_sweep.trigger(
                        self.ch1.freq,
                        (self.ch1_nr10 >> 4) & 0x07,
                        self.ch1_nr10 & 0x08 == 0,
                        self.ch1_nr10 & 0x07,
                    );
                }
            }
            0xFF16 => {
                self.ch2_nr11 = value;
                self.ch2.duty = (value >> 6) & 0x03;
                self.ch2.length_data = value & 0x3F;
            }
            0xFF17 => {
                self.ch2_nr12 = value;
                self.ch2.initial_vol = value >> 4;
                self.ch2.env_direction = value & 0x08 != 0;
                self.ch2.env_period = value & 0x07;
                self.ch2.envelope.initial_vol = value >> 4;
                self.ch2.envelope.direction = value & 0x08 != 0;
                self.ch2.envelope.period = value & 0x07;
                if !dac_enabled_ch1(value) { self.ch2.enabled = false; }
            }
            0xFF18 => {
                self.ch2.freq = (self.ch2.freq & 0x0700) | value as u16;
            }
            0xFF19 => {
                self.ch2.freq = (self.ch2.freq & 0x00FF) | ((value as u16 & 0x07) << 8);
                self.ch2.length_enable = value & 0x40 != 0;
                self.ch2.length.enabled = value & 0x40 != 0;
                if value & 0x80 != 0 { self.ch2.trigger(); }
            }
            0xFF1A => {
                self.ch3_nr30 = value;
                self.ch3.dac_enabled = value & 0x80 != 0;
                if !self.ch3.dac_enabled { self.ch3.enabled = false; }
            }
            0xFF1B => {
                self.ch3_nr31 = value;
                self.ch3.length_data = value;
            }
            0xFF1C => {
                self.ch3_nr32 = value;
                self.ch3.vol_code = (value >> 5) & 0x03;
            }
            0xFF1D => {
                self.ch3.freq = (self.ch3.freq & 0x0700) | value as u16;
            }
            0xFF1E => {
                self.ch3.freq = (self.ch3.freq & 0x00FF) | ((value as u16 & 0x07) << 8);
                self.ch3.length_enable = value & 0x40 != 0;
                self.ch3.length.enabled = value & 0x40 != 0;
                if value & 0x80 != 0 { self.ch3.trigger(); }
            }
            0xFF20 => {
            }
            0xFF21 => {
                self.ch4_nr42 = value;
                self.ch4.initial_vol = value >> 4;
                self.ch4.env_direction = value & 0x08 != 0;
                self.ch4.env_period = value & 0x07;
                self.ch4.envelope.initial_vol = value >> 4;
                self.ch4.envelope.direction = value & 0x08 != 0;
                self.ch4.envelope.period = value & 0x07;
                if !dac_enabled_ch4(value) { self.ch4.enabled = false; }
            }
            0xFF22 => {
                self.ch4_nr43 = value;
                self.ch4.clock_shift = value >> 4;
                self.ch4.width_mode = value & 0x08 != 0;
                self.ch4.divisor_code = value & 0x07;
            }
            0xFF23 => {
                self.ch4.length_enable = value & 0x40 != 0;
                self.ch4.length.enabled = value & 0x40 != 0;
                if value & 0x80 != 0 { self.ch4.trigger(); }
            }
            0xFF24 => {
                self.nr50 = value;
                self.left_vol = (value & 0x07) as f32 / 7.0;
                self.right_vol = ((value >> 4) & 0x07) as f32 / 7.0;
            }
            0xFF25 => {
                self.nr51 = value;
            }
            0xFF30..=0xFF3F => {
                self.ch3.wave_ram[(addr - 0xFF30) as usize] = value;
            }
            _ => {}
        }
    }

    fn reset_power(&mut self) {
        self.ch1 = SquareChannel::new();
        self.ch1_sweep = FrequencySweep::new();
        self.ch2 = SquareChannel::new();
        self.ch3 = WaveChannel::new();
        self.ch4 = NoiseChannel::new();
        self.nr50 = 0;
        self.nr51 = 0;
        self.ch1_nr10 = 0;
        self.ch1_nr11 = 0;
        self.ch1_nr12 = 0;
        self.ch2_nr11 = 0;
        self.ch2_nr12 = 0;
        self.ch3_nr30 = 0;
        self.ch3_nr31 = 0;
        self.ch3_nr32 = 0;
        self.ch4_nr42 = 0;
        self.ch4_nr43 = 0;
        self.hpf_left = 0.0;
        self.hpf_right = 0.0;
        self.prev_left = 0.0;
        self.prev_right = 0.0;
    }

    fn power_on(&mut self) {
    }

    pub fn save_to_bytes(&self, buf: &mut Vec<u8>) {
        self.ch1.save(buf);
        buf.push(self.ch1_sweep.period);
        buf.push(self.ch1_sweep.direction as u8);
        buf.push(self.ch1_sweep.shift);
        buf.push(self.ch1_sweep.timer);
        buf.extend_from_slice(&self.ch1_sweep.shadow_freq.to_le_bytes());
        buf.push(self.ch1_sweep.enabled as u8);
        self.ch2.save(buf);
        self.ch3.save(buf);
        self.ch4.save(buf);
        buf.push(self.nr50);
        buf.push(self.nr51);
        buf.push(self.nr52);
        buf.push(self.frame_step);
        buf.extend_from_slice(&self.frame_cycle.to_le_bytes());
        buf.extend_from_slice(&self.sample_cycle.to_le_bytes());
        buf.extend_from_slice(&self.sample_frac.to_le_bytes());
        buf.push(self.ch1_nr10);
        buf.push(self.ch1_nr11);
        buf.push(self.ch1_nr12);
        buf.push(self.ch2_nr11);
        buf.push(self.ch2_nr12);
        buf.push(self.ch3_nr30);
        buf.push(self.ch3_nr31);
        buf.push(self.ch3_nr32);
        buf.push(self.ch4_nr42);
        buf.push(self.ch4_nr43);
        buf.extend_from_slice(&self.hpf_left.to_le_bytes());
        buf.extend_from_slice(&self.hpf_right.to_le_bytes());
        buf.extend_from_slice(&self.prev_left.to_le_bytes());
        buf.extend_from_slice(&self.prev_right.to_le_bytes());
    }

    pub fn load_from_bytes(data: &[u8], pos: &mut usize) -> Result<Self> {
        let mut apu = Self::new();
        apu.ch1.load(data, pos);
        apu.ch1_sweep.period = data[*pos]; *pos += 1;
        apu.ch1_sweep.direction = data[*pos] != 0; *pos += 1;
        apu.ch1_sweep.shift = data[*pos]; *pos += 1;
        apu.ch1_sweep.timer = data[*pos]; *pos += 1;
        apu.ch1_sweep.shadow_freq = u16::from_le_bytes([data[*pos], data[*pos + 1]]); *pos += 2;
        apu.ch1_sweep.enabled = data[*pos] != 0; *pos += 1;
        apu.ch2.load(data, pos);
        apu.ch3.load(data, pos);
        apu.ch4.load(data, pos);
        apu.nr50 = data[*pos]; *pos += 1;
        apu.nr51 = data[*pos]; *pos += 1;
        apu.nr52 = data[*pos]; *pos += 1;
        apu.frame_step = data[*pos]; *pos += 1;
        apu.frame_cycle = u64::from_le_bytes([
            data[*pos], data[*pos+1], data[*pos+2], data[*pos+3],
            data[*pos+4], data[*pos+5], data[*pos+6], data[*pos+7],
        ]); *pos += 8;
        apu.sample_cycle = u64::from_le_bytes([
            data[*pos], data[*pos+1], data[*pos+2], data[*pos+3],
            data[*pos+4], data[*pos+5], data[*pos+6], data[*pos+7],
        ]); *pos += 8;
        apu.sample_frac = u64::from_le_bytes([
            data[*pos], data[*pos+1], data[*pos+2], data[*pos+3],
            data[*pos+4], data[*pos+5], data[*pos+6], data[*pos+7],
        ]); *pos += 8;
        apu.ch1_nr10 = data[*pos]; *pos += 1;
        apu.ch1_nr11 = data[*pos]; *pos += 1;
        apu.ch1_nr12 = data[*pos]; *pos += 1;
        apu.ch2_nr11 = data[*pos]; *pos += 1;
        apu.ch2_nr12 = data[*pos]; *pos += 1;
        apu.ch3_nr30 = data[*pos]; *pos += 1;
        apu.ch3_nr31 = data[*pos]; *pos += 1;
        apu.ch3_nr32 = data[*pos]; *pos += 1;
        apu.ch4_nr42 = data[*pos]; *pos += 1;
        apu.ch4_nr43 = data[*pos]; *pos += 1;
        apu.hpf_left = f32::from_le_bytes([data[*pos], data[*pos+1], data[*pos+2], data[*pos+3]]); *pos += 4;
        apu.hpf_right = f32::from_le_bytes([data[*pos], data[*pos+1], data[*pos+2], data[*pos+3]]); *pos += 4;
        apu.prev_left = f32::from_le_bytes([data[*pos], data[*pos+1], data[*pos+2], data[*pos+3]]); *pos += 4;
        apu.prev_right = f32::from_le_bytes([data[*pos], data[*pos+1], data[*pos+2], data[*pos+3]]); *pos += 4;
        Ok(apu)
    }
}