use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use super::{cartridge::Cartridge, Gameboy};

fn run_single_test_rom(
    rom_path: &PathBuf,
    timeout: Option<u64>,
    output_check_fn: impl Fn(String) -> Option<bool> + Send + Sync + 'static,
) -> bool {
    let data = std::fs::read(rom_path).unwrap();
    let cartridge = Cartridge::from_bytes(data, false);
    let mut gameboy = Gameboy::init(cartridge, false).unwrap();

    let start = Instant::now();
    let timeout = Duration::from_secs(timeout.unwrap_or(60));

    let mut passed = false;

    loop {
        if start.elapsed() > timeout {
            eprintln!("Test timed out after {:?}", timeout);
            break;
        }

        gameboy.frame(None);

        let serial = gameboy.serial_output();

        if let Some(result) = output_check_fn(serial) {
            passed = result;
            break;
        }
    }

    passed
}

#[test]
fn test_cpu_instrs() {
    let rom_path = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("..")
        .join("samples")
        .join("blargg")
        .join("cpu_instrs")
        .join("cpu_instrs.gb");
    let timeout = 180;

    let passed = run_single_test_rom(&rom_path, Some(timeout), |line| {
        if line.contains("Passed") {
            Some(true)
        } else if line.contains("Failed") {
            Some(false)
        } else {
            None
        }
    });

    assert!(passed);
}

#[test]
#[ignore]
fn test_cpu_instrs_individual() {
    const TEST_ROMS: &[&str] = &[
        "01-special.gb",
        "02-interrupts.gb",
        "03-op sp,hl.gb",
        "04-op r,imm.gb",
        "05-op rp.gb",
        "06-ld r,r.gb",
        "07-jr,jp,call,ret,rst.gb",
        "08-misc instrs.gb",
        "09-op r,r.gb",
        "10-bit ops.gb",
        "11-op a,(hl).gb",
    ];

    let rom_paths: Vec<PathBuf> = TEST_ROMS
        .iter()
        .map(|name| {
            Path::new(env!("CARGO_MANIFEST_DIR"))
                .join("..")
                .join("samples")
                .join("blargg")
                .join("cpu_instrs")
                .join(name)
        })
        .collect();

    for path in rom_paths {
        let passed = run_single_test_rom(&path, None, |line| {
            if line.contains("Passed") {
                Some(true)
            } else if line.contains("Failed") {
                Some(false)
            } else {
                None
            }
        });

        assert!(passed, "Test {} failed!", path.to_str().unwrap());
    }
}

#[test]
fn test_instr_timing() {
    let rom_path = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("..")
        .join("samples")
        .join("blargg")
        .join("instr_timing")
        .join("instr_timing.gb");
    let timeout = 120;

    let passed = run_single_test_rom(&rom_path, Some(timeout), |line| {
        if line.contains("Passed") {
            Some(true)
        } else if line.contains("Failed") {
            Some(false)
        } else {
            None
        }
    });

    assert!(passed);
}
