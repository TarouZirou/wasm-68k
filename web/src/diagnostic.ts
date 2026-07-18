const FDC_DATA = 0x00e9_4003;
const HDC_BASE = 0x00e9_6000;

export type DiagnosticModel = "x68000" | "xvi" | "x68030";

/** GPL-2.0-onlyで再配布可能な、プロジェクト自作の最小診断IPL。 */
export function createDiagnosticIpl(model: DiagnosticModel): Uint8Array {
  const rom = new Uint8Array(0x20_000);
  const view = new DataView(rom.buffer);
  view.setUint32(0x10_000, 0x0000_1000, false);
  view.setUint32(0x10_004, 0x00fe_0010, false);
  const program: number[] = [];

  const moveByteImmediate = (value: number, address: number): void => {
    program.push(0x13, 0xfc, 0x00, value & 0xff);
    program.push(
      (address >>> 24) & 0xff,
      (address >>> 16) & 0xff,
      (address >>> 8) & 0xff,
      address & 0xff,
    );
  };
  const moveWordImmediate = (value: number, address: number): void => {
    program.push(0x33, 0xfc, (value >>> 8) & 0xff, value & 0xff);
    program.push(
      (address >>> 24) & 0xff,
      (address >>> 16) & 0xff,
      (address >>> 8) & 0xff,
      address & 0xff,
    );
  };
  const moveLongImmediate = (value: number, address: number): void => {
    program.push(
      0x23, 0xfc,
      (value >>> 24) & 0xff, (value >>> 16) & 0xff,
      (value >>> 8) & 0xff, value & 0xff,
      (address >>> 24) & 0xff, (address >>> 16) & 0xff,
      (address >>> 8) & 0xff, address & 0xff,
    );
  };

  // 例外時も無限にRAMを実行せず、色付き停止画面へ入る診断vector。
  moveLongImmediate(0x00fe_0820, 2 * 4); // bus error
  moveLongImmediate(0x00fe_0840, 3 * 4); // address error
  moveLongImmediate(0x00fe_0860, 4 * 4); // illegal instruction

  // CRTC/videoを65536色へ設定し、左上に赤い画素を描く。
  moveByteImmediate(0x08, 0x00e8_0028);
  moveByteImmediate(0x03, 0x00e8_2401);
  moveByteImmediate(0x61, 0x00e8_2601); // graphics + text + sprite/BG
  moveWordImmediate(0x07c0, 0x00c0_0000);

  // text palette 1とTVRAM先頭画素。
  moveWordImmediate(0x003e, 0x00e8_2202);
  moveByteImmediate(0x80, 0x00e0_0000);

  // sprite 0: 画面左上、pattern 0、priority 3。
  moveWordImmediate(16, 0x00eb_0000);
  moveWordImmediate(16, 0x00eb_0002);
  moveWordImmediate(0, 0x00eb_0004);
  moveByteImmediate(3, 0x00eb_0006);
  moveByteImmediate(0x10, 0x00eb_8000);
  moveByteImmediate(2, 0x00eb_0808);

  // raster handler vector。MFP有効化は全デバイス診断の完了後に行う。
  moveLongImmediate(0x00fe_0800, 0x0000_0138); // MFP default vector 0x4e

  // HD63450 ch0でIPL内の識別byteをGVRAMへ1byte転送。
  moveByteImmediate(0x05, 0x00e8_4006); // MAR/DAR increment
  moveByteImmediate(0x00, 0x00e8_400a);
  moveByteImmediate(0x01, 0x00e8_400b);
  for (const [offset, value] of [
    [0x0c, 0x00], [0x0d, 0xfe], [0x0e, 0x09], [0x0f, 0x00],
    [0x14, 0x00], [0x15, 0xc0], [0x16, 0x00], [0x17, 0x04],
  ] as const) moveByteImmediate(value, 0x00e8_4000 + offset);
  moveByteImmediate(0x80, 0x00e8_4007);

  // YM2151 ch0を両chへ出し、全operatorをkey-onする。
  for (const [register, value] of [
    [0x20, 0xc7], [0x28, 0x4c], [0x60, 0x00], [0x80, 0x1f], [0x08, 0x78],
  ]) {
    moveByteImmediate(register, 0x00e9_0001);
    moveByteImmediate(value, 0x00e9_0003);
  }

  // MSM6258を両chへ出力し、既知nibble列を復号する。
  moveByteImmediate(0x0c, 0x00e9_a005);
  moveByteImmediate(0x02, 0x00e9_2001);
  for (const value of [0x12, 0x34, 0x56, 0x78]) moveByteImmediate(value, 0x00e9_2003);

  // CZ-6BM1の出力bankへMIDI note-onを送る。
  moveByteImmediate(5, 0x00ea_fa03);
  moveByteImmediate(0x90, 0x00ea_fa0d);
  moveByteImmediate(60, 0x00ea_fa0d);
  moveByteImmediate(100, 0x00ea_fa0d);

  // 同梱診断XDFの先頭セクタをFDC経由で読み、先頭値を左上画素へ反映する。
  // READ DATA: drive/head, C, H, R, N, EOT, GPL, DTL
  for (const value of [0x06, 0, 0, 0, 1, 3, 1, 0x1b, 0xff]) {
    moveByteImmediate(value, FDC_DATA);
  }
  // move.b $e94003.l,d0 / move.b d0,$c00001.l
  program.push(0x10, 0x39, 0x00, 0xe9, 0x40, 0x03);
  program.push(0x13, 0xc0, 0x00, 0xc0, 0x00, 0x01);

  // HDD先頭blockを機種固有のSASI/MB89352窓から読む。
  if (model === "x68000") {
    moveByteImmediate(1, HDC_BASE + 7);
    moveByteImmediate(0, HDC_BASE + 3);
    for (const value of [0x08, 0, 0, 0, 1, 0]) moveByteImmediate(value, HDC_BASE + 1);
    program.push(0x10, 0x39, 0x00, 0xe9, 0x60, 0x01);
  } else {
    const spc = HDC_BASE + 0x20;
    moveByteImmediate(7, spc + 0x01); // BDID initiator 7
    moveByteImmediate(1, spc + 0x03); // SCTL interrupt enable
    moveByteImmediate(0x81, spc + 0x17); // TEMP initiator + target 0
    moveByteImmediate(0x20, spc + 0x05); // select
    moveByteImmediate(0x10, spc + 0x09); // clear selection interrupt
    moveByteImmediate(2, spc + 0x11); // command phase
    moveByteImmediate(0, spc + 0x19);
    moveByteImmediate(0, spc + 0x1b);
    moveByteImmediate(6, spc + 0x1d);
    moveByteImmediate(0x84, spc + 0x05);
    for (const value of [0x08, 0, 0, 0, 1, 0]) moveByteImmediate(value, spc + 0x15);
    moveByteImmediate(1, spc + 0x11); // data-in phase
    moveByteImmediate(0, spc + 0x19);
    moveByteImmediate(1, spc + 0x1b);
    moveByteImmediate(0, spc + 0x1d);
    moveByteImmediate(0x84, spc + 0x05);
    program.push(0x10, 0x39, 0x00, 0xe9, 0x60, 0x35);
  }
  program.push(0x13, 0xc0, 0x00, 0xc0, 0x00, 0x05);

  // CRTC raster line 1をMFP source 1へ接続する。
  moveByteImmediate(0x00, 0x00e8_0012);
  moveByteImmediate(0x01, 0x00e8_0013);
  moveByteImmediate(0x40, 0x00e8_8007); // IERA
  moveByteImmediate(0x40, 0x00e8_8013); // IMRA

  // raster IRQまでSTOPし、復帰後はSTOPへ戻る。空回りで25MHz分の命令を
  // 消費せず、周辺デバイスと走査線の実時間だけを進める。
  program.push(0x4e, 0x72, 0x20, 0x00, 0x60, 0xfa);
  rom.set(program, 0x10);
  rom[0x0900] = 0xa5; // DMA source (mapped at $fe0900)
  // raster handler: TVRAM plane 1へ合格bitを立て、割り込みをmaskして停止する。
  // これは一回限りの自己診断であり、RTE命令の適合性はCPUコーパスで別途検査する。
  rom.set([
    0x13, 0xfc, 0x00, 0x80, 0x00, 0xe2, 0x00, 0x00,
    0x4e, 0x72, 0x27, 0x00, 0x60, 0xfa,
  ], 0x0800);
  const exceptionStop = (colour: number): number[] => [
    0x33, 0xfc, (colour >>> 8) & 0xff, colour & 0xff, 0x00, 0xc0, 0x00, 0x00,
    0x4e, 0x72, 0x27, 0x00, 0x60, 0xfa,
  ];
  rom.set(exceptionStop(0x003e), 0x0820);
  rom.set(exceptionStop(0x07c0), 0x0840);
  rom.set(exceptionStop(0xf800), 0x0860);
  return rom;
}

/**
 * 診断IPLと対になる自作XDF。ROM/OSを含まず、先頭セクタの識別文字列と
 * 決定論的パターンだけでFDC・媒体パーサ・copy-on-write経路を検査する。
 */
export function createDiagnosticXdf(): Uint8Array {
  const image = new Uint8Array(77 * 2 * 8 * 1024);
  image[0] = 0xc0;
  const notice = new TextEncoder().encode(
    "wasm-68k synthetic diagnostic disk\r\nGPL-2.0-only; no third-party ROM or software.\r\n",
  );
  image.set(notice, 1);
  for (let index = 1024; index < image.length; index += 1) {
    image[index] = ((index >>> 10) ^ (index >>> 3) ^ index) & 0xff;
  }
  return image;
}

/** SASI/SCSI双方で利用する256-byte blockの自作診断HDF。 */
export function createDiagnosticHdf(): Uint8Array {
  const image = new Uint8Array(256 * 64);
  image[0] = 0x3f;
  image.set(new TextEncoder().encode("wasm-68k synthetic diagnostic HDF\r\n"), 1);
  for (let index = 256; index < image.length; index += 1) {
    image[index] = ((index >>> 8) * 17 + index) & 0xff;
  }
  return image;
}
