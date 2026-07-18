use std::fs;
use std::path::PathBuf;

use x68k_core::{DriveId, InputEvent, Machine, MachineConfig, MachineModel, MediaFormat, RomKind};

fn main() {
    let mut arguments = std::env::args().skip(1);
    let profile = arguments.next().unwrap_or_else(|| "x68000".to_string());
    let frames = arguments
        .next()
        .map(|value| value.parse::<u32>().expect("frame count"))
        .unwrap_or(180);
    let (model, ipl_name) = match profile.as_str() {
        "x68000" => (MachineModel::X68000, "IPLROM.DAT"),
        "xvi" => (MachineModel::X68000Xvi, "IPLROMXV.DAT"),
        "x68030" => (MachineModel::X68030, "IPLROM30.DAT"),
        _ => panic!("profile must be x68000, xvi or x68030"),
    };
    let root = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../..");
    let local_assets = root.join("local-assets/xm6");
    let ram_mib = std::env::var("X68K_RAM_MIB")
        .ok()
        .and_then(|value| value.parse::<usize>().ok())
        .unwrap_or_else(|| if model == MachineModel::X68030 { 12 } else { 2 });
    let ipl_path = std::env::var_os("X68K_IPL")
        .map(PathBuf::from)
        .unwrap_or_else(|| root.join("web/public/sharp").join(ipl_name));
    let ipl = fs::read(&ipl_path).expect("official IPL");
    let floppy_path = std::env::var_os("X68K_FDD0")
        .map(PathBuf::from)
        .unwrap_or_else(|| root.join("web/public/sharp/HUMAN302.XDF"));
    let floppy = fs::read(&floppy_path).expect("FDD0 image");
    let mut machine = Machine::new(MachineConfig {
        model,
        ram_bytes: ram_mib * 1024 * 1024,
        ..MachineConfig::default()
    })
    .expect("machine");
    machine.set_cpu_trap_diagnostics(std::env::var_os("X68K_TRACE_TRAPS").is_some());
    if let Ok(cgrom) = fs::read(local_assets.join("CGROM.DAT")) {
        machine
            .load_rom(RomKind::CharacterGenerator, &cgrom)
            .expect("local CGROM");
        println!("loaded local CGROM");
    }
    // X68030では内蔵SCSI ROMを自動検出する。X68000/XVIで拡張SCSI ROMの
    // デコードを調査したい場合だけ環境変数で明示的に有効化する。
    if (model == MachineModel::X68030 || std::env::var_os("X68K_LOAD_SCSI").is_some())
        && let Ok(scsi) = fs::read(local_assets.join("SCSIINROM.DAT"))
    {
        machine
            .load_rom(RomKind::Scsi, &scsi)
            .expect("local internal SCSI ROM");
        println!("loaded local internal SCSI ROM");
    }
    machine
        .mount_media(DriveId::Floppy(0), MediaFormat::Xdf, &floppy, true)
        .expect("mount FDD0");
    println!("mounted FDD0 {}", floppy_path.display());
    machine.load_rom(RomKind::Ipl, &ipl).expect("load IPL");

    let (initial_pc, initial_sr, _, initial_sp, _) = machine.cpu_diagnostics();
    println!(
        "profile={profile} initial_pc={initial_pc:08x} initial_sp={initial_sp:08x} initial_sr={initial_sr:04x}"
    );

    if let Ok(count) = std::env::var("X68K_TRACE_INSTRUCTIONS") {
        let count = count.parse::<usize>().expect("instruction count");
        for index in 0..count {
            let (pc, opcode, sp, sr, cycles) = machine.step_instruction_diagnostics();
            let (mnemonic, _) = m68k::dasm::disassemble(
                pc,
                opcode,
                match model {
                    MachineModel::X68000 | MachineModel::X68000Xvi => m68k::CpuType::M68000,
                    MachineModel::X68030 => m68k::CpuType::M68EC030,
                },
            );
            println!(
                "instruction={index} pc={pc:08x} opcode={opcode:04x} {mnemonic} sp={sp:08x} sr={sr:04x} cycles={cycles}"
            );
        }
        return;
    }

    let mut previous = (0, 0);
    let trace_every_frame = std::env::var_os("X68K_TRACE_EVERY").is_some();
    let trace_from = std::env::var("X68K_TRACE_FROM")
        .ok()
        .and_then(|value| value.parse::<u32>().ok())
        .unwrap_or(1);
    let key_at = std::env::var("X68K_KEY_AT")
        .ok()
        .and_then(|value| value.parse::<u32>().ok());
    let key_scancode = std::env::var("X68K_KEY_SCANCODE")
        .ok()
        .and_then(|value| u8::from_str_radix(value.trim_start_matches("0x"), 16).ok())
        .unwrap_or(0x35);
    let mut key_events = std::env::var("X68K_KEYS")
        .ok()
        .into_iter()
        .flat_map(|value| {
            value
                .split(',')
                .filter_map(|event| {
                    let (frame, scan) = event.split_once(':')?;
                    Some((
                        frame.parse::<u32>().ok()?,
                        u8::from_str_radix(scan.trim_start_matches("0x"), 16).ok()?,
                    ))
                })
                .collect::<Vec<_>>()
        })
        .collect::<Vec<_>>();
    if let Some(frame) = key_at {
        key_events.push((frame, key_scancode));
    }
    let mut audio_peak = 0.0f32;
    let mut audio_samples = 0usize;
    for frame in 1..=frames {
        for &(_, scancode) in key_events.iter().filter(|(at, _)| *at == frame) {
            machine.input(InputEvent::Key {
                scancode,
                pressed: true,
            });
        }
        for &(_, scancode) in key_events
            .iter()
            .filter(|(at, _)| frame == at.saturating_add(1))
        {
            machine.input(InputEvent::Key {
                scancode,
                pressed: false,
            });
        }
        let result = machine.run_frame();
        let samples = machine.drain_audio();
        audio_samples += samples.len();
        audio_peak = samples
            .into_iter()
            .fold(audio_peak, |peak, sample| peak.max(sample.abs()));
        if trace_every_frame && frame >= trace_from
            || (result.width, result.height) != previous
            || matches!(frame, 1 | 2 | 5 | 10 | 30 | 60 | 120 | 180)
            || frame == frames
        {
            let (pc, sr, stopped, sp, exception) = machine.cpu_diagnostics();
            let (first_fault, last_fault, faults) = machine.bus_fault_diagnostics();
            let (fdc_commands, fdc_sector_reads, fdc_command, fdc_status, fdc_output) =
                machine.fdc_diagnostics();
            let [fdc_st0, fdc_st1, fdc_st2] = machine.fdc_result_status();
            let fdc_parameters = machine.fdc_command_parameters();
            let (dma_csr, dma_cer, dma_ocr, dma_ccr, dma_mtc, dma_mar, dma_dar) =
                machine.dma_diagnostics(0);
            let (
                ioc_signal,
                ioc_request,
                ioc_enable,
                ioc_vector,
                ioc_handler,
                ioc_acks,
                ioc_spurious,
            ) = machine.ioc_diagnostics();
            let non_black = machine
                .framebuffer()
                .iter()
                .filter(|&&pixel| pixel != 0)
                .count();
            println!(
                "frame={frame} size={}x{} pc={pc:08x} sr={sr:04x} sp={sp:08x} stopped={stopped} exception={exception:?} first_fault={first_fault:?} last_fault={last_fault:?} faults={faults} fdc_commands={fdc_commands} fdc_sector_reads={fdc_sector_reads} fdc_command={fdc_command:02x} fdc_params={fdc_parameters:02x?} fdc_status={fdc_status:02x} fdc_output={fdc_output} fdc_st={fdc_st0:02x}/{fdc_st1:02x}/{fdc_st2:02x} dma={dma_csr:02x}/{dma_cer:02x}/{dma_ocr:02x}/{dma_ccr:02x}/mtc={dma_mtc:04x}/mar={dma_mar:08x}/dar={dma_dar:08x} ioc={ioc_signal:02x}/{ioc_request:02x}/{ioc_enable:02x}/{ioc_vector:02x}->{ioc_handler:08x}/acks={ioc_acks}/spurious={ioc_spurious} non_black={non_black}",
                result.width, result.height,
            );
            previous = (result.width, result.height);
        }
    }
    println!("audio_samples={audio_samples} audio_peak={audio_peak:.6}");
    if let Some(path) = std::env::var_os("X68K_TRACE_PPM") {
        let (width, height) = machine.screen_dimensions();
        let mut ppm = format!("P6\n{width} {height}\n255\n").into_bytes();
        for &pixel in machine.framebuffer() {
            let (red, green, blue) = x68k_core::color::grbi_to_rgb(pixel);
            ppm.extend([red, green, blue]);
        }
        fs::write(&path, ppm).expect("write trace PPM");
        println!("wrote {}", PathBuf::from(path).display());
    }
    if let Ok(address) = std::env::var("X68K_TRACE_RAM") {
        let address = u32::from_str_radix(address.trim_start_matches("0x"), 16)
            .expect("X68K_TRACE_RAM hex address");
        let bytes = machine.ram_diagnostics(address, 128);
        for (line, chunk) in bytes.chunks(16).enumerate() {
            let at = address + (line * 16) as u32;
            println!("ram {at:08x}: {chunk:02x?}");
        }
    }
    if let Some((pc, opcode, kind)) = machine.cpu_trap_diagnostics() {
        println!("last_cpu_trap kind={kind} pc={pc:08x} opcode={opcode:04x}");
    }
}
