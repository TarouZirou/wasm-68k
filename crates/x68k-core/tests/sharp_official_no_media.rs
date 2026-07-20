use std::fs;
use std::path::PathBuf;

use x68k_core::{Machine, MachineConfig, RomKind};

#[test]
/// `nine_mib_x68000_reaches_the_ipl_no_boot_disk_path` が想定する振る舞いを満たし、回帰がないことを検証する。
fn nine_mib_x68000_reaches_the_ipl_no_boot_disk_path() {
    let root = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../..");
    let ipl = fs::read(root.join("web/public/sharp/IPLROM.DAT")).expect("official IPL fixture");
    let mut machine = Machine::new(MachineConfig {
        ram_bytes: 9 * 1024 * 1024,
        ..MachineConfig::default()
    })
    .unwrap();
    machine.set_cpu_trap_diagnostics(true);
    machine.load_rom(RomKind::Ipl, &ipl).unwrap();

    for _ in 0..300 {
        machine.run_frame();
        machine.drain_audio();
    }

    assert_eq!(
        machine.bus_fault_diagnostics().0,
        Some(0x00bf_fffc),
        "IPL must detect the end of the supported main-memory window"
    );
    assert_ne!(
        machine.cpu_trap_diagnostics(),
        Some((4, 0x00ff, "illegal")),
        "no-media boot must not enter the IPL fatal-error screen"
    );
    let (commands, sector_reads, _, _, _) = machine.fdc_diagnostics();
    assert!(commands > 1_000, "IPL must poll all empty floppy drives");
    assert_eq!(sector_reads, 0, "an empty drive cannot transfer a sector");
}
