use std::fs::File;
use std::io::{BufWriter, Write};

use crate::gbc::cartridge::Cartridge;
use crate::gbc::dma::DmaController;
use crate::gbc::error::Result;
use crate::gbc::instructions::{Arg, Cond, Cycles, Instruction};
use crate::gbc::memory::{MemoryBus, MemoryRead, MemoryWrite};
use crate::gbc::registers::{Flag, Reg16, Reg8, RegisterFile, RegisterOps};

/// Result of executing a single opcode via the dispatch tables.
#[derive(Clone)]
pub struct StepResult {
    pub inst: Instruction,
    pub size: u8,
    pub cycles: Cycles,
    pub jump: bool,
    pub taken: bool,
}

/// Handler signature for the opcode dispatch tables.
type OpHandler = fn(cpu: &mut Cpu, opcode: u8, arg8: u8, arg16: u16) -> StepResult;

// ---------------------------------------------------------------------------
// Handler helpers
// ---------------------------------------------------------------------------

const REG8_MAP: [Reg8; 7] = [Reg8::B, Reg8::C, Reg8::D, Reg8::E, Reg8::H, Reg8::L, Reg8::A];

#[inline]


// ---------------------------------------------------------------------------
// Base opcode handlers – one per unique execution pattern (not per opcode)
// ---------------------------------------------------------------------------

fn op_nop(_cpu: &mut Cpu, _op: u8, _a8: u8, _a16: u16) -> StepResult {
    StepResult { inst: Instruction::Nop, size: 1, cycles: Cycles::from(4), jump: false, taken: false }
}

fn op_halt(cpu: &mut Cpu, _op: u8, _a8: u8, _a16: u16) -> StepResult {
    cpu.halted = true;
    StepResult { inst: Instruction::Halt, size: 1, cycles: Cycles::from(4), jump: false, taken: false }
}

fn op_stop(cpu: &mut Cpu, _op: u8, _a8: u8, _a16: u16) -> StepResult {
    if cpu.cgb && cpu.memory.io().prep_speed_switch & 0x1 != 0 {
        cpu.speed_switch();
    }
    cpu.memory.timer().write(0xFF04, 0);
    StepResult { inst: Instruction::Stop, size: 2, cycles: Cycles::from(4), jump: false, taken: false }
}

fn op_di(cpu: &mut Cpu, _op: u8, _a8: u8, _a16: u16) -> StepResult {
    cpu.ime = false;
    StepResult { inst: Instruction::Di, size: 1, cycles: Cycles::from(4), jump: false, taken: false }
}

fn op_ei(cpu: &mut Cpu, _op: u8, _a8: u8, _a16: u16) -> StepResult {
    cpu.ime = true;
    StepResult { inst: Instruction::Ei, size: 1, cycles: Cycles::from(4), jump: false, taken: false }
}

fn op_daa(cpu: &mut Cpu, _op: u8, _a8: u8, _a16: u16) -> StepResult {
    cpu.daa();
    StepResult { inst: Instruction::Daa, size: 1, cycles: Cycles::from(4), jump: false, taken: false }
}

fn op_cpl(cpu: &mut Cpu, _op: u8, _a8: u8, _a16: u16) -> StepResult {
    let curr = cpu.registers.read(Reg8::A);
    cpu.registers.write(Reg8::A, !curr);
    cpu.registers.set(Flag::Subtract, true);
    cpu.registers.set(Flag::HalfCarry, true);
    StepResult { inst: Instruction::Cpl, size: 1, cycles: Cycles::from(4), jump: false, taken: false }
}

fn op_ccf(cpu: &mut Cpu, _op: u8, _a8: u8, _a16: u16) -> StepResult {
    let f = cpu.registers.carry();
    cpu.registers.set(Flag::Carry, !f);
    cpu.registers.set(Flag::Subtract, false);
    cpu.registers.set(Flag::HalfCarry, false);
    StepResult { inst: Instruction::Ccf, size: 1, cycles: Cycles::from(4), jump: false, taken: false }
}

fn op_scf(cpu: &mut Cpu, _op: u8, _a8: u8, _a16: u16) -> StepResult {
    cpu.registers.set(Flag::Carry, true);
    cpu.registers.set(Flag::Subtract, false);
    cpu.registers.set(Flag::HalfCarry, false);
    StepResult { inst: Instruction::Scf, size: 1, cycles: Cycles::from(4), jump: false, taken: false }
}

fn op_reti(cpu: &mut Cpu, _op: u8, _a8: u8, _a16: u16) -> StepResult {
    let addr = cpu.pop();
    cpu.registers.PC = addr;
    cpu.ime = true;
    StepResult { inst: Instruction::RetI, size: 1, cycles: Cycles::from(16), jump: true, taken: true }
}

fn op_jphl(cpu: &mut Cpu, _op: u8, _a8: u8, _a16: u16) -> StepResult {
    let addr = cpu.registers.read(Reg16::HL);
    cpu.registers.PC = addr;
    StepResult { inst: Instruction::JpHl, size: 1, cycles: Cycles::from(4), jump: true, taken: true }
}

// -- LDH / LD A, [0xFF00+C] / LD [0xFF00+C], A ---------------------------------

fn op_ld_memc_a(cpu: &mut Cpu, _op: u8, _a8: u8, _a16: u16) -> StepResult {
    let addr = 0xFF00 + cpu.registers.read(Reg8::C) as u16;
    cpu.memory.write(addr, cpu.registers.read(Reg8::A));
    StepResult { inst: Instruction::LdMemCA, size: 1, cycles: Cycles::from(8), jump: false, taken: false }
}

fn op_ld_a_memc(cpu: &mut Cpu, _op: u8, _a8: u8, _a16: u16) -> StepResult {
    let addr = 0xFF00 + cpu.registers.read(Reg8::C) as u16;
    let val = cpu.memory.read(addr);
    cpu.registers.write(Reg8::A, val);
    StepResult { inst: Instruction::LdAMemC, size: 1, cycles: Cycles::from(8), jump: false, taken: false }
}

fn op_ldh_a(cpu: &mut Cpu, _op: u8, a8: u8, _a16: u16) -> StepResult {
    let val = cpu.memory.read(0xFF00 + a8 as u16);
    cpu.registers.write(Reg8::A, val);
    StepResult { inst: Instruction::LdhA { offset: a8 }, size: 2, cycles: Cycles::from(12), jump: false, taken: false }
}

fn op_ldh(cpu: &mut Cpu, _op: u8, a8: u8, _a16: u16) -> StepResult {
    cpu.memory.write(0xFF00 + a8 as u16, cpu.registers.read(Reg8::A));
    StepResult { inst: Instruction::Ldh { offset: a8 }, size: 2, cycles: Cycles::from(12), jump: false, taken: false }
}

// -- LD r16, imm16 --------------------------------------------------------------

fn op_ld_r16_imm16(cpu: &mut Cpu, op: u8, _a8: u8, a16: u16) -> StepResult {
    let reg = match (op >> 4) & 0x3 {
        0 => Reg16::BC,
        1 => Reg16::DE,
        2 => Reg16::HL,
        _ => Reg16::SP,
    };
    cpu.registers.write(reg, a16);
    let inst = Instruction::Ld { dst: Arg::Reg16(reg), src: Arg::Imm16(a16) };
    StepResult { inst, size: 3, cycles: Cycles::from(12), jump: false, taken: false }
}

// -- LD (mem), A / LD A, (mem) --------------------------------------------------

fn op_ld_a_mem_r16(cpu: &mut Cpu, op: u8, _a8: u8, _a16: u16) -> StepResult {
    let reg = if op == 0x0A { Reg16::BC } else { Reg16::DE };
    let addr = cpu.registers.read(reg);
    cpu.registers.write(Reg8::A, cpu.memory.read(addr));
    let inst = Instruction::Ld { dst: Arg::Reg8(Reg8::A), src: Arg::Mem(reg) };
    StepResult { inst, size: 1, cycles: Cycles::from(8), jump: false, taken: false }
}

fn op_ld_mem_r16_a(cpu: &mut Cpu, op: u8, _a8: u8, _a16: u16) -> StepResult {
    let reg = if op == 0x02 { Reg16::BC } else { Reg16::DE };
    let addr = cpu.registers.read(reg);
    cpu.memory.write(addr, cpu.registers.read(Reg8::A));
    let inst = Instruction::Ld { dst: Arg::Mem(reg), src: Arg::Reg8(Reg8::A) };
    StepResult { inst, size: 1, cycles: Cycles::from(8), jump: false, taken: false }
}

// -- LD r8, imm8 ----------------------------------------------------------------

fn op_ld_r8_imm8(cpu: &mut Cpu, op: u8, a8: u8, _a16: u16) -> StepResult {
    let idx = (op >> 3) & 0x7;
    let r = REG8_MAP[(if idx == 7 { 6 } else { idx }) as usize];
    cpu.registers.write(r, a8);
    StepResult { inst: Instruction::Ld { dst: Arg::Reg8(r), src: Arg::Imm8(a8) }, size: 2, cycles: Cycles::from(8), jump: false, taken: false }
}

// Fix: 0x36 is LD (HL), imm8 (12 cycles, handled separately)
fn op_ld_memhl_imm8(cpu: &mut Cpu, _op: u8, a8: u8, _a16: u16) -> StepResult {
    let addr = cpu.registers.read(Reg16::HL);
    cpu.memory.write(addr, a8);
    StepResult { inst: Instruction::Ld { dst: Arg::Mem(Reg16::HL), src: Arg::Imm8(a8) }, size: 2, cycles: Cycles::from(12), jump: false, taken: false }
}

// -- LD r8, r8  (0x40-0x7F) ----------------------------------------------------

fn op_ld_r8_r8(cpu: &mut Cpu, op: u8, _a8: u8, _a16: u16) -> StepResult {
    let dst_idx = (op >> 3) & 0x7;
    let src_idx = op & 0x7;
    let (cycles, size, inst) = match (dst_idx, src_idx) {
        (6, 6) => unreachable!(),
        (6, _) => {
            let addr = cpu.registers.read(Reg16::HL);
            let val = if src_idx == 7 { cpu.registers.read(Reg8::A) } else { cpu.registers.read(REG8_MAP[src_idx as usize]) };
            cpu.memory.write(addr, val);
            (8, 1, Instruction::Ld { dst: Arg::Mem(Reg16::HL), src: Arg::Reg8(REG8_MAP[src_idx.min(6) as usize]) })
        }
        (_, 6) => {
            let addr = cpu.registers.read(Reg16::HL);
            let val = cpu.memory.read(addr);
            let r = if dst_idx == 7 { Reg8::A } else { REG8_MAP[dst_idx as usize] };
            cpu.registers.write(r, val);
            (8, 1, Instruction::Ld { dst: Arg::Reg8(r), src: Arg::Mem(Reg16::HL) })
        }
        (_, _) => {
            let dst = if dst_idx == 7 { Reg8::A } else { REG8_MAP[dst_idx as usize] };
            let src = if src_idx == 7 { Reg8::A } else { REG8_MAP[src_idx as usize] };
            let val = cpu.registers.read(src);
            cpu.registers.write(dst, val);
            (4, 1, Instruction::Ld { dst: Arg::Reg8(dst), src: Arg::Reg8(src) })
        }
    };
    StepResult { inst, size, cycles: Cycles::from(cycles), jump: false, taken: false }
}

// -- LDI / LDD -----------------------------------------------------------------

fn op_ldi_a_memhl(cpu: &mut Cpu, _op: u8, _a8: u8, _a16: u16) -> StepResult {
    let addr = cpu.registers.read(Reg16::HL);
    cpu.registers.write(Reg8::A, cpu.memory.read(addr));
    cpu.registers.write(Reg16::HL, addr.wrapping_add(1));
    StepResult { inst: Instruction::LdiAMemHl, size: 1, cycles: Cycles::from(8), jump: false, taken: false }
}

fn op_ldi_memhl_a(cpu: &mut Cpu, _op: u8, _a8: u8, _a16: u16) -> StepResult {
    let addr = cpu.registers.read(Reg16::HL);
    cpu.memory.write(addr, cpu.registers.read(Reg8::A));
    cpu.registers.write(Reg16::HL, addr.wrapping_add(1));
    StepResult { inst: Instruction::LdiMemHlA, size: 1, cycles: Cycles::from(8), jump: false, taken: false }
}

fn op_ldd_a_memhl(cpu: &mut Cpu, _op: u8, _a8: u8, _a16: u16) -> StepResult {
    let addr = cpu.registers.read(Reg16::HL);
    cpu.registers.write(Reg8::A, cpu.memory.read(addr));
    cpu.registers.write(Reg16::HL, addr.wrapping_sub(1));
    StepResult { inst: Instruction::LddAMemHl, size: 1, cycles: Cycles::from(8), jump: false, taken: false }
}

fn op_ldd_memhl_a(cpu: &mut Cpu, _op: u8, _a8: u8, _a16: u16) -> StepResult {
    let addr = cpu.registers.read(Reg16::HL);
    cpu.memory.write(addr, cpu.registers.read(Reg8::A));
    cpu.registers.write(Reg16::HL, addr.wrapping_sub(1));
    StepResult { inst: Instruction::LddMemHlA, size: 1, cycles: Cycles::from(8), jump: false, taken: false }
}

// -- LD A, (imm16) / LD (imm16), A / LD (imm16), SP ---------------------------

fn op_ld_a_memimm(cpu: &mut Cpu, _op: u8, _a8: u8, a16: u16) -> StepResult {
    cpu.registers.write(Reg8::A, cpu.memory.read(a16));
    StepResult { inst: Instruction::Ld { dst: Arg::Reg8(Reg8::A), src: Arg::MemImm(a16) }, size: 3, cycles: Cycles::from(16), jump: false, taken: false }
}

fn op_ld_memimm_a(cpu: &mut Cpu, _op: u8, _a8: u8, a16: u16) -> StepResult {
    cpu.memory.write(a16, cpu.registers.read(Reg8::A));
    StepResult { inst: Instruction::Ld { dst: Arg::MemImm(a16), src: Arg::Reg8(Reg8::A) }, size: 3, cycles: Cycles::from(16), jump: false, taken: false }
}

fn op_ld_memimm_sp(cpu: &mut Cpu, _op: u8, _a8: u8, a16: u16) -> StepResult {
    cpu.memory.write(a16, cpu.registers.read(Reg16::SP));
    StepResult { inst: Instruction::Ld { dst: Arg::MemImm(a16), src: Arg::Reg16(Reg16::SP) }, size: 3, cycles: Cycles::from(20), jump: false, taken: false }
}

fn op_ld_sp_hl(cpu: &mut Cpu, _op: u8, _a8: u8, _a16: u16) -> StepResult {
    cpu.registers.write(Reg16::SP, cpu.registers.read(Reg16::HL));
    StepResult { inst: Instruction::Ld { dst: Arg::Reg16(Reg16::SP), src: Arg::Reg16(Reg16::HL) }, size: 1, cycles: Cycles::from(8), jump: false, taken: false }
}

fn op_ld_hl_sp_imm8(cpu: &mut Cpu, _op: u8, a8: u8, _a16: u16) -> StepResult {
    let offset = (a8 as i8) as i16 as u16;
    let val = cpu.registers.read(Reg16::SP);
    let half_carry = ((val & 0xF) + (offset & 0xF)) & 0x10 != 0;
    let carry = ((val & 0xFF) + (offset & 0xFF)) & 0x100 != 0;
    cpu.registers.write(Reg16::HL, val.wrapping_add(offset));
    cpu.registers.set(Flag::Zero, false);
    cpu.registers.set(Flag::Subtract, false);
    cpu.registers.set(Flag::HalfCarry, half_carry);
    cpu.registers.set(Flag::Carry, carry);
    StepResult { inst: Instruction::LdHlSpImm8i { offset: a8 as i8 }, size: 2, cycles: Cycles::from(12), jump: false, taken: false }
}

fn op_add_sp_imm8(cpu: &mut Cpu, _op: u8, a8: u8, _a16: u16) -> StepResult {
    let offset = (a8 as i8) as i16 as u16;
    let val = cpu.registers.read(Reg16::SP);
    let half_carry = ((val & 0xF) + (offset & 0xF)) & 0x10 != 0;
    let carry = ((val & 0xFF) + (offset & 0xFF)) & 0x100 != 0;
    cpu.registers.write(Reg16::SP, val.wrapping_add(offset));
    cpu.registers.set(Flag::Zero, false);
    cpu.registers.set(Flag::Subtract, false);
    cpu.registers.set(Flag::HalfCarry, half_carry);
    cpu.registers.set(Flag::Carry, carry);
    StepResult { inst: Instruction::AddSpImm8i { offset: a8 as i8 }, size: 2, cycles: Cycles::from(16), jump: false, taken: false }
}

// -- ADD HL, r16 ---------------------------------------------------------------

fn op_add_hl_r16(cpu: &mut Cpu, op: u8, _a8: u8, _a16: u16) -> StepResult {
    let src = match (op >> 4) & 0x3 {
        0 => Reg16::BC, 1 => Reg16::DE, 2 => Reg16::HL, _ => Reg16::SP,
    };
    cpu.add_hl(src);
    StepResult { inst: Instruction::AddHlReg16 { src }, size: 1, cycles: Cycles::from(8), jump: false, taken: false }
}

// -- INC / DEC r8 --------------------------------------------------------------

fn op_inc_r8(cpu: &mut Cpu, op: u8, _a8: u8, _a16: u16) -> StepResult {
    let idx = (op >> 3) & 0x7;
    let (inst, cycles) = if idx == 6 {
        cpu.inc(Arg::MemHl);
        (Instruction::Inc { dst: Arg::MemHl }, 12)
    } else {
        let r = REG8_MAP[idx.min(6) as usize];
        cpu.inc(Arg::Reg8(r));
        (Instruction::Inc { dst: Arg::Reg8(r) }, 4)
    };
    StepResult { inst, size: 1, cycles: Cycles::from(cycles), jump: false, taken: false }
}

fn op_dec_r8(cpu: &mut Cpu, op: u8, _a8: u8, _a16: u16) -> StepResult {
    let idx = (op >> 3) & 0x7;
    let (inst, cycles) = if idx == 6 {
        cpu.dec(Arg::MemHl);
        (Instruction::Dec { dst: Arg::MemHl }, 12)
    } else {
        let r = REG8_MAP[idx.min(6) as usize];
        cpu.dec(Arg::Reg8(r));
        (Instruction::Dec { dst: Arg::Reg8(r) }, 4)
    };
    StepResult { inst, size: 1, cycles: Cycles::from(cycles), jump: false, taken: false }
}

// -- INC / DEC r16 -------------------------------------------------------------

fn op_inc_r16(cpu: &mut Cpu, op: u8, _a8: u8, _a16: u16) -> StepResult {
    let reg = match (op >> 4) & 0x3 { 0 => Reg16::BC, 1 => Reg16::DE, 2 => Reg16::HL, _ => Reg16::SP };
    cpu.inc(Arg::Reg16(reg));
    StepResult { inst: Instruction::Inc { dst: Arg::Reg16(reg) }, size: 1, cycles: Cycles::from(8), jump: false, taken: false }
}

fn op_dec_r16(cpu: &mut Cpu, op: u8, _a8: u8, _a16: u16) -> StepResult {
    let reg = match (op >> 4) & 0x3 { 0 => Reg16::BC, 1 => Reg16::DE, 2 => Reg16::HL, _ => Reg16::SP };
    cpu.dec(Arg::Reg16(reg));
    StepResult { inst: Instruction::Dec { dst: Arg::Reg16(reg) }, size: 1, cycles: Cycles::from(8), jump: false, taken: false }
}

fn op_alu_r8(cpu: &mut Cpu, op: u8, _a8: u8, _a16: u16) -> StepResult {
    let idx = op & 0x7;
    let (src, cycles) = if idx == 6 {
        (Arg::MemHl, 8)
    } else {
        (Arg::Reg8(REG8_MAP[(if idx == 7 { 6 } else { idx }) as usize]), 4)
    };
    let kind = op & 0xF8;
    let (inst, size) = match kind {
        0x80 => { cpu.add(src); (Instruction::Add { src }, 1) }
        0x88 => { cpu.adc(src); (Instruction::Adc { src }, 1) }
        0x90 => { cpu.sub(src, true); (Instruction::Sub { src }, 1) }
        0x98 => { cpu.sbc(src); (Instruction::Sbc { src }, 1) }
        0xA0 => { cpu.logical(src, LogicalOp::And); (Instruction::And { src }, 1) }
        0xA8 => { cpu.logical(src, LogicalOp::Xor); (Instruction::Xor { src }, 1) }
        0xB0 => { cpu.logical(src, LogicalOp::Or); (Instruction::Or { src }, 1) }
        0xB8 => { cpu.sub(src, false); (Instruction::Cp { src }, 1) }
        _ => unreachable!(),
    };
    StepResult { inst, size, cycles: Cycles::from(cycles), jump: false, taken: false }
}

fn op_alu_imm(cpu: &mut Cpu, op: u8, a8: u8, _a16: u16) -> StepResult {
    let src = Arg::Imm8(a8);
    let (inst, size, cycles) = match op {
        0xC6 => { cpu.add(src); (Instruction::Add { src }, 2, 8) }
        0xCE => { cpu.adc(src); (Instruction::Adc { src }, 2, 8) }
        0xD6 => { cpu.sub(src, true); (Instruction::Sub { src }, 2, 8) }
        0xDE => { cpu.sbc(src); (Instruction::Sbc { src }, 2, 8) }
        0xE6 => { cpu.logical(src, LogicalOp::And); (Instruction::And { src }, 2, 8) }
        0xEE => { cpu.logical(src, LogicalOp::Xor); (Instruction::Xor { src }, 2, 8) }
        0xF6 => { cpu.logical(src, LogicalOp::Or); (Instruction::Or { src }, 2, 8) }
        0xFE => { cpu.sub(src, false); (Instruction::Cp { src }, 2, 8) }
        _ => unreachable!(),
    };
    StepResult { inst, size, cycles: Cycles::from(cycles), jump: false, taken: false }
}

// -- PUSH / POP ----------------------------------------------------------------

fn op_push(cpu: &mut Cpu, op: u8, _a8: u8, _a16: u16) -> StepResult {
    let reg = match (op >> 4) & 0x3 { 0 => Reg16::BC, 1 => Reg16::DE, 2 => Reg16::HL, _ => Reg16::AF };
    cpu.push(cpu.registers.read(reg));
    StepResult { inst: Instruction::Push { src: reg.into() }, size: 1, cycles: Cycles::from(16), jump: false, taken: false }
}

fn op_pop(cpu: &mut Cpu, op: u8, _a8: u8, _a16: u16) -> StepResult {
    let reg = match (op >> 4) & 0x3 { 0 => Reg16::BC, 1 => Reg16::DE, 2 => Reg16::HL, _ => Reg16::AF };
    let val = cpu.pop();
    cpu.registers.write(reg, val);
    StepResult { inst: Instruction::Pop { dst: reg.into() }, size: 1, cycles: Cycles::from(12), jump: false, taken: false }
}

// -- ROTATE (A variants) -------------------------------------------------------

fn op_rlca(cpu: &mut Cpu, _op: u8, _a8: u8, _a16: u16) -> StepResult {
    cpu.rotate(Arg::Reg8(Reg8::A), true, false, true);
    StepResult { inst: Instruction::Rlca, size: 1, cycles: Cycles::from(4), jump: false, taken: false }
}

fn op_rla(cpu: &mut Cpu, _op: u8, _a8: u8, _a16: u16) -> StepResult {
    cpu.rotate(Arg::Reg8(Reg8::A), true, true, true);
    StepResult { inst: Instruction::Rla, size: 1, cycles: Cycles::from(4), jump: false, taken: false }
}

fn op_rrca(cpu: &mut Cpu, _op: u8, _a8: u8, _a16: u16) -> StepResult {
    cpu.rotate(Arg::Reg8(Reg8::A), false, false, true);
    StepResult { inst: Instruction::Rrca, size: 1, cycles: Cycles::from(4), jump: false, taken: false }
}

fn op_rra(cpu: &mut Cpu, _op: u8, _a8: u8, _a16: u16) -> StepResult {
    cpu.rotate(Arg::Reg8(Reg8::A), false, true, true);
    StepResult { inst: Instruction::Rra, size: 1, cycles: Cycles::from(4), jump: false, taken: false }
}

// -- JP / JR / CALL / RET / RST ------------------------------------------------

fn op_jr(cpu: &mut Cpu, op: u8, a8: u8, _a16: u16) -> StepResult {
    let cond = match op { 0x18 => Cond::None, 0x20 => Cond::NotZero, 0x28 => Cond::Zero, 0x30 => Cond::NotCarry, _ => Cond::Carry };
    let ok = cond_matches(cpu, cond);
    let (taken, cyc) = if ok {
        let offset = a8 as i8 as u16;
        cpu.registers.PC = cpu.registers.PC.wrapping_add(2).wrapping_add(offset);
        (true, 12)
    } else {
        (false, 8)
    };
    StepResult { inst: Instruction::Jr { offset: a8 as i8, cond }, size: 2, cycles: Cycles(cyc, 8), jump: true, taken }
}

fn op_jp(cpu: &mut Cpu, op: u8, _a8: u8, a16: u16) -> StepResult {
    let cond = match op { 0xC3 => Cond::None, 0xC2 => Cond::NotZero, 0xCA => Cond::Zero, 0xD2 => Cond::NotCarry, _ => Cond::Carry };
    let ok = cond_matches(cpu, cond);
    let taken = if ok { cpu.registers.PC = a16; true } else { false };
    StepResult { inst: Instruction::Jp { addr: a16, cond }, size: 3, cycles: Cycles(16, 12), jump: true, taken }
}

fn op_call(cpu: &mut Cpu, op: u8, _a8: u8, a16: u16) -> StepResult {
    let cond = match op { 0xCD => Cond::None, 0xC4 => Cond::NotZero, 0xCC => Cond::Zero, 0xD4 => Cond::NotCarry, _ => Cond::Carry };
    let ok = cond_matches(cpu, cond);
    let taken = if ok {
        cpu.push(cpu.registers.PC + 3);
        cpu.registers.PC = a16;
        true
    } else {
        false
    };
    StepResult { inst: Instruction::Call { addr: a16, cond }, size: 3, cycles: Cycles(24, 12), jump: true, taken }
}

fn op_ret(cpu: &mut Cpu, op: u8, _a8: u8, _a16: u16) -> StepResult {
    let cond = match op { 0xC9 => Cond::None, 0xC0 => Cond::NotZero, 0xC8 => Cond::Zero, 0xD0 => Cond::NotCarry, _ => Cond::Carry };
    let ok = cond_matches(cpu, cond);
    let taken = if ok { cpu.registers.PC = cpu.pop(); true } else { false };
    StepResult { inst: Instruction::Ret { cond }, size: 1, cycles: Cycles(20, 8), jump: true, taken }
}

fn op_rst(cpu: &mut Cpu, op: u8, _a8: u8, _a16: u16) -> StepResult {
    let offset: u8 = match op { 0xC7 => 0x00, 0xCF => 0x08, 0xD7 => 0x10, 0xDF => 0x18, 0xE7 => 0x20, 0xEF => 0x28, 0xF7 => 0x30, _ => 0x38 };
    cpu.push(cpu.registers.PC + 1);
    cpu.registers.PC = offset as u16;
    StepResult { inst: Instruction::Rst { offset }, size: 1, cycles: Cycles::from(16), jump: true, taken: true }
}

#[inline]
fn cond_matches(cpu: &Cpu, cond: Cond) -> bool {
    match cond {
        Cond::None => true,
        Cond::NotZero => !cpu.registers.zero(),
        Cond::Zero => cpu.registers.zero(),
        Cond::NotCarry => !cpu.registers.carry(),
        Cond::Carry => cpu.registers.carry(),
    }
}

// -- Invalid opcode -----------------------------------------------------------

fn op_invalid(_cpu: &mut Cpu, op: u8, _a8: u8, _a16: u16) -> StepResult {
    panic!("Invalid instruction: 0x{:02X}", op);
}

// ---------------------------------------------------------------------------
// CB-prefix opcode handlers
// ---------------------------------------------------------------------------

fn cb_rot(cpu: &mut Cpu, op: u8, _a8: u8, _a16: u16, left: bool, through: bool) -> StepResult {
    let idx = op & 0x7;
    let (dst, cycles) = if idx == 6 {
        (Arg::MemHl, 16)
    } else {
        (Arg::Reg8(REG8_MAP[idx.min(6) as usize]), 8)
    };
    cpu.rotate(dst, left, through, false);
    // Build the right Instruction variant
    let inst = match (left, through) {
        (true, false) => Instruction::Rlc { dst },
        (true, true)  => Instruction::Rl  { dst },
        (false, false) => Instruction::Rrc { dst },
        (false, true)  => Instruction::Rr  { dst },
    };
    StepResult { inst, size: 2, cycles: Cycles::from(cycles), jump: false, taken: false }
}

fn cb_rlc(cpu: &mut Cpu, op: u8, a8: u8, a16: u16) -> StepResult { cb_rot(cpu, op, a8, a16, true, false) }
fn cb_rl(cpu: &mut Cpu, op: u8, a8: u8, a16: u16) -> StepResult { cb_rot(cpu, op, a8, a16, true, true) }
fn cb_rrc(cpu: &mut Cpu, op: u8, a8: u8, a16: u16) -> StepResult { cb_rot(cpu, op, a8, a16, false, false) }
fn cb_rr(cpu: &mut Cpu, op: u8, a8: u8, a16: u16) -> StepResult { cb_rot(cpu, op, a8, a16, false, true) }

fn cb_shift(cpu: &mut Cpu, op: u8, _a8: u8, _a16: u16, left: bool, arithmetic: bool) -> StepResult {
    let idx = op & 0x7;
    let (dst, cycles) = if idx == 6 {
        (Arg::MemHl, 16)
    } else {
        (Arg::Reg8(REG8_MAP[idx.min(6) as usize]), 8)
    };
    cpu.shift(dst, left, arithmetic);
    let inst = match (left, arithmetic) {
        (true, _)     => Instruction::Sla { dst },
        (false, true) => Instruction::Sra { dst },
        (false, false)=> Instruction::Srl { dst },
    };
    StepResult { inst, size: 2, cycles: Cycles::from(cycles), jump: false, taken: false }
}

fn cb_sla(cpu: &mut Cpu, op: u8, a8: u8, a16: u16) -> StepResult { cb_shift(cpu, op, a8, a16, true, false) }
fn cb_sra(cpu: &mut Cpu, op: u8, a8: u8, a16: u16) -> StepResult { cb_shift(cpu, op, a8, a16, false, true) }
fn cb_srl(cpu: &mut Cpu, op: u8, a8: u8, a16: u16) -> StepResult { cb_shift(cpu, op, a8, a16, false, false) }

fn cb_swap(cpu: &mut Cpu, op: u8, _a8: u8, _a16: u16) -> StepResult {
    let idx = op & 0x7;
    let (dst, cycles, result) = if idx == 6 {
        let addr = cpu.registers.read(Reg16::HL);
        let val = cpu.memory.read(addr);
        let res = (val << 4) | (val >> 4);
        cpu.memory.write(addr, res);
        (Arg::MemHl, 16, res)
    } else {
        let r = REG8_MAP[idx.min(6) as usize];
        let val = cpu.registers.read(r);
        let res = (val << 4) | (val >> 4);
        cpu.registers.write(r, res);
        (Arg::Reg8(r), 8, res)
    };
    cpu.registers.set(Flag::Zero, result == 0);
    cpu.registers.set(Flag::Subtract, false);
    cpu.registers.set(Flag::HalfCarry, false);
    cpu.registers.set(Flag::Carry, false);
    StepResult { inst: Instruction::Swap { dst }, size: 2, cycles: Cycles::from(cycles), jump: false, taken: false }
}

fn cb_bit(cpu: &mut Cpu, op: u8, _a8: u8, _a16: u16) -> StepResult {
    let idx = op & 0x7;
    let bit = ((op >> 3) & 0x7) as u8;
    let (dst, cycles, value) = if idx == 6 {
        let addr = cpu.registers.read(Reg16::HL);
        let val = cpu.memory.read(addr);
        (Arg::MemHl, 12, val)
    } else {
        let r = REG8_MAP[idx.min(6) as usize];
        let val = cpu.registers.read(r);
        (Arg::Reg8(r), 8, val)
    };
    cpu.registers.set(Flag::Zero, value & (1 << bit) == 0);
    cpu.registers.set(Flag::Subtract, false);
    cpu.registers.set(Flag::HalfCarry, true);
    StepResult { inst: Instruction::Bit { dst, bit }, size: 2, cycles: Cycles::from(cycles), jump: false, taken: false }
}

fn cb_res_set(cpu: &mut Cpu, op: u8, _a8: u8, _a16: u16, set: bool) -> StepResult {
    let idx = op & 0x7;
    let bit = ((op >> 3) & 0x7) as u8;
    let (dst, cycles) = if idx == 6 { (Arg::MemHl, 16) } else { (Arg::Reg8(REG8_MAP[idx.min(6) as usize]), 8) };
    cpu.set(dst, bit, !set);
    let inst = if set { Instruction::Set { dst, bit } } else { Instruction::Res { dst, bit } };
    StepResult { inst, size: 2, cycles: Cycles::from(cycles), jump: false, taken: false }
}

fn cb_res(cpu: &mut Cpu, op: u8, a8: u8, a16: u16) -> StepResult { cb_res_set(cpu, op, a8, a16, false) }
fn cb_set(cpu: &mut Cpu, op: u8, a8: u8, a16: u16) -> StepResult { cb_res_set(cpu, op, a8, a16, true) }

// ---------------------------------------------------------------------------
// Dispatch tables
// ---------------------------------------------------------------------------

#[rustfmt::skip]
static OP_TABLE: [OpHandler; 256] = [
    // 0x0X
    op_nop,         // 0x00
    op_ld_r16_imm16,// 0x01
    op_ld_mem_r16_a,// 0x02
    op_inc_r16,     // 0x03
    op_inc_r8,      // 0x04
    op_dec_r8,      // 0x05
    op_ld_r8_imm8,  // 0x06
    op_rlca,        // 0x07
    op_ld_memimm_sp,// 0x08
    op_add_hl_r16,  // 0x09
    op_ld_a_mem_r16,// 0x0A
    op_dec_r16,     // 0x0B
    op_inc_r8,      // 0x0C
    op_dec_r8,      // 0x0D
    op_ld_r8_imm8,  // 0x0E
    op_rrca,        // 0x0F
    // 0x1X
    op_stop,        // 0x10
    op_ld_r16_imm16,// 0x11
    op_ld_mem_r16_a,// 0x12
    op_inc_r16,     // 0x13
    op_inc_r8,      // 0x14
    op_dec_r8,      // 0x15
    op_ld_r8_imm8,  // 0x16
    op_rla,         // 0x17
    op_jr,          // 0x18
    op_add_hl_r16,  // 0x19
    op_ld_a_mem_r16,// 0x1A
    op_dec_r16,     // 0x1B
    op_inc_r8,      // 0x1C
    op_dec_r8,      // 0x1D
    op_ld_r8_imm8,  // 0x1E
    op_rra,         // 0x1F
    // 0x2X
    op_jr,          // 0x20
    op_ld_r16_imm16,// 0x21
    op_ldi_memhl_a, // 0x22
    op_inc_r16,     // 0x23
    op_inc_r8,      // 0x24
    op_dec_r8,      // 0x25
    op_ld_r8_imm8,  // 0x26
    op_daa,         // 0x27
    op_jr,          // 0x28
    op_add_hl_r16,  // 0x29
    op_ldi_a_memhl, // 0x2A
    op_dec_r16,     // 0x2B
    op_inc_r8,      // 0x2C
    op_dec_r8,      // 0x2D
    op_ld_r8_imm8,  // 0x2E
    op_cpl,         // 0x2F
    // 0x3X
    op_jr,          // 0x30
    op_ld_r16_imm16,// 0x31
    op_ldd_memhl_a, // 0x32
    op_inc_r16,     // 0x33
    op_inc_r8,      // 0x34   (HL)
    op_dec_r8,      // 0x35   (HL)
    op_ld_memhl_imm8,// 0x36
    op_scf,         // 0x37
    op_jr,          // 0x38
    op_add_hl_r16,  // 0x39
    op_ldd_a_memhl, // 0x3A
    op_dec_r16,     // 0x3B
    op_inc_r8,      // 0x3C
    op_dec_r8,      // 0x3D
    op_ld_r8_imm8,  // 0x3E
    op_ccf,         // 0x3F
    // 0x4X – LD r8,r8 range (B,C,D,E,H,L,(HL),A as dst)
    op_ld_r8_r8, op_ld_r8_r8, op_ld_r8_r8, op_ld_r8_r8, op_ld_r8_r8, op_ld_r8_r8, op_ld_r8_r8, op_ld_r8_r8,
    op_ld_r8_r8, op_ld_r8_r8, op_ld_r8_r8, op_ld_r8_r8, op_ld_r8_r8, op_ld_r8_r8, op_ld_r8_r8, op_ld_r8_r8,
    op_ld_r8_r8, op_ld_r8_r8, op_ld_r8_r8, op_ld_r8_r8, op_ld_r8_r8, op_ld_r8_r8, op_ld_r8_r8, op_ld_r8_r8,
    op_ld_r8_r8, op_ld_r8_r8, op_ld_r8_r8, op_ld_r8_r8, op_ld_r8_r8, op_ld_r8_r8, op_ld_r8_r8, op_ld_r8_r8,
    op_ld_r8_r8, op_ld_r8_r8, op_ld_r8_r8, op_ld_r8_r8, op_ld_r8_r8, op_ld_r8_r8, op_ld_r8_r8, op_ld_r8_r8,
    op_ld_r8_r8, op_ld_r8_r8, op_ld_r8_r8, op_ld_r8_r8, op_ld_r8_r8, op_ld_r8_r8, op_ld_r8_r8, op_ld_r8_r8,
    op_ld_r8_r8, op_ld_r8_r8, op_ld_r8_r8, op_ld_r8_r8, op_ld_r8_r8, op_ld_r8_r8, // 0x75
    op_halt,         // 0x76 – HALT (not LD)
    op_ld_r8_r8, op_ld_r8_r8, op_ld_r8_r8, op_ld_r8_r8, op_ld_r8_r8, op_ld_r8_r8, op_ld_r8_r8, op_ld_r8_r8,
    op_ld_r8_r8,    // 0x7F
    // 0x8X – ALU r8
    op_alu_r8, op_alu_r8, op_alu_r8, op_alu_r8, op_alu_r8, op_alu_r8, op_alu_r8, op_alu_r8,
    op_alu_r8, op_alu_r8, op_alu_r8, op_alu_r8, op_alu_r8, op_alu_r8, op_alu_r8, op_alu_r8,
    op_alu_r8, op_alu_r8, op_alu_r8, op_alu_r8, op_alu_r8, op_alu_r8, op_alu_r8, op_alu_r8,
    op_alu_r8, op_alu_r8, op_alu_r8, op_alu_r8, op_alu_r8, op_alu_r8, op_alu_r8, op_alu_r8,
    op_alu_r8, op_alu_r8, op_alu_r8, op_alu_r8, op_alu_r8, op_alu_r8, op_alu_r8, op_alu_r8,
    op_alu_r8, op_alu_r8, op_alu_r8, op_alu_r8, op_alu_r8, op_alu_r8, op_alu_r8, op_alu_r8,
    op_alu_r8, op_alu_r8, op_alu_r8, op_alu_r8, op_alu_r8, op_alu_r8, op_alu_r8, op_alu_r8,
    op_alu_r8, op_alu_r8, op_alu_r8, op_alu_r8, op_alu_r8, op_alu_r8, op_alu_r8, op_alu_r8,
    // 0xC0 – 0xFF
    op_ret,         // 0xC0
    op_pop,         // 0xC1
    op_jp,          // 0xC2
    op_jp,          // 0xC3
    op_call,        // 0xC4
    op_push,        // 0xC5
    op_alu_imm,     // 0xC6  ADD A, imm8
    op_rst,         // 0xC7
    op_ret,         // 0xC8
    op_ret,         // 0xC9
    op_jp,          // 0xCA
    op_alu_imm,     // 0xCB – handled specially in step(), but table entry unused
    op_call,        // 0xCC
    op_call,        // 0xCD
    op_alu_imm,     // 0xCE  ADC A, imm8
    op_rst,         // 0xCF
    op_ret,         // 0xD0
    op_pop,         // 0xD1
    op_jp,          // 0xD2
    op_invalid,     // 0xD3
    op_call,        // 0xD4
    op_push,        // 0xD5
    op_alu_imm,     // 0xD6  SUB A, imm8
    op_rst,         // 0xD7
    op_ret,         // 0xD8
    op_reti,        // 0xD9
    op_jp,          // 0xDA
    op_invalid,     // 0xDB
    op_call,        // 0xDC
    op_invalid,     // 0xDD
    op_alu_imm,     // 0xDE  SBC A, imm8
    op_rst,         // 0xDF
    op_ldh,         // 0xE0
    op_pop,         // 0xE1
    op_ld_memc_a,   // 0xE2
    op_invalid,     // 0xE3
    op_invalid,     // 0xE4
    op_push,        // 0xE5
    op_alu_imm,     // 0xE6  AND A, imm8
    op_rst,         // 0xE7
    op_add_sp_imm8, // 0xE8
    op_jphl,        // 0xE9
    op_ld_memimm_a, // 0xEA
    op_invalid,     // 0xEB
    op_invalid,     // 0xEC
    op_invalid,     // 0xED
    op_alu_imm,     // 0xEE  XOR A, imm8
    op_rst,         // 0xEF
    op_ldh_a,       // 0xF0
    op_pop,         // 0xF1
    op_ld_a_memc,   // 0xF2
    op_di,          // 0xF3
    op_invalid,     // 0xF4
    op_push,        // 0xF5
    op_alu_imm,     // 0xF6  OR A, imm8
    op_rst,         // 0xF7
    op_ld_hl_sp_imm8,// 0xF8
    op_ld_sp_hl,    // 0xF9
    op_ld_a_memimm, // 0xFA
    op_ei,          // 0xFB
    op_invalid,     // 0xFC
    op_invalid,     // 0xFD
    op_alu_imm,     // 0xFE  CP A, imm8
    op_rst,         // 0xFF
];

#[rustfmt::skip]
static CB_TABLE: [OpHandler; 256] = [
    // 0x0X – RLC
    cb_rlc, cb_rlc, cb_rlc, cb_rlc, cb_rlc, cb_rlc, cb_rlc, cb_rlc,
    // 0x0X – RRC
    cb_rrc, cb_rrc, cb_rrc, cb_rrc, cb_rrc, cb_rrc, cb_rrc, cb_rrc,
    // 0x1X – RL
    cb_rl,  cb_rl,  cb_rl,  cb_rl,  cb_rl,  cb_rl,  cb_rl,  cb_rl,
    // 0x1X – RR
    cb_rr,  cb_rr,  cb_rr,  cb_rr,  cb_rr,  cb_rr,  cb_rr,  cb_rr,
    // 0x2X – SLA
    cb_sla, cb_sla, cb_sla, cb_sla, cb_sla, cb_sla, cb_sla, cb_sla,
    // 0x2X – SRA
    cb_sra, cb_sra, cb_sra, cb_sra, cb_sra, cb_sra, cb_sra, cb_sra,
    // 0x3X – SWAP
    cb_swap,cb_swap,cb_swap,cb_swap,cb_swap,cb_swap,cb_swap,cb_swap,
    // 0x3X – SRL
    cb_srl, cb_srl, cb_srl, cb_srl, cb_srl, cb_srl, cb_srl, cb_srl,
    // 0x4X – 0x7X  BIT b, r8
    cb_bit, cb_bit, cb_bit, cb_bit, cb_bit, cb_bit, cb_bit, cb_bit,
    cb_bit, cb_bit, cb_bit, cb_bit, cb_bit, cb_bit, cb_bit, cb_bit,
    cb_bit, cb_bit, cb_bit, cb_bit, cb_bit, cb_bit, cb_bit, cb_bit,
    cb_bit, cb_bit, cb_bit, cb_bit, cb_bit, cb_bit, cb_bit, cb_bit,
    cb_bit, cb_bit, cb_bit, cb_bit, cb_bit, cb_bit, cb_bit, cb_bit,
    cb_bit, cb_bit, cb_bit, cb_bit, cb_bit, cb_bit, cb_bit, cb_bit,
    cb_bit, cb_bit, cb_bit, cb_bit, cb_bit, cb_bit, cb_bit, cb_bit,
    cb_bit, cb_bit, cb_bit, cb_bit, cb_bit, cb_bit, cb_bit, cb_bit,
    // 0x8X – 0xBX  RES b, r8
    cb_res, cb_res, cb_res, cb_res, cb_res, cb_res, cb_res, cb_res,
    cb_res, cb_res, cb_res, cb_res, cb_res, cb_res, cb_res, cb_res,
    cb_res, cb_res, cb_res, cb_res, cb_res, cb_res, cb_res, cb_res,
    cb_res, cb_res, cb_res, cb_res, cb_res, cb_res, cb_res, cb_res,
    cb_res, cb_res, cb_res, cb_res, cb_res, cb_res, cb_res, cb_res,
    cb_res, cb_res, cb_res, cb_res, cb_res, cb_res, cb_res, cb_res,
    cb_res, cb_res, cb_res, cb_res, cb_res, cb_res, cb_res, cb_res,
    cb_res, cb_res, cb_res, cb_res, cb_res, cb_res, cb_res, cb_res,
    // 0xCX – 0xFX  SET b, r8
    cb_set, cb_set, cb_set, cb_set, cb_set, cb_set, cb_set, cb_set,
    cb_set, cb_set, cb_set, cb_set, cb_set, cb_set, cb_set, cb_set,
    cb_set, cb_set, cb_set, cb_set, cb_set, cb_set, cb_set, cb_set,
    cb_set, cb_set, cb_set, cb_set, cb_set, cb_set, cb_set, cb_set,
    cb_set, cb_set, cb_set, cb_set, cb_set, cb_set, cb_set, cb_set,
    cb_set, cb_set, cb_set, cb_set, cb_set, cb_set, cb_set, cb_set,
    cb_set, cb_set, cb_set, cb_set, cb_set, cb_set, cb_set, cb_set,
    cb_set, cb_set, cb_set, cb_set, cb_set, cb_set, cb_set, cb_set,
];

#[derive(Clone, Copy)]
#[repr(u8)]
#[allow(dead_code)]
pub enum Interrupt {
    Vblank = 0,
    LcdStat,
    Timer,
    Serial,
    Joypad,
}

/// Types of logical operations
///
/// This allows us to use a single method for all supported
/// logical operations.
#[derive(Clone, Copy)]
enum LogicalOp {
    And,
    Or,
    Xor,
}

/// Small trait that abstracts computation of half-carry
/// between two numbers
trait HalfCarry<T> {
    /// Half carry for addition
    fn half_carry(&self, other: T) -> bool;

    /// Half carry for subtraction
    fn half_carry_sub(&self, other: T) -> bool;
}

impl HalfCarry<u8> for u8 {
    fn half_carry(&self, other: u8) -> bool {
        ((self & 0xF) + (other & 0xF)) & 0x10 != 0
    }

    fn half_carry_sub(&self, other: u8) -> bool {
        (self & 0xF) < (other & 0xF)
    }
}

impl HalfCarry<u16> for u16 {
    fn half_carry(&self, other: u16) -> bool {
        ((self & 0x0FFF) + (other & 0x0FFF)) & 0x1000 != 0
    }

    fn half_carry_sub(&self, other: u16) -> bool {
        (self & 0x00FF) < (other & 0x00FF)
    }
}

pub struct Cpu {
    pub registers: RegisterFile,
    pub memory: MemoryBus,
    dma: DmaController,
    pub cgb: bool,

    pub halted: bool,
    pub stopped: bool,
    pub speed: bool,

    /// Global interrupt enable flag (Interrupt Master Enable)
    pub ime: bool,

    /// Trace all instructions executed to a file
    trace: Option<BufWriter<File>>,
}

impl Cpu {
    /// Base CPU frequency, in Hz
    pub const BASE_FREQ: u32 = 4_194_304;

    /// CPU cycle time, in ns
    pub const CYCLE_TIME: u32 = ((1.0 / Self::BASE_FREQ as f64) * 1e9) as u32;

    /// Create an empty CPU without a cartridge
    ///
    /// Mainly used for tests
    #[allow(dead_code)]
    pub fn new(cgb: bool) -> Self {
        let memory = MemoryBus::new(cgb);
        let registers = RegisterFile::new(cgb);

        Self {
            registers,
            memory,
            dma: DmaController::new(cgb),
            cgb,
            ime: false,
            halted: false,
            stopped: false,
            speed: false,
            trace: None,
        }
    }

    /// Create a CPU from a cartridge
    pub fn from_cartridge(cartridge: Cartridge, trace: bool) -> Result<Self> {
        let cgb = cartridge.cgb();
        let boot_rom = cartridge.boot_rom;
        let memory = MemoryBus::from_cartridge(cartridge)?;

        let registers = if boot_rom {
            // If boot ROM is required, keep registers empty
            RegisterFile::empty()
        } else {
            // Otherwise, init registers based on mode
            RegisterFile::new(cgb)
        };

        let dma = DmaController::new(cgb);

        // If tracing is enabled, create a trace file in the current directory
        let trace = if trace {
            let f = File::create("gbc.trace")?;
            Some(BufWriter::new(f))
        } else {
            None
        };

        Ok(Self {
            registers,
            memory,
            dma,
            cgb,
            ime: false,
            halted: false,
            stopped: false,
            speed: false,
            trace,
        })
    }

    /// Current clock cycle duration, in ns. This value is based
    /// on the current value in the speed I/O register.
    #[inline]
    pub fn cycle_time(speed: bool) -> u32 {
        if speed {
            Self::CYCLE_TIME / 2
        } else {
            Self::CYCLE_TIME
        }
    }

    /// Reset this CPU to initial state
    ///
    /// This involves resetting memory, the ROM controller, and the PPU
    pub fn reset(&mut self) {
        self.registers = RegisterFile::new(self.cgb);
        self.memory.reset();
        self.dma = DmaController::new(self.cgb);
        self.ime = false;
        self.halted = false;
        self.stopped = false;
        self.speed = false;
    }

    /// Executes the next instruction and returns the number of cycles it
    /// took to complete.
    pub fn step(&mut self) -> (u16, Instruction) {
        let int_cycles = self.service_interrupts();
        if self.halted {
            return (4, Instruction::Nop);
        }

        let pc = self.registers.PC;
        let opcode = self.memory.read(pc);
        let arg8 = self.memory.read(pc + 1);
        let arg16 = u16::from_le_bytes([arg8, self.memory.read(pc + 2)]);

        let result = if opcode == 0xCB {
            CB_TABLE[arg8 as usize](self, arg8, arg8, arg16)
        } else {
            OP_TABLE[opcode as usize](self, opcode, arg8, arg16)
        };

        if let Some(_) = &mut self.trace {
            self.trace(&result.inst);
        }

        let mut cycles = if !result.jump || result.jump && !result.taken {
            self.registers.PC += result.size as u16;
            result.cycles.not_taken() as u16
        } else {
            result.cycles.taken() as u16
        };

        if self.stopped {
            cycles += 8200;
        } else {
            cycles += int_cycles as u16;
            cycles += self.dma(cycles);
        }

        if self.speed {
            cycles /= 2;
        }

        (cycles, result.inst)
    }

    /// Switch CPU speed
    fn speed_switch(&mut self) {
        if !self.speed {
            // Normal to double speed
            self.memory.io_mut().prep_speed_switch = 1 << 7;
            self.speed = true;
        } else {
            // Double to normal speed
            self.memory.io_mut().prep_speed_switch = 0;
            self.speed = false;
        }

        self.stopped = true;
    }

    #[inline]
    fn trace(&mut self, inst: &Instruction) {
        // Figure out the currently active ROM bank based on the memory region of PC
        let pc = self.registers.PC;
        let (memory_type, bank) = self.memory.memory_info(pc);
        let f = &mut self.trace.as_mut().unwrap();

        // First, write the register state prior to executing this instruction
        write!(f, "{}\n\n", self.registers).unwrap();

        // Then write the instruction
        write!(f, "{:03}:{}:{:#06X} - {}\n\n", bank, memory_type, pc, inst).unwrap();

        f.flush().unwrap();
    }

    /// Execute a single step of DMA (if active).
    fn dma(&mut self, cycles: u16) -> u16 {
        let memory = &mut self.memory;
        self.dma.step(cycles, self.speed, memory)
    }

    /// Fetch the next instruction and return it
    pub fn fetch(&self, addr: Option<u16>) -> (Instruction, u8, Cycles) {
        let addr = addr.unwrap_or(self.registers.PC);

        // Read the next 3 bytes from memory, starting from PC.
        // This is what we will use to decode the next instruction.
        //
        // TODO: Evaluate the boundary cases
        let data: [u8; 3] = [
            self.memory.read(addr),
            self.memory.read(addr + 1),
            self.memory.read(addr + 2),
        ];

        // Decode the instruction
        Instruction::decode(data)
    }

    /// Disassemble the next `count` instructions, starting at the given address.
    #[allow(dead_code)]
    pub fn disassemble(&self, count: usize, addr: Option<u16>) -> Vec<(Instruction, u16)> {
        let mut addr = addr.unwrap_or(self.registers.PC);
        let mut result = Vec::new();

        for _ in 0..count {
            let (inst, size, _) = self.fetch(Some(addr));
            result.push((inst, addr));
            addr = addr.wrapping_add(size as u16);
        }

        result
    }

    /// Figure out which interrupts are pending and service the one with the
    /// highest priority.
    ///
    /// Servicing an interrupt takes 20 clock cycles according to Pandocs.
    ///
    /// See: pg. 27 of GB Programming Manual
    fn service_interrupts(&mut self) -> u8 {
        let int_enable = self.memory.read(0xFFFF);
        let int_flags = self.memory.read(0xFF0F);

        // If no interrupts are pending, bail out now
        if int_enable & int_flags == 0 {
            return 0;
        }

        // If the CPU is currently halted and there is a pending interrupt,
        // leave HALT state, *even if IME is disabled*.
        if self.halted {
            self.halted = (int_enable & int_flags) == 0;
        }

        // If the IME is disabled, do not process any interrupts
        if !self.ime {
            return 0;
        }

        // Iterate over each interrupt in priority order and service
        // the first one
        for int in 0..5 {
            let enabled = (int_enable & 1 << int) != 0;
            let pending = (int_flags & 1 << int) != 0;

            if enabled && pending {
                // Disable interrupts
                self.ime = false;

                // Clear the pending flag
                self.memory.write(0xFF0F, int_flags & !(1 << int));

                // Push current PC to the stack
                self.push(self.registers.PC);

                // Compute the ISR address to jump to
                let isr = (int << 3) + 0x40;

                self.registers.PC = isr;

                break;
            }
        }

        20
    }

    /// Trigger a particular interrupt
    #[inline]
    pub fn trigger_interrupt(&mut self, interrupt: Interrupt) {
        let bit = interrupt as u8;
        let int_flags = self.memory.read(0xFF0F);
        self.memory.write(0xFF0F, int_flags | 1 << bit);
    }

    /// Handle rotate instructions
    ///
    /// # Arguments
    ///
    /// * `left`: If true, value is rotated left
    /// * `through`: If true, value is rotated through the carry bit
    /// * `a`: If true, A variant of the instruction is used
    fn rotate(&mut self, dst: Arg, left: bool, through: bool, a: bool) {
        let curr = match dst {
            Arg::Reg8(dst) => self.registers.read(dst),
            Arg::MemHl => {
                let addr = self.registers.read(Reg16::HL);
                self.memory.read(addr)
            }
            _ => unreachable!("Unexpected dst: {}", dst),
        };

        let prev_carry = self.registers.carry();
        let carry;
        let mut value;

        if left {
            carry = curr & (1 << 7) != 0;
            value = curr.rotate_left(1);

            if through {
                // Set bit 0 to old carry
                if prev_carry {
                    value |= 1 << 0;
                } else {
                    value &= !(1 << 0);
                }
            }
        } else {
            carry = curr & (1 << 0) != 0;
            value = curr.rotate_right(1);

            if through {
                if prev_carry {
                    value |= 1 << 7;
                } else {
                    value &= !(1 << 7);
                }
            }
        }

        // Write back the result
        match dst {
            Arg::Reg8(dst) => self.registers.write(dst, value),
            Arg::MemHl => {
                let addr = self.registers.read(Reg16::HL);
                self.memory.write(addr, value);
            }
            _ => unreachable!("Unexpected dst: {}", dst),
        }

        let zero_flag = if a { false } else { value == 0 };

        // Flags
        self.registers.set(Flag::Zero, zero_flag);
        self.registers.set(Flag::Subtract, false);
        self.registers.set(Flag::HalfCarry, false);
        self.registers.set(Flag::Carry, carry);
    }

    /// Handle shift instructions
    fn shift(&mut self, dst: Arg, left: bool, arithmetic: bool) {
        let curr = match dst {
            Arg::Reg8(dst) => self.registers.read(dst),
            Arg::MemHl => {
                let addr = self.registers.read(Reg16::HL);
                self.memory.read(addr)
            }
            _ => unreachable!("Unexpected dst: {}", dst),
        };

        let carry;
        let value;

        if left {
            carry = curr & (1 << 7) != 0;
            value = curr.wrapping_shl(1);
        } else {
            carry = curr & (1 << 0) != 0;

            if arithmetic {
                // In case of arithmetic right shift, cast to i8 before shifting,
                // then cast back. This takes care of the MSB.
                value = (curr as i8).wrapping_shr(1) as u8;
            } else {
                value = curr.wrapping_shr(1);
            }
        }

        // Write back the result
        match dst {
            Arg::Reg8(dst) => self.registers.write(dst, value),
            Arg::MemHl => {
                let addr = self.registers.read(Reg16::HL);
                self.memory.write(addr, value);
            }
            _ => unreachable!("Unexpected dst: {}", dst),
        }

        // Flags
        self.registers.set(Flag::Zero, value == 0);
        self.registers.set(Flag::Subtract, false);
        self.registers.set(Flag::HalfCarry, false);
        self.registers.set(Flag::Carry, carry);
    }

    /// Helper that pops 2 bytes off the stack
    fn pop(&mut self) -> u16 {
        // Read upper and lower bytes from stack.
        let lower = self.memory.read(self.registers.SP);
        let upper = self.memory.read(self.registers.SP.wrapping_add(1));
        let value = (upper as u16) << 8 | lower as u16;

        // Increment SP
        self.registers.SP = self.registers.SP.wrapping_add(2);

        value
    }

    /// Helper that pushes 2 bytes to the stack
    fn push(&mut self, value: u16) {
        let lower = value as u8;
        let upper = (value >> 8) as u8;

        // Write upper and lower bytes seperately to the stack.
        // We cannot use the `MemoryWrite` trait because it assumes
        // that memory addresses increase instead of decrease.
        self.memory.write(self.registers.SP.wrapping_sub(1), upper);
        self.memory.write(self.registers.SP.wrapping_sub(2), lower);

        // Decrement SP
        self.registers.SP = self.registers.SP.wrapping_sub(2);
    }

    fn add(&mut self, src: Arg) {
        let a = self.registers.read(Reg8::A);

        let val = match src {
            Arg::Reg8(src) => self.registers.read(src),
            Arg::Imm8(src) => src,
            Arg::MemHl => {
                let addr = self.registers.read(Reg16::HL);
                let val = self.memory.read(addr);
                val
            }
            _ => unreachable!("Unexpected src: {}", src),
        };

        let half_carry = a.half_carry(val);
        let (result, carry) = a.overflowing_add(val);

        self.registers.write(Reg8::A, result);

        self.registers.set(Flag::Zero, result == 0);
        self.registers.set(Flag::Subtract, false);
        self.registers.set(Flag::HalfCarry, half_carry);
        self.registers.set(Flag::Carry, carry);
    }

    /// ADD with carry
    fn adc(&mut self, src: Arg) {
        let a = self.registers.read(Reg8::A);

        let val = match src {
            Arg::Reg8(src) => self.registers.read(src),
            Arg::Imm8(src) => src,
            Arg::MemHl => {
                let addr = self.registers.read(Reg16::HL);
                let val = self.memory.read(addr);
                val
            }
            _ => unreachable!("Unexpected src: {}", src),
        };

        let curr_carry = if self.registers.carry() { 1u8 } else { 0u8 };

        // First, add the current carry to A and compute initial carry flags
        let tmp = a.wrapping_add(curr_carry);
        let mut carry = tmp < a;
        let mut half_carry = a.half_carry(curr_carry);

        // Then, add the actual value to A and update the carry flags (if needed)
        let result = tmp.wrapping_add(val);
        carry = carry || result < tmp;
        half_carry = half_carry || tmp.half_carry(val);

        self.registers.write(Reg8::A, result);

        self.registers.set(Flag::Zero, result == 0);
        self.registers.set(Flag::Subtract, false);
        self.registers.set(Flag::HalfCarry, half_carry);
        self.registers.set(Flag::Carry, carry);
    }

    /// 16-bit version of ADD for HL
    fn add_hl(&mut self, src: Reg16) {
        let hl = self.registers.read(Reg16::HL);
        let half_carry: bool;

        let (result, carry) = {
            let val = self.registers.read(src);
            half_carry = hl.half_carry(val);
            hl.overflowing_add(val)
        };

        self.registers.write(Reg16::HL, result);

        self.registers.set(Flag::Subtract, false);
        self.registers.set(Flag::HalfCarry, half_carry);
        self.registers.set(Flag::Carry, carry);
    }

    fn sub(&mut self, src: Arg, write: bool) {
        let a = self.registers.read(Reg8::A);

        let val = match src {
            Arg::Reg8(src) => self.registers.read(src),
            Arg::Imm8(src) => src,
            Arg::MemHl => {
                let addr = self.registers.read(Reg16::HL);
                let val = self.memory.read(addr);
                val
            }
            _ => unreachable!("Unexpected src: {}", src),
        };

        let half_carry = a.half_carry_sub(val);
        let (result, carry) = a.overflowing_sub(val);

        // Write back the result for non-CP
        if write {
            self.registers.write(Reg8::A, result);
        }

        self.registers.set(Flag::Zero, result == 0);
        self.registers.set(Flag::Subtract, true);
        self.registers.set(Flag::HalfCarry, half_carry);
        self.registers.set(Flag::Carry, carry);
    }

    /// SUB with carry
    fn sbc(&mut self, src: Arg) {
        let a = self.registers.read(Reg8::A);

        let val = match src {
            Arg::Reg8(src) => self.registers.read(src),
            Arg::Imm8(src) => src,
            Arg::MemHl => {
                let addr = self.registers.read(Reg16::HL);
                let val = self.memory.read(addr);
                val
            }
            _ => unreachable!("Unexpected src: {}", src),
        };

        let curr_carry = if self.registers.carry() { 1u8 } else { 0u8 };

        // First, subtract the current carry from A and compute initial carry flags
        let tmp = a.wrapping_sub(curr_carry);
        let mut carry = tmp > a;
        let mut half_carry = a.half_carry_sub(curr_carry);

        // Then, subtract the actual value from A and update the carry flags (if needed)
        let result = tmp.wrapping_sub(val);
        carry = carry || result > tmp;
        half_carry = half_carry || tmp.half_carry_sub(val);

        self.registers.write(Reg8::A, result);

        self.registers.set(Flag::Zero, result == 0);
        self.registers.set(Flag::Subtract, true);
        self.registers.set(Flag::HalfCarry, half_carry);
        self.registers.set(Flag::Carry, carry);
    }

    fn logical(&mut self, src: Arg, op: LogicalOp) {
        let a = self.registers.read(Reg8::A);

        let val = match src {
            Arg::Reg8(src) => self.registers.read(src),
            Arg::Imm8(src) => src,
            Arg::MemHl => {
                let addr = self.registers.read(Reg16::HL);
                let val = self.memory.read(addr);
                val
            }
            _ => unreachable!("Unexpected src: {}", src),
        };

        let result = match op {
            LogicalOp::And => a & val,
            LogicalOp::Or => a | val,
            LogicalOp::Xor => a ^ val,
        };

        self.registers.write(Reg8::A, result);

        self.registers.set(Flag::Zero, result == 0);
        self.registers.set(Flag::Subtract, false);
        self.registers.set(Flag::Carry, false);

        if let LogicalOp::And = op {
            self.registers.set(Flag::HalfCarry, true);
        } else {
            self.registers.set(Flag::HalfCarry, false);
        }
    }

    /// Increment instruction
    fn inc(&mut self, dst: Arg) {
        let mut update_flags = true;
        let mut half_carry = false;

        let result = match dst {
            Arg::Reg8(dst) => {
                let curr = self.registers.read(dst);
                let result = curr.wrapping_add(1);
                half_carry = curr.half_carry(1);
                self.registers.write(dst, result);
                result as u16
            }
            Arg::Reg16(dst) => {
                let curr = self.registers.read(dst);
                let result = curr.wrapping_add(1);
                update_flags = false;
                self.registers.write(dst, result);
                result
            }
            Arg::MemHl => {
                let addr = self.registers.read(Reg16::HL);
                let curr = self.memory.read(addr);
                let result = curr.wrapping_add(1);
                half_carry = curr.half_carry(1);
                self.memory.write(addr, result);
                result as u16
            }
            _ => unreachable!("Unexpected dst: {}", dst),
        };

        if update_flags {
            self.registers.set(Flag::Zero, result == 0);
            self.registers.set(Flag::Subtract, false);
            self.registers.set(Flag::HalfCarry, half_carry);
        }
    }

    /// Decrement instruction
    fn dec(&mut self, dst: Arg) {
        let mut update_flags = true;
        let mut half_carry = false;

        let result = match dst {
            Arg::Reg8(dst) => {
                let curr = self.registers.read(dst);

                // If lower nibble == 0, set the half-carry bit
                half_carry = curr & 0x0F == 0;

                let result = curr.wrapping_sub(1);
                self.registers.write(dst, result);

                result as u16
            }
            Arg::Reg16(dst) => {
                update_flags = false; // 16-bit variant does not touch flags
                let result = self.registers.read(dst).wrapping_sub(1);
                self.registers.write(dst, result);
                result
            }
            Arg::MemHl => {
                let addr = self.registers.read(Reg16::HL);
                let curr = self.memory.read(addr);

                // If lower nibble == 0, set the half-carry bit
                half_carry = curr & 0x0F == 0;

                let result = curr.wrapping_sub(1);
                self.memory.write(addr, result);

                result as u16
            }
            _ => unreachable!("Unexpected dst: {}", dst),
        };

        if update_flags {
            self.registers.set(Flag::Zero, result == 0);
            self.registers.set(Flag::Subtract, true);
            self.registers.set(Flag::HalfCarry, half_carry);
        }
    }

    /// Set and Res instructions
    fn set(&mut self, dst: Arg, bit: u8, reset: bool) {
        let value = match dst {
            Arg::Reg8(dst) => self.registers.read(dst),
            Arg::MemHl => {
                let addr = self.registers.read(Reg16::HL);
                self.memory.read(addr)
            }
            _ => unreachable!("Unexpected dst: {}", dst),
        };

        let result;

        if !reset {
            result = value | (1 << bit);
        } else {
            result = value & !(1 << bit);
        }

        match dst {
            Arg::Reg8(dst) => self.registers.write(dst, result),
            Arg::MemHl => {
                let addr = self.registers.read(Reg16::HL);
                self.memory.write(addr, result);
            }
            _ => unreachable!("Unexpected dst: {}", dst),
        }
    }

    /// Adjust A to BCD following ADD or SUB
    //
    /// Refer to first table on pg. 110 of GB Programming Manual
    fn daa(&mut self) {
        let subtract = self.registers.subtract();
        let half_carry = self.registers.half_carry();
        let mut carry = self.registers.carry();

        let mut a = self.registers.read(Reg8::A);

        if !subtract {
            if carry || a > 0x99 {
                a = a.wrapping_add(0x60);
                carry = true;
            }
            if half_carry || (a & 0x0F) > 0x9 {
                a = a.wrapping_add(0x06);
            }
        } else {
            if half_carry {
                a = a.wrapping_sub(0x06);
            }
            if carry {
                a = a.wrapping_sub(0x60);
            }
        }

        self.registers.write(Reg8::A, a);

        self.registers.set(Flag::Zero, a == 0);
        self.registers.clear(Flag::HalfCarry);
        self.registers.set(Flag::Carry, carry);
    }

    #[allow(dead_code)]
    pub fn memory(&self) -> &MemoryBus {
        &self.memory
    }

    pub fn save_to_bytes(&self, buf: &mut Vec<u8>) {
        self.registers.save_to_bytes(buf);
        self.memory.save_to_bytes(buf);
        self.dma.save_to_bytes(buf);
        buf.push(self.cgb as u8);
        buf.push(self.halted as u8);
        buf.push(self.stopped as u8);
        buf.push(self.speed as u8);
        buf.push(self.ime as u8);
    }

    pub fn load_from_bytes(data: &[u8], pos: &mut usize) -> crate::gbc::error::Result<Self> {
        let registers = RegisterFile::load_from_bytes(data, pos)?;
        let memory = MemoryBus::load_from_bytes(data, pos)?;
        let dma = DmaController::load_from_bytes(data, pos)?;
        let cgb = data[*pos] != 0; *pos += 1;
        let halted = data[*pos] != 0; *pos += 1;
        let stopped = data[*pos] != 0; *pos += 1;
        let speed = data[*pos] != 0; *pos += 1;
        let ime = data[*pos] != 0; *pos += 1;
        Ok(Self { registers, memory, dma, cgb, halted, stopped, speed, ime, trace: None })
    }
}

#[cfg(test)]
impl Cpu {
    /// Execute a single instruction (test helper).
    /// Dispatches through the same handler tables as step().
    pub fn execute(&mut self, instruction: Instruction) {
        use Instruction::*;

        fn r8i(r: Reg8) -> u8 {
            match r {
                Reg8::B => 0, Reg8::C => 1, Reg8::D => 2, Reg8::E => 3,
                Reg8::H => 4, Reg8::L => 5, Reg8::A => 7, Reg8::F => unreachable!(),
            }
        }

        match instruction {
            Nop => { op_nop(self, 0x00, 0, 0); }
            Halt => { op_halt(self, 0x76, 0, 0); }
            Stop => { op_stop(self, 0x10, 0, 0); }
            Di => { op_di(self, 0xF3, 0, 0); }
            Ei => { op_ei(self, 0xFB, 0, 0); }
            Daa => { op_daa(self, 0x27, 0, 0); }
            Cpl => { op_cpl(self, 0x2F, 0, 0); }
            Ccf => { op_ccf(self, 0x3F, 0, 0); }
            Scf => { op_scf(self, 0x37, 0, 0); }
            Rlca => { op_rlca(self, 0x07, 0, 0); }
            Rla => { op_rla(self, 0x17, 0, 0); }
            Rrca => { op_rrca(self, 0x0F, 0, 0); }
            Rra => { op_rra(self, 0x1F, 0, 0); }
            JpHl => { op_jphl(self, 0xE9, 0, 0); }
            RetI => { op_reti(self, 0xD9, 0, 0); }
            LdMemCA => { op_ld_memc_a(self, 0xE2, 0, 0); }
            LdAMemC => { op_ld_a_memc(self, 0xF2, 0, 0); }
            LdiAMemHl => { op_ldi_a_memhl(self, 0x2A, 0, 0); }
            LdiMemHlA => { op_ldi_memhl_a(self, 0x22, 0, 0); }
            LddAMemHl => { op_ldd_a_memhl(self, 0x3A, 0, 0); }
            LddMemHlA => { op_ldd_memhl_a(self, 0x32, 0, 0); }

            // ALU operations (Add, Adc, Sub, Sbc, And, Xor, Or, Cp) — r8, imm8, or MemHl
            Add { src } | Adc { src } | Sub { src } | Sbc { src }
            | And { src } | Xor { src } | Or { src } | Cp { src } => {
                let alu = match instruction {
                    Add { .. } => 0, Adc { .. } => 1, Sub { .. } => 2, Sbc { .. } => 3,
                    And { .. } => 4, Xor { .. } => 5, Or { .. } => 6, Cp { .. } => 7,
                    _ => unreachable!(),
                };
                match src {
                    Arg::Reg8(r) => { op_alu_r8(self, 0x80 + alu * 8 + r8i(r), 0, 0); }
                    Arg::Imm8(n) => { op_alu_imm(self, 0xC6 + alu * 8, n, 0); }
                    Arg::MemHl => { op_alu_r8(self, 0x80 + alu * 8 + 6, 0, 0); }
                    _ => unreachable!(),
                }
            }

            Inc { dst } => match dst {
                Arg::Reg8(r) => { op_inc_r8(self, 0x04 + r8i(r) * 8, 0, 0); }
                _ => unreachable!(),
            },
            Dec { dst } => match dst {
                Arg::Reg8(r) => { op_dec_r8(self, 0x05 + r8i(r) * 8, 0, 0); }
                _ => unreachable!(),
            },

            AddHlReg16 { src } => {
                let op = match src {
                    Reg16::BC => 0x09, Reg16::DE => 0x19,
                    Reg16::HL => 0x29, Reg16::SP => 0x39,
                    _ => unreachable!(),
                };
                op_add_hl_r16(self, op, 0, 0);
            }
            AddSpImm8i { offset } => { op_add_sp_imm8(self, 0xE8, offset as u8, 0); }
            LdHlSpImm8i { offset } => { op_ld_hl_sp_imm8(self, 0xF8, offset as u8, 0); }
            LdhA { offset } => { op_ldh_a(self, 0xF0, offset, 0); }
            Ldh { offset } => { op_ldh(self, 0xE0, offset, 0); }

            Push { src } => {
                let op = match src {
                    Reg16::BC => 0xC5, Reg16::DE => 0xD5,
                    Reg16::HL => 0xE5, Reg16::AF => 0xF5,
                    _ => unreachable!(),
                };
                op_push(self, op, 0, 0);
            }
            Pop { dst } => {
                let op = match dst {
                    Reg16::BC => 0xC1, Reg16::DE => 0xD1,
                    Reg16::HL => 0xE1, Reg16::AF => 0xF1,
                    _ => unreachable!(),
                };
                op_pop(self, op, 0, 0);
            }

            Jp { addr, cond } => {
                let op = match cond {
                    Cond::None => 0xC3, Cond::NotZero => 0xC2,
                    Cond::Zero => 0xCA, Cond::NotCarry => 0xD2,
                    Cond::Carry => 0xDA,
                };
                op_jp(self, op, 0, addr);
            }
            Jr { offset, cond } => {
                let op = match cond {
                    Cond::None => 0x18, Cond::NotZero => 0x20,
                    Cond::Zero => 0x28, Cond::NotCarry => 0x30,
                    Cond::Carry => 0x38,
                };
                op_jr(self, op, offset as u8, 0);
            }
            Call { addr, cond } => {
                let op = match cond {
                    Cond::None => 0xCD, Cond::NotZero => 0xC4,
                    Cond::Zero => 0xCC, Cond::NotCarry => 0xD4,
                    Cond::Carry => 0xDC,
                };
                op_call(self, op, 0, addr);
            }
            Ret { cond } => {
                let op = match cond {
                    Cond::None => 0xC9, Cond::NotZero => 0xC0,
                    Cond::Zero => 0xC8, Cond::NotCarry => 0xD0,
                    Cond::Carry => 0xD8,
                };
                op_ret(self, op, 0, 0);
            }
            Rst { offset } => { op_rst(self, 0xC7 + offset, 0, 0); }

            // CB-prefixed rotate / shift / swap
            Rlc { dst } => { let i = cbidx(dst); cb_rlc(self, i, i, 0); }
            Rl  { dst } => { let i = cbidx(dst); cb_rl(self, 0x10 + i, 0x10 + i, 0); }
            Rrc { dst } => { let i = cbidx(dst); cb_rrc(self, 0x08 + i, 0x08 + i, 0); }
            Rr  { dst } => { let i = cbidx(dst); cb_rr(self, 0x18 + i, 0x18 + i, 0); }
            Sla { dst } => { let i = cbidx(dst); cb_sla(self, 0x20 + i, 0x20 + i, 0); }
            Sra { dst } => { let i = cbidx(dst); cb_sra(self, 0x28 + i, 0x28 + i, 0); }
            Srl { dst } => { let i = cbidx(dst); cb_srl(self, 0x38 + i, 0x38 + i, 0); }
            Swap{dst} => { let i = cbidx(dst); cb_swap(self, 0x30 + i, 0x30 + i, 0); }
            Bit { dst, bit } => { let i = cbidx(dst); cb_bit(self, 0x40 + bit * 8 + i, 0x40 + bit * 8 + i, 0); }
            Set { dst, bit } => { let i = cbidx(dst); cb_set(self, 0xC0 + bit * 8 + i, 0xC0 + bit * 8 + i, 0); }
            Res { dst, bit } => { let i = cbidx(dst); cb_res(self, 0x80 + bit * 8 + i, 0x80 + bit * 8 + i, 0); }

            // LD — all variants used in tests
            Ld { dst, src } => match (dst, src) {
                (Arg::Reg8(r), Arg::Imm8(n))    => { op_ld_r8_imm8(self, 0x06 + r8i(r) * 8, n, 0); }
                (Arg::Reg8(d), Arg::Reg8(s))    => { op_ld_r8_r8(self, 0x40 + r8i(d) * 8 + r8i(s), 0, 0); }
                (Arg::Reg16(r), Arg::Imm16(n))  => {
                    let op = match r { Reg16::BC => 0x01, Reg16::DE => 0x11, Reg16::HL => 0x21, Reg16::SP => 0x31, _ => unreachable!() };
                    op_ld_r16_imm16(self, op, 0, n);
                }
                (Arg::Reg8(Reg8::A), Arg::Mem(Reg16::BC)) => { op_ld_a_mem_r16(self, 0x0A, 0, 0); }
                (Arg::Reg8(Reg8::A), Arg::Mem(Reg16::DE)) => { op_ld_a_mem_r16(self, 0x1A, 0, 0); }
                (Arg::Reg8(r), Arg::Mem(Reg16::HL))       => { op_ld_r8_r8(self, 0x46 + r8i(r) * 8, 0, 0); }
                (Arg::Mem(Reg16::BC), Arg::Reg8(Reg8::A)) => { op_ld_mem_r16_a(self, 0x02, 0, 0); }
                (Arg::Mem(Reg16::DE), Arg::Reg8(Reg8::A)) => { op_ld_mem_r16_a(self, 0x12, 0, 0); }
                (Arg::Mem(Reg16::HL), Arg::Reg8(r))       => { op_ld_r8_r8(self, 0x70 + r8i(r), 0, 0); }
                (Arg::Mem(Reg16::HL), Arg::Imm8(n))       => { op_ld_memhl_imm8(self, 0x36, n, 0); }
                (Arg::Reg8(Reg8::A), Arg::MemImm(a))      => { op_ld_a_memimm(self, 0xFA, 0, a); }
                (Arg::MemImm(a), Arg::Reg8(Reg8::A))      => { op_ld_memimm_a(self, 0xEA, 0, a); }
                (Arg::Reg16(Reg16::SP), Arg::Reg16(Reg16::HL)) => { op_ld_sp_hl(self, 0xF9, 0, 0); }
                (Arg::MemImm(a), Arg::Reg16(Reg16::SP))   => { op_ld_memimm_sp(self, 0x08, 0, a); }
                _ => unreachable!(),
            },
        }
    }
}

/// CB-prefix register index helper (test-only)
#[cfg(test)]
fn cbidx(dst: Arg) -> u8 {
    match dst {
        Arg::Reg8(Reg8::B) => 0, Arg::Reg8(Reg8::C) => 1,
        Arg::Reg8(Reg8::D) => 2, Arg::Reg8(Reg8::E) => 3,
        Arg::Reg8(Reg8::H) => 4, Arg::Reg8(Reg8::L) => 5,
        Arg::MemHl => 6,
        Arg::Reg8(Reg8::A) => 7,
        _ => unreachable!(),
    }
}

#[cfg(test)]
mod test {
    use super::*;

    fn get_cpu() -> Cpu {
        Cpu::new(true)
    }

    #[test]
    fn interrupts() {
        let mut cpu = get_cpu();

        cpu.registers.PC = 0x1000;
        cpu.registers.write(Reg8::A, 0x01u8);
        cpu.registers.write(Reg8::B, 0x40u8);

        // Enable interrupts
        let inst = Instruction::Ei;
        cpu.execute(inst);

        // Enable VBLANK and LCD STAT
        cpu.memory.write(0xFFFF, 0x03u8);

        // Prepare basic ISRs in ROM:
        //
        // Vblank:
        // 1. ADD A, B
        // 2. RETI
        //
        // LcdStat:
        // 1. NOP
        // 2. RET
        let controller = cpu.memory.controller_mut();
        controller.rom.write(0x40, 0x80u8);
        controller.rom.write(0x41, 0xD9u8);
        controller.rom.write(0x48, 0x00u8);
        controller.rom.write(0x49, 0xC9u8);

        // Trigger VBLANK and LCD interrupts
        cpu.trigger_interrupt(Interrupt::Vblank);
        cpu.trigger_interrupt(Interrupt::LcdStat);

        // Execute a CPU step and verify that the ADD
        // in the VBLANK ISR was executed
        cpu.step();
        assert!(!cpu.ime);
        assert_eq!(cpu.registers.PC, 0x41);
        assert_eq!(cpu.registers.read(Reg8::A), 0x41);

        // Step again
        // RETI should restore original PC and re-enable interrupts
        cpu.step();
        assert!(cpu.ime);
        assert_eq!(cpu.registers.PC, 0x1000);

        // Step again -> this should trigger LcdStat and execute a NOP
        cpu.step();
        assert!(!cpu.ime);
        assert_eq!(cpu.registers.PC, 0x49);

        // Last step -> verify PC is restored
        cpu.step();
        assert!(!cpu.ime);
        assert_eq!(cpu.registers.PC, 0x1000);
    }

    #[test]
    fn add() {
        let mut cpu = get_cpu();

        cpu.registers.write(Reg8::A, 0);

        // Normal add
        let inst = Instruction::Add { src: 0x10u8.into() };
        cpu.execute(inst);
        assert_eq!(cpu.registers.read(Reg8::A), 0x10);

        // Overflow
        cpu.registers.write(Reg8::B, 0xF0);
        let inst = Instruction::Add {
            src: Reg8::B.into(),
        };
        cpu.execute(inst);
        assert_eq!(cpu.registers.read(Reg8::A), 0x00);
        assert!(cpu.registers.zero());
        assert!(cpu.registers.carry());

        // Half overflow
        cpu.registers.write(Reg8::A, 0x3C);
        let inst = Instruction::Add { src: 0xFFu8.into() };
        cpu.execute(inst);
        assert_eq!(cpu.registers.read(Reg8::A), 0x3B);
        assert!(!cpu.registers.zero());
        assert!(!cpu.registers.subtract());
        assert!(cpu.registers.half_carry());
        assert!(cpu.registers.carry());
    }

    #[test]
    fn add_hl() {
        let mut cpu = get_cpu();

        let old_zero = cpu.registers.zero();

        cpu.registers.write(Reg8::A, 0);
        cpu.registers.write(Reg16::HL, 0);
        cpu.registers.write(Reg16::DE, 0x1234);

        let inst = Instruction::AddHlReg16 { src: Reg16::DE };

        // Normal add
        cpu.execute(inst);
        assert_eq!(cpu.registers.read(Reg16::HL), 0x1234);
        assert!(cpu.registers.zero() == old_zero);
        assert!(!cpu.registers.subtract());
        assert!(!cpu.registers.half_carry());
        assert!(!cpu.registers.carry());

        // Overflow
        cpu.registers.write(Reg16::DE, 0xEDCC);
        cpu.execute(inst);
        assert_eq!(cpu.registers.read(Reg16::HL), 0x0);
        assert!(cpu.registers.zero() == old_zero);
        assert!(!cpu.registers.subtract());
        assert!(cpu.registers.half_carry());
        assert!(cpu.registers.carry());
    }

    #[test]
    fn adc() {
        let mut cpu = get_cpu();

        cpu.registers.write(Reg8::A, 0);

        // Normal add
        let inst = Instruction::Adc { src: 0x10u8.into() };
        cpu.registers.set(Flag::Carry, true);
        cpu.execute(inst);
        assert_eq!(cpu.registers.read(Reg8::A), 0x11);
        assert!(!cpu.registers.zero());
        assert!(!cpu.registers.carry());

        // Overflow
        let inst = Instruction::Adc {
            src: Reg8::B.into(),
        };
        cpu.registers.set(Flag::Carry, true);
        cpu.registers.write(Reg8::A, 0xE1);
        cpu.registers.write(Reg8::B, 0x1E);
        cpu.execute(inst);
        assert_eq!(cpu.registers.read(Reg8::A), 0x00);
        assert!(cpu.registers.zero());
        assert!(cpu.registers.half_carry());
        assert!(cpu.registers.carry());
    }

    #[test]
    fn sub() {
        let mut cpu = get_cpu();

        cpu.registers.write(Reg8::A, 0x10);

        // Normal sub
        let inst = Instruction::Sub { src: 0x10u8.into() };
        cpu.execute(inst);
        assert_eq!(cpu.registers.read(Reg8::A), 0);
        assert!(cpu.registers.zero());
        assert!(cpu.registers.subtract());

        // Underflow
        let inst = Instruction::Sub { src: 0x10u8.into() };
        cpu.execute(inst);
        assert_eq!(cpu.registers.read(Reg8::A), 0xF0);
        assert!(!cpu.registers.zero());
        assert!(cpu.registers.subtract());
        assert!(cpu.registers.carry());

        // Half underflow
        cpu.registers.write(Reg8::A, 0x3E);
        let inst = Instruction::Sub { src: 0xFu8.into() };
        cpu.execute(inst);
        assert_eq!(cpu.registers.read(Reg8::A), 0x2F);
        assert!(!cpu.registers.zero());
        assert!(cpu.registers.subtract());

        cpu.registers.write(Reg8::F, 0);
        cpu.registers.write(Reg8::A, 0x83);
        cpu.registers.write(Reg8::B, 0x38);
        let inst = Instruction::Sub {
            src: Reg8::B.into(),
        };
        cpu.execute(inst);
        assert_eq!(cpu.registers.read(Reg8::A), 0x4B);
        assert!(!cpu.registers.zero());
        assert!(cpu.registers.subtract());
        assert!(cpu.registers.half_carry());
    }

    #[test]
    fn sbc() {
        let mut cpu = get_cpu();

        // Normal sub
        let inst = Instruction::Sbc { src: 0x09u8.into() };
        cpu.registers.write(Reg8::A, 0x0A);
        cpu.registers.set(Flag::Carry, true);
        cpu.execute(inst);
        assert_eq!(cpu.registers.read(Reg8::A), 0x00);
        assert!(cpu.registers.zero());
        assert!(!cpu.registers.carry());

        // Overflow
        let inst = Instruction::Sbc {
            src: Reg8::B.into(),
        };
        cpu.registers.set(Flag::Carry, true);
        cpu.registers.write(Reg8::A, 0x3B);
        cpu.registers.write(Reg8::B, 0x4F);
        cpu.execute(inst);
        assert_eq!(cpu.registers.read(Reg8::A), 0xEB);
        assert!(!cpu.registers.zero());
        assert!(cpu.registers.subtract());
        assert!(cpu.registers.half_carry());
        assert!(cpu.registers.carry());
    }

    #[test]
    fn logical() {
        let mut cpu = get_cpu();

        // AND
        let inst = Instruction::And { src: 0x02u8.into() };
        cpu.registers.write(Reg8::A, 0x0A);
        cpu.execute(inst);
        assert_eq!(cpu.registers.read(Reg8::A), 0x02);
        assert!(!cpu.registers.zero());
        assert!(cpu.registers.half_carry());

        // OR
        let inst = Instruction::Or { src: 0xF0u8.into() };
        cpu.registers.write(Reg8::A, 0x0A);
        cpu.execute(inst);
        assert_eq!(cpu.registers.read(Reg8::A), 0xFA);
        assert!(!cpu.registers.zero());
        assert!(!cpu.registers.half_carry());

        // XOR
        let inst = Instruction::Xor { src: 0xFFu8.into() };
        cpu.registers.write(Reg8::A, 0x0F);
        cpu.execute(inst);
        assert_eq!(cpu.registers.read(Reg8::A), 0xF0);
        assert!(!cpu.registers.zero());
        assert!(!cpu.registers.half_carry());
    }

    #[test]
    fn inc() {
        let mut cpu = get_cpu();

        cpu.registers.write(Reg8::A, 0xFF);

        // Overflow
        let inst = Instruction::Inc {
            dst: Reg8::A.into(),
        };
        cpu.execute(inst);
        assert_eq!(cpu.registers.read(Reg8::A), 0);
        assert!(cpu.registers.zero());
        assert!(cpu.registers.half_carry());
        assert!(!cpu.registers.subtract());
    }

    #[test]
    fn dec() {
        let mut cpu = get_cpu();

        cpu.registers.write(Reg8::A, 0);

        // Underflow
        let inst = Instruction::Dec {
            dst: Reg8::A.into(),
        };
        cpu.execute(inst);
        assert_eq!(cpu.registers.read(Reg8::A), 0xFF);
        assert!(!cpu.registers.zero());
        assert!(cpu.registers.half_carry());
        assert!(cpu.registers.subtract());
    }

    #[test]
    fn cp() {
        let mut cpu = get_cpu();

        cpu.registers.write(Reg8::A, 0x3C);
        cpu.registers.write(Reg8::B, 0x2F);

        let inst = Instruction::Cp {
            src: Reg8::B.into(),
        };
        cpu.execute(inst);
        assert!(!cpu.registers.zero());
        assert!(cpu.registers.subtract());
        assert!(cpu.registers.half_carry());
        assert!(!cpu.registers.carry());

        let inst = Instruction::Cp { src: 0x3Cu8.into() };
        cpu.execute(inst);
        assert!(cpu.registers.zero());
        assert!(cpu.registers.subtract());

        let (addr, value) = (0xC000, 0x40u8);
        cpu.memory.write(addr, value);
        cpu.registers.write(Reg16::HL, addr);
        let inst = Instruction::Cp { src: Arg::MemHl };
        cpu.execute(inst);
        assert!(!cpu.registers.zero());
        assert!(cpu.registers.subtract());
        assert!(!cpu.registers.half_carry());
        assert!(cpu.registers.carry());
    }

    #[test]
    fn daa() {
        let mut cpu = get_cpu();

        cpu.registers.write(Reg8::A, 0x45);
        cpu.registers.write(Reg8::B, 0x38);

        // ADD, then DAA
        let inst = Instruction::Add {
            src: Reg8::B.into(),
        };
        cpu.execute(inst);
        assert_eq!(cpu.registers.read(Reg8::A), 0x7D);

        let inst = Instruction::Daa;
        cpu.execute(inst);
        assert_eq!(cpu.registers.read(Reg8::A), 0x83);
        assert!(!cpu.registers.carry());

        // SUB, then DAA
        let inst = Instruction::Sub {
            src: Reg8::B.into(),
        };
        cpu.execute(inst);
        assert_eq!(cpu.registers.read(Reg8::A), 0x4B);
        assert!(cpu.registers.subtract());

        let inst = Instruction::Daa;
        cpu.execute(inst);
        assert_eq!(cpu.registers.read(Reg8::A), 0x45);
    }

    #[test]
    fn push_and_pop() {
        let mut cpu = get_cpu();

        cpu.registers.write(Reg16::SP, 0xFFFE);
        cpu.registers.write(Reg16::HL, 0x1234);

        let inst = Instruction::Push {
            src: Reg16::HL.into(),
        };
        cpu.execute(inst);
        assert_eq!(cpu.registers.SP, 0xFFFC);

        let inst = Instruction::Pop {
            dst: Reg16::AF.into(),
        };
        cpu.execute(inst);
        assert_eq!(cpu.registers.SP, 0xFFFE);
        assert_eq!(cpu.registers.read(Reg16::AF), 0x1230);
    }

    #[test]
    fn jumps() {
        let mut cpu = get_cpu();

        let inst = Instruction::Jp {
            addr: 0x2345,
            cond: Cond::None,
        };
        cpu.execute(inst);
        assert_eq!(cpu.registers.PC, 0x2345);

        let inst = Instruction::Jp {
            addr: 0x1234,
            cond: Cond::Zero,
        };
        cpu.registers.set(Flag::Zero, true);
        cpu.execute(inst);
        assert_eq!(cpu.registers.PC, 0x1234);

        let inst = Instruction::Jr {
            offset: -0x36,
            cond: Cond::None,
        };
        cpu.execute(inst);
        assert_eq!(cpu.registers.PC, 0x1200);

        let inst = Instruction::Jr {
            offset: 1,
            cond: Cond::NotCarry,
        };
        cpu.registers.write(Reg16::PC, 0xFFFF);
        cpu.registers.set(Flag::Carry, false);
        cpu.execute(inst);
        assert_eq!(cpu.registers.PC, 0x2);

        let inst = Instruction::JpHl;
        cpu.registers.write(Reg16::HL, 0x1234);
        cpu.execute(inst);
        assert_eq!(cpu.registers.PC, 0x1234);

        // CALL, then RET
        let inst = Instruction::Call {
            addr: 0x1234,
            cond: Cond::None,
        };
        cpu.registers.write(Reg16::PC, 0xFF00);
        cpu.registers.write(Reg16::SP, 0xFFFE);
        cpu.execute(inst);
        assert_eq!(cpu.registers.PC, 0x1234);
        assert_eq!(cpu.registers.SP, 0xFFFC);

        let inst = Instruction::Ret { cond: Cond::None };
        cpu.execute(inst);
        assert_eq!(cpu.registers.PC, 0xFF03); // Next PC pushed during CALL
        assert_eq!(cpu.registers.SP, 0xFFFE);

        // RST
        let inst = Instruction::Rst { offset: 0x10 };
        cpu.registers.write(Reg16::PC, 0xFF00);
        cpu.registers.write(Reg16::SP, 0xFFFE);
        cpu.execute(inst);
        assert_eq!(cpu.registers.PC, 0x0010);
        assert_eq!(cpu.registers.SP, 0xFFFC);
        assert_eq!(cpu.pop(), 0xFF01);
    }

    #[test]
    fn ret() {
        let mut cpu = get_cpu();

        let inst = Instruction::Ret { cond: Cond::None };
        cpu.push(0x1234);
        cpu.execute(inst);
        assert_eq!(cpu.registers.PC, 0x1234);

        let inst = Instruction::RetI;
        cpu.push(0x1234);
        cpu.execute(inst);
        assert_eq!(cpu.registers.PC, 0x1234);
    }

    #[test]
    fn rotate_shift_swap() {
        let mut cpu = get_cpu();

        let inst = Instruction::Rlc {
            dst: Reg8::B.into(),
        };
        cpu.registers.write(Reg8::B, 0x85);
        cpu.execute(inst);
        assert_eq!(cpu.registers.read(Reg8::B), 0x0B);
        assert!(!cpu.registers.zero());
        assert!(!cpu.registers.subtract());
        assert!(!cpu.registers.half_carry());
        assert!(cpu.registers.carry());

        let inst = Instruction::Rl {
            dst: Reg8::L.into(),
        };
        cpu.registers.clear(Flag::Carry);
        cpu.registers.write(Reg8::L, 0x80);
        cpu.execute(inst);
        assert_eq!(cpu.registers.read(Reg8::L), 0);
        assert!(cpu.registers.zero());
        assert!(!cpu.registers.subtract());
        assert!(!cpu.registers.half_carry());
        assert!(cpu.registers.carry());

        let inst = Instruction::Rrc {
            dst: Reg8::C.into(),
        };
        cpu.registers.clear(Flag::Carry);
        cpu.registers.write(Reg8::C, 0x1);
        cpu.execute(inst);
        assert_eq!(cpu.registers.read(Reg8::C), 0x80);
        assert!(!cpu.registers.zero());
        assert!(!cpu.registers.subtract());
        assert!(!cpu.registers.half_carry());
        assert!(cpu.registers.carry());

        let inst = Instruction::Rr {
            dst: Reg8::A.into(),
        };
        cpu.registers.clear(Flag::Carry);
        cpu.registers.write(Reg8::A, 0x1);
        cpu.execute(inst);
        assert_eq!(cpu.registers.read(Reg8::A), 0);
        assert!(cpu.registers.zero());
        assert!(!cpu.registers.subtract());
        assert!(!cpu.registers.half_carry());
        assert!(cpu.registers.carry());

        let inst = Instruction::Sla {
            dst: Reg8::D.into(),
        };
        cpu.registers.write(Reg8::D, 0x80);
        cpu.execute(inst);
        assert_eq!(cpu.registers.read(Reg8::D), 0);
        assert!(cpu.registers.zero());
        assert!(!cpu.registers.subtract());
        assert!(!cpu.registers.half_carry());
        assert!(cpu.registers.carry());

        let inst = Instruction::Sra {
            dst: Reg8::A.into(),
        };
        cpu.registers.write(Reg8::A, 0x8A);
        cpu.execute(inst);
        assert_eq!(cpu.registers.read(Reg8::A), 0xC5);
        assert!(!cpu.registers.zero());
        assert!(!cpu.registers.subtract());
        assert!(!cpu.registers.half_carry());
        assert!(!cpu.registers.carry());

        cpu.registers.write(Reg8::A, 0x1);
        cpu.execute(inst);
        assert_eq!(cpu.registers.read(Reg8::A), 0);
        assert!(cpu.registers.zero());
        assert!(!cpu.registers.subtract());
        assert!(!cpu.registers.half_carry());
        assert!(cpu.registers.carry());

        let inst = Instruction::Srl {
            dst: Reg8::A.into(),
        };
        cpu.registers.clear(Flag::Carry);
        cpu.registers.write(Reg8::A, 0xFF);
        cpu.execute(inst);
        assert_eq!(cpu.registers.read(Reg8::A), 0x7F);
        assert!(!cpu.registers.zero());
        assert!(!cpu.registers.subtract());
        assert!(!cpu.registers.half_carry());
        assert!(cpu.registers.carry());

        let inst = Instruction::Swap {
            dst: Reg8::A.into(),
        };
        cpu.registers.write(Reg8::A, 0xF1);
        cpu.execute(inst);
        assert_eq!(cpu.registers.read(Reg8::A), 0x1F);
        assert!(!cpu.registers.zero());
        assert!(!cpu.registers.subtract());
        assert!(!cpu.registers.half_carry());
        assert!(!cpu.registers.carry());
    }

    #[test]
    fn bit() {
        let mut cpu = get_cpu();

        let inst = Instruction::Bit {
            dst: Reg8::B.into(),
            bit: 2,
        };
        cpu.registers.write(Reg8::B, 0x4);
        cpu.execute(inst);
        assert!(!cpu.registers.zero());
        assert!(!cpu.registers.subtract());
        assert!(cpu.registers.half_carry());

        let inst = Instruction::Set {
            dst: Reg8::B.into(),
            bit: 3,
        };
        cpu.registers.write(Reg8::B, 0x7);
        cpu.execute(inst);
        assert_eq!(cpu.registers.read(Reg8::B), 0xF);

        let inst = Instruction::Res {
            dst: Reg8::B.into(),
            bit: 3,
        };
        cpu.registers.write(Reg8::B, 0xF);
        cpu.execute(inst);
        assert_eq!(cpu.registers.read(Reg8::B), 0x7);
    }
}
