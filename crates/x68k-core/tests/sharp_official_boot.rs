use std::fs;
use std::path::PathBuf;

use x68k_core::{DriveId, Machine, MachineConfig, MediaFormat, RomKind};

/// 16進文字列を検証し、対応するバイト列へ復号する。
fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|byte| format!("{byte:02x}")).collect()
}

#[test]
/// `official_ipl_boots_human68k_to_the_command_prompt_workload` が想定する振る舞いを満たし、回帰がないことを検証する。
fn official_ipl_boots_human68k_to_the_command_prompt_workload() {
    let root = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../..");
    let ipl = fs::read(root.join("web/public/sharp/IPLROM.DAT")).expect("official IPL fixture");
    let human =
        fs::read(root.join("web/public/sharp/HUMAN302.XDF")).expect("official Human68k fixture");
    let mut machine = Machine::new(MachineConfig::default()).unwrap();
    machine
        .mount_media(DriveId::Floppy(0), MediaFormat::Xdf, &human, true)
        .unwrap();
    machine.load_rom(RomKind::Ipl, &ipl).unwrap();
    machine.set_cpu_trap_diagnostics(true);

    let mut reached_human68k_ram = false;
    for frame_index in 0..700 {
        let frame = machine.run_frame();
        assert_eq!((frame.width, frame.height), (768, 512));
        if frame_index >= 110 {
            let (pc, _, _, _, _) = machine.cpu_diagnostics();
            reached_human68k_ram |= pc < 0x00fe_0000;
        }
    }

    assert!(
        reached_human68k_ram,
        "boot must leave the IPL FDC wait/error handlers and execute Human68k in RAM"
    );
    let (pc, _, stopped, _, _) = machine.cpu_diagnostics();
    assert!(pc < 0x00fe_0000, "Human68k must still be executing in RAM");
    assert!(!stopped, "CPU must not halt during Human68k startup");
    assert_ne!(
        machine.cpu_trap_diagnostics(),
        Some((4, 0x00ff, "illegal")),
        "boot must not enter the IPL $0018 / PC=$00000004 fatal-error path"
    );
    let (commands, sector_reads, _, _, output) = machine.fdc_diagnostics();
    assert!(commands > 0, "IPL must issue FDC commands");
    assert!(
        sector_reads >= 250,
        "Human68k must finish loading its startup files instead of stalling at $00ff9006"
    );
    assert_eq!(output, 0, "FDC execution/result FIFO must be drained");
    assert_eq!(
        machine.ioc_diagnostics().6,
        0,
        "IOC acknowledge must not be spurious"
    );
    let hashes = machine.content_hashes();
    assert_eq!(
        hex(&hashes.iter().find(|(slot, _)| slot == "rom:ipl").unwrap().1),
        "8ead1d0f4ebb9c59a7fa118596f819e191c310442a00c56ab5ec5e9e7a189677"
    );
    assert_eq!(
        hex(&hashes.iter().find(|(slot, _)| slot == "fdd:0").unwrap().1),
        "bc814dab949f517ec3fb5b5b0e71f2adb468107ae0c431ee92ec38b30b031833"
    );
}
