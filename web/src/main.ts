import init, { WebX68k } from "./wasm/x68k_wasm.js";
import { createDiagnosticHdf, createDiagnosticIpl, createDiagnosticXdf } from "./diagnostic.js";
import {
  decodeKeyMap,
  defaultKeyMap,
  encodeKeyMap,
  isKeyMap,
  PressedKeyState,
} from "./keyboard.js";

type LoadTarget = "ipl" | "cgrom" | "scsi" | `fdd${0 | 1 | 2 | 3}` | "hdd0" | "state";
// wasm-pack生成物がエディタ上で一世代古くても、Rust側の安定公開APIを型として
// 保持する。実体の存在はPages E2Eで検証する。
type Emulator = WebX68k & {
  frame_number(): bigint;
  set_volume(volume: number): void;
};

let keyMap: Record<string, number> = { ...defaultKeyMap };
const pressedKeys = new PressedKeyState();
const pressedMouseButtons = new Set<number>();

const $ = <T extends HTMLElement>(id: string): T => {
  const element = document.getElementById(id);
  if (!element) throw new Error(`required element #${id} not found`);
  return element as T;
};

const canvas = $<HTMLCanvasElement>("screen");
const status = $<HTMLParagraphElement>("status");
const backend = $<HTMLSpanElement>("backend");
const model = $<HTMLSelectElement>("model");
const picker = $<HTMLInputElement>("file-picker");
const autoPauseAfterFirstFrame = new URLSearchParams(location.search).get("autopause") === "1";
let emulator: Emulator;
let emulatorReady = false;
let loadTarget: LoadTarget = "ipl";
let audioContext: AudioContext | undefined;
let audioNode: AudioWorkletNode | undefined;
let gainNode: GainNode | undefined;
let midiOutput: MIDIOutput | undefined;
let lastGamepadButtons: boolean[] = [];
let lastGamepadAxes: number[] = [];
let activeModel = "x68000";
let uiFrames = 0;
let assetLoadInProgress = false;
const mountedNames = new Map<LoadTarget, string>();

const sharpAssets = {
  x68000: {
    ipl: { name: "IPLROM.DAT", size: 131_072, sha256: "8ead1d0f4ebb9c59a7fa118596f819e191c310442a00c56ab5ec5e9e7a189677" },
  },
  xvi: {
    ipl: { name: "IPLROMXV.DAT", size: 131_072, sha256: "743436ba571b73ba7d9e12cde2767d05f2885e1ec275fbc3cd0904994675b79a" },
  },
  x68030: {
    ipl: { name: "IPLROM30.DAT", size: 131_072, sha256: "bdba942ab9c633a3172fbf1a8579849df52c0eeb0da8a3411402f4564d035a27" },
  },
} as const;
const human302 = {
  name: "HUMAN302.XDF",
  size: 1_261_568,
  sha256: "bc814dab949f517ec3fb5b5b0e71f2adb468107ae0c431ee92ec38b30b031833",
} as const;

class MidiParser {
  private runningStatus: number | undefined;
  private message: number[] = [];
  private expected = 0;
  private sysex = false;

  push(bytes: Uint8Array): number[][] {
    const complete: number[][] = [];
    for (const byte of bytes) {
      if (byte >= 0xf8) {
        complete.push([byte]);
        continue;
      }
      if (this.sysex) {
        this.message.push(byte);
        if (byte === 0xf7) {
          // Web MIDIはsysex権限を要求しないため、SysExだけ安全に破棄する。
          this.message = [];
          this.sysex = false;
        }
        continue;
      }
      if ((byte & 0x80) !== 0) {
        this.message = [byte];
        this.expected = midiMessageLength(byte);
        this.sysex = byte === 0xf0;
        this.runningStatus = byte < 0xf0 ? byte : undefined;
        if (this.expected === 1) complete.push(this.takeMessage());
        continue;
      }
      if (this.message.length === 0 && this.runningStatus !== undefined) {
        this.message.push(this.runningStatus);
        this.expected = midiMessageLength(this.runningStatus);
      }
      if (this.message.length !== 0) {
        this.message.push(byte);
        if (this.message.length === this.expected) complete.push(this.takeMessage());
      }
    }
    return complete;
  }

  private takeMessage(): number[] {
    const message = this.message;
    this.message = [];
    return message;
  }
}

function midiMessageLength(statusByte: number): number {
  if ((statusByte >= 0x80 && statusByte <= 0xbf) ||
      (statusByte >= 0xe0 && statusByte <= 0xef) || statusByte === 0xf2) return 3;
  if ((statusByte >= 0xc0 && statusByte <= 0xdf) || statusByte === 0xf1 || statusByte === 0xf3) return 2;
  return statusByte === 0xf0 ? Number.MAX_SAFE_INTEGER : 1;
}

const midiParser = new MidiParser();

function message(text: string, error = false): void {
  status.textContent = text;
  status.classList.toggle("error", error);
}

async function fetchVerifiedSharpAsset(asset: { name: string; size: number; sha256: string }): Promise<Uint8Array> {
  const response = await fetch(`${import.meta.env.BASE_URL}sharp/${asset.name}`);
  if (!response.ok) throw new Error(`${asset.name}の取得に失敗しました (${response.status})`);
  const bytes = new Uint8Array(await response.arrayBuffer());
  if (bytes.byteLength !== asset.size) {
    throw new Error(`${asset.name}のサイズが公式配布物と一致しません`);
  }
  const digest = new Uint8Array(await crypto.subtle.digest("SHA-256", bytes));
  const actual = [...digest].map((byte) => byte.toString(16).padStart(2, "0")).join("");
  if (actual !== asset.sha256) throw new Error(`${asset.name}のSHA-256が公式配布物と一致しません`);
  return bytes;
}

async function bootOfficialHuman68k(): Promise<void> {
  if (!emulatorReady) return;
  const button = $<HTMLButtonElement>("official-software");
  const wasPaused = emulator.is_paused();
  let started = false;
  button.disabled = true;
  assetLoadInProgress = true;
  // 25MHzプロファイルのframe実行がfetch/hash検証のPromiseを飢餓させないよう、
  // 資産の検証中だけコアを停止する。
  emulator.set_paused(true);
  message("Sharp公式IPL・Human68k 3.02を検証しています…");
  try {
    const selected = model.value as keyof typeof sharpAssets;
    const [ipl, disk] = await Promise.all([
      fetchVerifiedSharpAsset(sharpAssets[selected].ipl),
      fetchVerifiedSharpAsset(human302),
    ]);
    message("公式配布物のhash検証が完了しました。媒体を接続しています…");
    if (mountedNames.has("fdd0")) exportMounted("fdd0", true);
    emulator.mount_media("floppy", 0, "xdf", disk, true);
    mountedNames.set("fdd0", human302.name);
    $<HTMLSpanElement>("fdd0-name").textContent = human302.name;
    $<HTMLInputElement>("write-protect").checked = true;
    message("公式媒体を接続しました。IPLをリセットしています…");
    emulator.load_rom("ipl", ipl);
    emulator.set_paused(autoPauseAfterFirstFrame);
    $<HTMLButtonElement>("pause").textContent = autoPauseAfterFirstFrame ? "再開" : "一時停止";
    started = true;
    canvas.dataset.machineFrame = "0";
    $<HTMLSpanElement>("ipl-name").textContent = sharpAssets[selected].ipl.name;
    const cgromLoaded = $<HTMLSpanElement>("cgrom-name").textContent !== "未読込";
    $<HTMLParagraphElement>("cgrom-hint").hidden = cgromLoaded;
    message(
      `Sharp公式IPLとHuman68k 3.02を起動しました（書込保護・非商用許諾）${
        cgromLoaded ? "" : "。文字表示にはユーザー所有のCGROMを読み込んでください"
      }`,
    );
  } finally {
    if (!started) emulator.set_paused(wasPaused);
    assetLoadInProgress = false;
    button.disabled = false;
  }
}

async function createEmulator(): Promise<void> {
  // Wasm surface生成のawait中に、破棄済みインスタンスをanimateが呼ばないようにする。
  emulatorReady = false;
  canvas.dataset.emulatorReady = "false";
  backend.textContent = "初期化中…";
  if (emulator) {
    releaseTransientInputs();
    await dbPut(`sram:${activeModel}`, emulator.export_sram()).catch(console.warn);
    for (const target of [...mountedNames.keys()]) exportMounted(target, true);
    emulator.free();
  }
  activeModel = model.value;
  emulator = await WebX68k.create(canvas, model.value) as Emulator;
  // 機種切替ではROMは新しいMachineへ引き継がれないため、表示ラベルも初期化する。
  $<HTMLSpanElement>("ipl-name").textContent = "未読込";
  $<HTMLSpanElement>("cgrom-name").textContent = "未読込";
  $<HTMLSpanElement>("scsi-name").textContent = "未読込";
  $<HTMLParagraphElement>("cgrom-hint").hidden = false;
  $<HTMLButtonElement>("pause").textContent = "一時停止";
  const savedSram = await dbGet(`sram:${activeModel}`).catch(() => undefined);
  if (savedSram) emulator.load_sram(savedSram);
  backend.textContent = `${emulator.model_name()} · ${emulator.backend_name()}`;
  emulatorReady = true;
  canvas.dataset.emulatorReady = "true";
  emulator.set_volume(Number($<HTMLInputElement>("volume").value));
  updateVideoOptions();
  fitCanvas();
  message("準備完了。IPLを読み込むか、内蔵の診断IPLを起動してください。");
}

function fitCanvas(): void {
  if (!emulator || !emulatorReady) return;
  const dpr = window.devicePixelRatio || 1;
  const rect = canvas.getBoundingClientRect();
  const width = Math.max(1, Math.round(rect.width * dpr));
  const height = Math.max(1, Math.round(rect.height * dpr));
  if (canvas.width !== width || canvas.height !== height) {
    canvas.width = width;
    canvas.height = height;
    emulator.resize(width, height);
  }
}

function updateVideoOptions(): void {
  emulator?.set_video_options(
    $<HTMLInputElement>("crt").checked,
    Number($<HTMLInputElement>("scanline").value),
    Number($<HTMLInputElement>("mask").value),
    Number($<HTMLInputElement>("curvature").value),
  );
}

function extension(name: string): string {
  return name.toLowerCase().split(".").pop() ?? "";
}

async function loadFile(file: File, target: LoadTarget): Promise<void> {
  const bytes = new Uint8Array(await file.arrayBuffer());
  if (target === "state") {
    emulator.load_state(bytes);
    message(`保存状態を復元しました: ${file.name}`);
    return;
  }
  if (target === "ipl" || target === "cgrom" || target === "scsi") {
    emulator.load_rom(target, bytes);
    $<HTMLSpanElement>(`${target}-name`).textContent = file.name;
    if (target === "cgrom") $<HTMLParagraphElement>("cgrom-hint").hidden = true;
    message(`${target.toUpperCase()}を読み込みました: ${file.name}`);
    return;
  }
  const isHdd = target === "hdd0";
  const drive = isHdd ? 0 : Number(target.at(-1));
  const format = isHdd ? "hdf" : extension(file.name);
  emulator.mount_media(
    isHdd ? "hard-disk" : "floppy",
    drive,
    format,
    bytes,
    $<HTMLInputElement>("write-protect").checked,
  );
  $<HTMLSpanElement>(`${target}-name`).textContent = file.name;
  mountedNames.set(target, file.name);
  message(`${target.toUpperCase()}へ挿入しました: ${file.name}`);
}

function mediaTarget(target: LoadTarget): { kind: "floppy" | "hard-disk"; drive: number } {
  if (target === "hdd0") return { kind: "hard-disk", drive: 0 };
  if (target.startsWith("fdd")) return { kind: "floppy", drive: Number(target.at(-1)) };
  throw new Error(`${target} is not a media drive`);
}

function exportMounted(target: LoadTarget, eject: boolean): void {
  const { kind, drive } = mediaTarget(target);
  const bytes = eject
    ? emulator.eject_media(kind, drive)
    : emulator.export_media(kind, drive);
  const original = mountedNames.get(target) ?? `${target}.img`;
  download(`changed-${original}`, bytes);
  if (eject) {
    mountedNames.delete(target);
    $<HTMLSpanElement>(`${target}-name`).textContent = "空";
    message(`${target.toUpperCase()}を排出し、変更媒体を書き出しました`);
  }
}

function inferTarget(file: File): LoadTarget {
  const lower = file.name.toLowerCase();
  if (lower.includes("cgrom")) return "cgrom";
  if (lower.includes("scsi")) return "scsi";
  if (lower.includes("ipl") || lower.endsWith(".rom")) return "ipl";
  if (lower.endsWith(".hdf")) return "hdd0";
  if (lower.endsWith(".x68state")) return "state";
  for (const drive of [0, 1, 2, 3] as const) {
    if ($<HTMLSpanElement>(`fdd${drive}-name`).textContent === "空") return `fdd${drive}`;
  }
  return "fdd0";
}

async function startAudio(): Promise<void> {
  if (!audioContext) {
    audioContext = new AudioContext({ sampleRate: 48_000, latencyHint: "interactive" });
    await audioContext.audioWorklet.addModule(`${import.meta.env.BASE_URL}audio-worklet.js`);
    audioNode = new AudioWorkletNode(audioContext, "x68k-audio", { outputChannelCount: [2] });
    gainNode = audioContext.createGain();
    gainNode.gain.value = 1;
    audioNode.connect(gainNode).connect(audioContext.destination);
  }
  await audioContext.resume();
  emulator.set_audio_enabled(true);
  $<HTMLButtonElement>("audio").textContent = "音声有効";
}

async function enableMidi(): Promise<void> {
  if (!("requestMIDIAccess" in navigator)) throw new Error("Web MIDIはこのブラウザで利用できません");
  const access = await navigator.requestMIDIAccess({ sysex: false });
  midiOutput = access.outputs.values().next().value;
  if (!midiOutput) throw new Error("MIDI出力が見つかりません");
  emulator.set_midi_enabled(true);
  $<HTMLButtonElement>("midi").textContent = midiOutput.name ?? "MIDI有効";
}

function pollGamepad(): void {
  const pad = navigator.getGamepads()[0];
  if (!pad) {
    releaseGamepad();
    return;
  }
  pad.buttons.forEach((button, index) => {
    if (lastGamepadButtons[index] !== button.pressed) {
      emulator.set_joystick_button(0, index, button.pressed);
      lastGamepadButtons[index] = button.pressed;
    }
  });
  pad.axes.slice(0, 2).forEach((axis, index) => {
    if (Math.abs((lastGamepadAxes[index] ?? 0) - axis) > 0.02) {
      emulator.set_joystick_axis(0, index, Math.round(axis * 32767));
      lastGamepadAxes[index] = axis;
    }
  });
}

function releaseGamepad(): void {
  lastGamepadButtons.forEach((pressed, index) => {
    if (pressed && emulator) emulator.set_joystick_button(0, index, false);
  });
  lastGamepadAxes.forEach((axis, index) => {
    if (axis !== 0 && emulator) emulator.set_joystick_axis(0, index, 0);
  });
  lastGamepadButtons = [];
  lastGamepadAxes = [];
}

function download(name: string, bytes: string | Uint8Array, type = "application/octet-stream"): void {
  let part: BlobPart;
  if (typeof bytes === "string") {
    part = bytes;
  } else {
    const copy = new Uint8Array(bytes.byteLength);
    copy.set(bytes);
    part = copy.buffer;
  }
  const link = document.createElement("a");
  link.href = URL.createObjectURL(new Blob([part], { type }));
  link.download = name;
  link.click();
  setTimeout(() => URL.revokeObjectURL(link.href), 0);
}

function openDb(): Promise<IDBDatabase> {
  return new Promise((resolve, reject) => {
    const request = indexedDB.open("wasm-68k", 1);
    request.onupgradeneeded = () => request.result.createObjectStore("data");
    request.onsuccess = () => resolve(request.result);
    request.onerror = () => reject(request.error);
  });
}

async function dbPut(key: string, value: Uint8Array): Promise<void> {
  const db = await openDb();
  await new Promise<void>((resolve, reject) => {
    const transaction = db.transaction("data", "readwrite");
    transaction.objectStore("data").put(value, key);
    transaction.oncomplete = () => resolve();
    transaction.onerror = () => reject(transaction.error);
  });
  db.close();
}

async function dbGet(key: string): Promise<Uint8Array | undefined> {
  const db = await openDb();
  const value = await new Promise<Uint8Array | undefined>((resolve, reject) => {
    const request = db.transaction("data").objectStore("data").get(key);
    request.onsuccess = () => resolve(request.result as Uint8Array | undefined);
    request.onerror = () => reject(request.error);
  });
  db.close();
  return value;
}

function settingsBytes(): Uint8Array {
  return new TextEncoder().encode(JSON.stringify({
    model: model.value,
    volume: $<HTMLInputElement>("volume").value,
    crt: $<HTMLInputElement>("crt").checked,
    scanline: $<HTMLInputElement>("scanline").value,
    mask: $<HTMLInputElement>("mask").value,
    curvature: $<HTMLInputElement>("curvature").value,
  }));
}

async function saveSettings(): Promise<void> {
  await dbPut("settings", settingsBytes());
}

async function restoreSettings(): Promise<void> {
  const bytes = await dbGet("settings").catch(() => undefined);
  if (bytes) {
    try {
      const settings = JSON.parse(new TextDecoder().decode(bytes)) as Partial<Record<string, unknown>>;
      if (["x68000", "xvi", "x68030"].includes(String(settings.model))) model.value = String(settings.model);
      restoreRange("volume", settings.volume);
      if (typeof settings.crt === "boolean") $<HTMLInputElement>("crt").checked = settings.crt;
      restoreRange("scanline", settings.scanline);
      restoreRange("mask", settings.mask);
      restoreRange("curvature", settings.curvature);
    } catch (error) {
      console.warn("破損した保存設定を無視しました", error);
    }
  }
  const savedMap = await dbGet("keymap").catch(() => undefined);
  if (savedMap) {
    try {
      const candidate = JSON.parse(new TextDecoder().decode(savedMap)) as unknown;
      const restored = decodeKeyMap(candidate);
      keyMap = restored.keyMap;
      if (restored.migrated) await dbPut("keymap", encodeKeyMap(keyMap));
    } catch (error) {
      console.warn("破損したキーマップを無視しました", error);
    }
  }
  $<HTMLTextAreaElement>("keymap").value = JSON.stringify(keyMap, null, 2);
}

function restoreRange(id: string, value: unknown): void {
  if (typeof value !== "string" || !Number.isFinite(Number(value))) return;
  const input = $<HTMLInputElement>(id);
  const numeric = Number(value);
  if (numeric >= Number(input.min) && numeric <= Number(input.max)) input.value = value;
}

function keyDown(event: KeyboardEvent): void {
  if (document.activeElement !== canvas) return;
  const scancode = pressedKeys.press(event.code, keyMap);
  if (scancode === undefined) return;
  event.preventDefault();
  // 実機キーボードのtypematic相当としてブラウザのrepeat makeも配送する。
  emulator.set_key(scancode, true);
}

function keyUp(event: KeyboardEvent): void {
  const released = pressedKeys.release(event.code);
  if (released === undefined) return;
  event.preventDefault();
  if (released.sendBreak) emulator.set_key(released.scancode, false);
}

function releaseAllKeys(): void {
  const scancodes = pressedKeys.drain();
  if (emulator) for (const scancode of scancodes) emulator.set_key(scancode, false);
}

function releaseTransientInputs(): void {
  releaseAllKeys();
  if (emulator) {
    for (const button of pressedMouseButtons) emulator.set_mouse_button(button, false);
  }
  pressedMouseButtons.clear();
  releaseGamepad();
}

function wireUi(): void {
  new ResizeObserver(fitCanvas).observe(canvas);
  model.addEventListener("change", () => {
    void saveSettings().catch(console.warn);
    void createEmulator().catch(showError);
  });
  $<HTMLButtonElement>("reset").onclick = () => { emulator.reset(); message("リセットしました"); };
  $<HTMLButtonElement>("diagnostic-rom").onclick = () => {
    if (!emulatorReady) return;
    if (mountedNames.has("fdd0")) exportMounted("fdd0", true);
    if (mountedNames.has("hdd0")) exportMounted("hdd0", true);
    emulator.mount_media("floppy", 0, "xdf", createDiagnosticXdf(), true);
    emulator.mount_media("hard-disk", 0, "hdf", createDiagnosticHdf(), true);
    mountedNames.set("fdd0", "builtin-diagnostic.xdf");
    mountedNames.set("hdd0", "builtin-diagnostic.hdf");
    $<HTMLSpanElement>("fdd0-name").textContent = "builtin-diagnostic.xdf";
    $<HTMLSpanElement>("hdd0-name").textContent = "builtin-diagnostic.hdf";
    if (autoPauseAfterFirstFrame) {
      emulator.set_paused(false);
      $<HTMLButtonElement>("pause").textContent = "一時停止";
    }
    emulator.load_rom("ipl", createDiagnosticIpl(model.value as "x68000" | "xvi" | "x68030"));
    canvas.dataset.machineFrame = "0";
    $<HTMLSpanElement>("ipl-name").textContent = "合成診断IPL";
    message("再配布可能な合成診断IPL・XDFを起動しました");
  };
  $<HTMLButtonElement>("official-software").onclick = () => void bootOfficialHuman68k().catch(showError);
  $<HTMLButtonElement>("pause").onclick = (event) => {
    emulator.set_paused(!emulator.is_paused());
    (event.currentTarget as HTMLButtonElement).textContent = emulator.is_paused() ? "再開" : "一時停止";
  };
  $<HTMLButtonElement>("fullscreen").onclick = () => void canvas.requestFullscreen();
  document.querySelectorAll<HTMLButtonElement>("button[data-load]").forEach((button) => {
    button.onclick = () => { loadTarget = button.dataset.load as LoadTarget; picker.accept = loadTarget === "hdd0" ? ".hdf" : ""; picker.click(); };
  });
  document.querySelectorAll<HTMLButtonElement>("button[data-export]").forEach((button) => {
    button.onclick = () => {
      try { exportMounted(button.dataset.export as LoadTarget, false); } catch (error) { showError(error); }
    };
  });
  document.querySelectorAll<HTMLButtonElement>("button[data-eject]").forEach((button) => {
    button.onclick = () => {
      try { exportMounted(button.dataset.eject as LoadTarget, true); } catch (error) { showError(error); }
    };
  });
  picker.onchange = () => {
    const file = picker.files?.[0];
    if (file) void loadFile(file, loadTarget).catch(showError);
    picker.value = "";
  };
  for (const eventName of ["dragenter", "dragover"]) canvas.addEventListener(eventName, (event) => { event.preventDefault(); canvas.classList.add("drop"); });
  for (const eventName of ["dragleave", "drop"]) canvas.addEventListener(eventName, (event) => { event.preventDefault(); canvas.classList.remove("drop"); });
  canvas.addEventListener("drop", (event) => {
    const files = [...(event.dataTransfer?.files ?? [])];
    void files.reduce((promise, file) => promise.then(() => loadFile(file, inferTarget(file))), Promise.resolve()).catch(showError);
  });
  canvas.addEventListener("mousemove", (event) => { if (document.pointerLockElement === canvas) emulator.set_mouse_delta(event.movementX, event.movementY); });
  canvas.addEventListener("mousedown", (event) => {
    canvas.focus();
    pressedMouseButtons.add(event.button);
    emulator.set_mouse_button(event.button, true);
  });
  window.addEventListener("mouseup", (event) => {
    if (!pressedMouseButtons.delete(event.button)) return;
    emulator.set_mouse_button(event.button, false);
  });
  canvas.addEventListener("contextmenu", (event) => event.preventDefault());
  canvas.addEventListener("dblclick", () => void canvas.requestPointerLock());
  window.addEventListener("keydown", keyDown);
  window.addEventListener("keyup", keyUp);
  window.addEventListener("blur", releaseTransientInputs);
  document.addEventListener("visibilitychange", () => {
    if (document.visibilityState === "hidden") releaseTransientInputs();
  });
  $<HTMLButtonElement>("audio").onclick = () => void startAudio().catch(showError);
  $<HTMLButtonElement>("midi").onclick = () => void enableMidi().catch(showError);
  $<HTMLInputElement>("volume").oninput = (event) => {
    emulator.set_volume(Number((event.target as HTMLInputElement).value));
    void saveSettings().catch(console.warn);
  };
  for (const id of ["crt", "scanline", "mask", "curvature"]) $<HTMLInputElement>(id).oninput = () => { updateVideoOptions(); void saveSettings().catch(console.warn); };
  $<HTMLButtonElement>("apply-keymap").onclick = () => {
    try {
      const value = JSON.parse($<HTMLTextAreaElement>("keymap").value) as Record<string, number>;
      if (!isKeyMap(value)) throw new Error("キーマップはキー名と0〜127の整数scancodeで指定してください");
      releaseAllKeys();
      keyMap = value;
      void dbPut("keymap", encodeKeyMap(value)).then(() => message("キーマップを保存しました")).catch(showError);
    } catch (error) { showError(error); }
  };
  $<HTMLButtonElement>("reset-keymap").onclick = () => {
    releaseAllKeys();
    keyMap = { ...defaultKeyMap };
    $<HTMLTextAreaElement>("keymap").value = JSON.stringify(keyMap, null, 2);
    void dbPut("keymap", encodeKeyMap(keyMap))
      .then(() => message("X68000既定キーマップへ戻しました"))
      .catch(showError);
  };
  const stateSlot = () => $<HTMLSelectElement>("state-slot").value;
  $<HTMLButtonElement>("save-state").onclick = () => void dbPut(`state:${model.value}:${stateSlot()}`, emulator.save_state()).then(() => message(`保存状態スロット${stateSlot()}へ保存しました`)).catch(showError);
  $<HTMLButtonElement>("load-state").onclick = () => void dbGet(`state:${model.value}:${stateSlot()}`).then((state) => { if (!state) throw new Error("保存状態がありません"); emulator.load_state(state); message(`保存状態スロット${stateSlot()}を復元しました`); }).catch(showError);
  $<HTMLButtonElement>("export-state").onclick = () => download(`wasm-68k-${model.value}.x68state`, emulator.save_state());
  $<HTMLButtonElement>("import-state").onclick = () => { loadTarget = "state"; picker.accept = ".x68state"; picker.click(); };
  $<HTMLButtonElement>("diagnostics").onclick = () => download("wasm-68k-diagnostics.json", emulator.diagnostics(), "application/json");
}

function showError(error: unknown): void {
  console.error(error);
  message(String(error), true);
}

function animate(timestamp: number): void {
  if (!emulatorReady) {
    requestAnimationFrame(animate);
    return;
  }
  if (assetLoadInProgress) {
    requestAnimationFrame(animate);
    return;
  }
  emulator.frame(timestamp);
  canvas.dataset.machineFrame = String(emulator.frame_number());
  if (autoPauseAfterFirstFrame && emulator.frame_number() >= 1 && !emulator.is_paused()) {
    emulator.set_paused(true);
    $<HTMLButtonElement>("pause").textContent = "再開";
  }
  pollGamepad();
  const samples = emulator.drain_audio();
  if (samples.length && audioNode) audioNode.port.postMessage(samples, [samples.buffer]);
  const midi = emulator.drain_midi();
  if (midi.length && midiOutput) {
    for (const messageBytes of midiParser.push(midi)) {
      try {
        midiOutput.send(messageBytes);
      } catch (error) {
        // デバイス切断や権限変更でもエミュレーション自体は継続する。
        console.warn("MIDI output failed", error);
      }
    }
  }
  uiFrames += 1;
  if (uiFrames % 300 === 0) {
    void dbPut(`sram:${activeModel}`, emulator.export_sram()).catch(console.warn);
  }
  requestAnimationFrame(animate);
}

async function main(): Promise<void> {
  await init();
  await restoreSettings();
  const requestedModel = new URLSearchParams(location.search).get("model");
  if (requestedModel && ["x68000", "xvi", "x68030"].includes(requestedModel)) {
    model.value = requestedModel;
  }
  wireUi();
  await createEmulator();
  requestAnimationFrame(animate);
}

void main().catch(showError);
