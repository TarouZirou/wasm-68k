import { chromium, expect, test, type Page } from "@playwright/test";

interface DiagnosticReport {
  frame: number;
  cpu_pc: number;
  cpu_stopped: boolean;
  first_bus_fault: number | null;
  last_bus_fault: number | null;
  bus_fault_count: number;
  fdc_commands: number;
  fdc_sector_reads: number;
  fdc_command: number;
  fdc_status: number;
  fdc_output: number;
  fdc_st0: number;
  fdc_st1: number;
  fdc_st2: number;
  adpcm_writes: number;
  adpcm_starts: number;
  adpcm_dma_transfers: number;
  adpcm_playing: boolean;
  adpcm_buffered: number;
  mouse_buttons: number;
  frame_sha256: string;
  audio_peak: number;
  content: Array<{ slot: string; sha256: string }>;
}

/** `downloadDiagnostics` が想定する振る舞いを満たし、回帰がないことを検証する。 */
async function downloadDiagnostics(page: Page): Promise<DiagnosticReport> {
  const downloadPromise = page.waitForEvent("download");
  await page.locator("#diagnostics").click();
  const reportStream = await (await downloadPromise).createReadStream();
  let reportText = "";
  for await (const chunk of reportStream) reportText += chunk.toString();
  return JSON.parse(reportText) as DiagnosticReport;
}

/** `readDownload` が想定する振る舞いを満たし、回帰がないことを検証する。 */
async function readDownload(page: Page, selector: string): Promise<Buffer> {
  const downloadPromise = page.waitForEvent("download");
  await page.locator(selector).click();
  const stream = await (await downloadPromise).createReadStream();
  const chunks: Buffer[] = [];
  for await (const chunk of stream) chunks.push(Buffer.from(chunk));
  return Buffer.concat(chunks);
}

test("Pages base path loads Wasm and AudioWorklet", async ({ page }) => {
  const loaded = new Set<string>();
  page.on("response", (response) => {
    if (response.ok()) loaded.add(new URL(response.url()).pathname);
  });

  await page.goto("/wasm-68k/?autopause=1");
  await expect(page.locator("#status")).toContainText("準備完了");
  await expect(page.locator("#backend")).not.toBeEmpty();
  expect([...loaded].some((path) => path.startsWith("/wasm-68k/assets/") && path.endsWith(".wasm"))).toBeTruthy();

  // PCM生成はWeb Audioを開始するユーザー操作後だけ有効になる。
  await page.locator("#audio").click();
  await expect(page.locator("#audio")).toHaveText("音声有効");
  await page.locator("#diagnostic-rom").click();
  await expect(page.locator("#ipl-name")).toHaveText("合成診断IPL");
  await expect(page.locator("#fdd0-name")).toHaveText("builtin-diagnostic.xdf");
  await expect(page.locator("#hdd0-name")).toHaveText("builtin-diagnostic.hdf");
  await expect(page.locator("#screen")).toHaveAttribute("data-machine-frame", /^[1-9]\d*$/);
  const report = await downloadDiagnostics(page);
  expect(report.frame).toBeGreaterThan(0);
  expect(report.cpu_pc).toBe(0x00fe_080c);
  expect(report.cpu_stopped).toBeTruthy();
  expect(report.frame_sha256).toBe("1b7fb3ae09846d111a73750709fa8ebd0c3a0421f500ecbec6034c839cafa9eb");
  expect(report.audio_peak).toBeGreaterThan(0);
  expect(report.content.map(({ slot }) => slot).sort()).toEqual(["fdd:0", "hdd:0", "rom:ipl"]);
  expect(report.content.every(({ sha256 }) => /^[0-9a-f]{64}$/.test(sha256))).toBeTruthy();
  await expect(page.locator("#pause")).toHaveText("再開");
  // AudioWorkletGlobalScopeの取得はpageのresponseイベントへ公開されない環境が
  // あるため、addModule完了後にだけ変わるUIと同じbaseの実資産を検証する。
  await expect(page.locator("#audio")).toHaveText("音声有効");
  const worklet = await page.request.get("/wasm-68k/audio-worklet.js");
  expect(worklet.ok()).toBeTruthy();
  expect(await worklet.text()).toContain('registerProcessor("x68k-audio"');
});

test("root-relative deployment mistakes are absent", async ({ page }) => {
  const failed: string[] = [];
  page.on("response", (response) => {
    if (response.status() >= 400) failed.push(response.url());
  });
  await page.goto("/wasm-68k/");
  await expect(page.locator("#status")).toContainText("準備完了");
  expect(failed).toEqual([]);
});

test("WebGL2 fallback initializes when WebGPU is unavailable", async ({ page }) => {
  await page.addInitScript(() => {
    Object.defineProperty(Navigator.prototype, "gpu", {
      configurable: true,
      get: () => undefined,
    });
  });
  await page.goto("/wasm-68k/");
  await expect(page.locator("#status")).toContainText("準備完了");
  await expect(page.locator("#backend")).toContainText("webgl2");
});

test("WebGPU and WebGL2 produce the same diagnostic pixels", async ({ page: comparisonPage }, testInfo) => {
  test.setTimeout(120_000);
  const origin = String(testInfo.project.use.baseURL);

  /** 指定backendで診断IPLを1frame描画し、compositor確定後のPNGを返す。 */
  const capture = async (webgpu: boolean) => {
    const browser = await chromium.launch({
      headless: true,
      args: webgpu
        ? ["--enable-unsafe-webgpu", "--enable-features=Vulkan", "--use-angle=vulkan", "--disable-vulkan-surface"]
        : ["--use-angle=swiftshader", "--enable-unsafe-swiftshader"],
    });
    try {
      const context = await browser.newContext();
      const page = await context.newPage();
      if (!webgpu) {
        await page.addInitScript(() => {
          Object.defineProperty(Navigator.prototype, "gpu", {
            configurable: true,
            get: () => undefined,
          });
        });
      }
      await page.goto(`${origin}/wasm-68k/?autopause=1`);
      await expect(page.locator("#status")).toContainText("準備完了");
      await expect(page.locator("#backend")).toContainText(webgpu ? "webgpu" : "webgl2");
      await page.locator("#diagnostic-rom").click();
      await expect(page.locator("#screen")).toHaveAttribute("data-machine-frame", /^[1-9]\d*$/);
      // WebGPU canvasはpresent後にbacking storeを破棄できるため、browser compositor
      // が確定したlocator screenshotを通常画像へdecodeして比較する。
      return (await page.locator("#screen").screenshot()).toString("base64");
    } finally {
      await browser.close();
    }
  };

  const [gpu, gl] = await Promise.all([capture(true), capture(false)]);
  const comparison = await comparisonPage.evaluate(async ([gpuPng, glPng]: string[]) => {
    /** PNGをRGBAへ復号し、backend間の全画素比較に使う。 */
    const decode = async (base64: string) => {
      const image = new Image();
      image.src = `data:image/png;base64,${base64}`;
      await image.decode();
      const canvas = document.createElement("canvas");
      canvas.width = image.naturalWidth;
      canvas.height = image.naturalHeight;
      const context = canvas.getContext("2d", { willReadFrequently: true });
      if (!context) throw new Error("2D canvas is unavailable");
      context.drawImage(image, 0, 0);
      return {
        width: canvas.width,
        height: canvas.height,
        rgba: context.getImageData(0, 0, canvas.width, canvas.height).data,
      };
    };
    const [gpuImage, glImage] = await Promise.all([decode(gpuPng), decode(glPng)]);
    let gpuNonBlack = 0;
    let glNonBlack = 0;
    let differingChannels = 0;
    let maximumDifference = 0;
    for (let offset = 0; offset < gpuImage.rgba.length; offset += 4) {
      if (gpuImage.rgba[offset] || gpuImage.rgba[offset + 1] || gpuImage.rgba[offset + 2]) gpuNonBlack += 1;
      if (glImage.rgba[offset] || glImage.rgba[offset + 1] || glImage.rgba[offset + 2]) glNonBlack += 1;
      for (let channel = 0; channel < 3; channel += 1) {
        const difference = Math.abs(gpuImage.rgba[offset + channel] - glImage.rgba[offset + channel]);
        if (difference) differingChannels += 1;
        maximumDifference = Math.max(maximumDifference, difference);
      }
    }
    return {
      gpuSize: [gpuImage.width, gpuImage.height],
      glSize: [glImage.width, glImage.height],
      gpuNonBlack,
      glNonBlack,
      differingChannels,
      maximumDifference,
    };
  }, [gpu, gl]);
  expect(comparison.glSize).toEqual(comparison.gpuSize);
  expect(comparison.gpuNonBlack).toBeGreaterThan(0);
  // compositor境界の丸め差だけを許し、shader由来の面・色ずれは検出する。
  expect(Math.abs(comparison.glNonBlack - comparison.gpuNonBlack)).toBeLessThan(100);
  expect(comparison.differingChannels).toBeLessThan(100);
  expect(comparison.maximumDifference).toBeLessThanOrEqual(64);
});

test("settings persist and display scaling stays pixel-exact", async ({ page }) => {
  await page.goto("/wasm-68k/");
  await expect(page.locator("#status")).toContainText("準備完了");
  await page.locator("#volume").fill("0.31");
  await page.locator("#mask").fill("0.42");
  await page.locator("#crt").check();
  await page.locator("#gamepad-port").selectOption({ value: "1" });
  await expect(page.locator("#gamepad-port")).toHaveValue("1");
  await page.locator("#gamepad-deadzone").fill("0.35");
  await page.locator("#gamepad-buttons").fill("2,3,0,1");
  await expect.poll(async () => page.evaluate(async () => {
    const db = await new Promise<IDBDatabase>((resolve, reject) => {
      const request = indexedDB.open("wasm-68k", 1);
      request.onsuccess = () => resolve(request.result);
      request.onerror = () => reject(request.error);
    });
    const value = await new Promise<Uint8Array | undefined>((resolve, reject) => {
      const request = db.transaction("data").objectStore("data").get("settings");
      request.onsuccess = () => resolve(request.result as Uint8Array | undefined);
      request.onerror = () => reject(request.error);
    });
    db.close();
    return value
      ? JSON.parse(new TextDecoder().decode(value)) as { gamepadPort?: string; gamepadButtons?: string }
      : {};
  })).toMatchObject({ gamepadPort: "1", gamepadButtons: "2,3,0,1" });

  await page.reload();
  await expect(page.locator("#status")).toContainText("準備完了");
  await expect(page.locator("#volume")).toHaveValue("0.31");
  await expect(page.locator("#mask")).toHaveValue("0.42");
  await expect(page.locator("#crt")).toBeChecked();
  await expect(page.locator("#gamepad-port")).toHaveValue("1");
  await expect(page.locator("#gamepad-deadzone")).toHaveValue("0.35");
  await expect(page.locator("#gamepad-buttons")).toHaveValue("2,3,0,1");

  await page.locator("#display-size").selectOption("native");
  await expect.poll(() => page.locator("#screen").evaluate((canvas: HTMLCanvasElement) => {
    const rect = canvas.getBoundingClientRect();
    return [rect.width, rect.height, getComputedStyle(canvas).imageRendering];
  })).toEqual([768, 512, "pixelated"]);

  await page.locator("#display-size").selectOption("auto");
  await page.setViewportSize({ width: 2200, height: 1200 });
  await expect.poll(() => page.locator("#screen").evaluate((canvas: HTMLCanvasElement) => {
    const rect = canvas.getBoundingClientRect();
    return [rect.width, rect.height];
  })).toEqual([1536, 1024]);

  await page.locator("#display-size").selectOption("768x512");
  await expect.poll(() => page.locator("#screen").evaluate((canvas: HTMLCanvasElement) => [canvas.width, canvas.height]))
    .toEqual([768, 512]);
});

test("ROMs persist locally, while 2HD eject never downloads automatically", async ({ page }) => {
  await page.goto("/wasm-68k/");
  await expect(page.locator("#status")).toContainText("準備完了");
  await page.locator('button[data-load="cgrom"]').click();
  await page.locator("#file-picker").setInputFiles({
    name: "CGROM.DAT",
    mimeType: "application/octet-stream",
    buffer: Buffer.alloc(0x0c_0000),
  });
  await expect(page.locator("#status")).toContainText("次回用に保存しました");

  await page.reload();
  await expect(page.locator("#status")).toContainText("保存ROMを復元しました");
  await expect(page.locator("#cgrom-name")).toHaveText("CGROM.DAT");

  let downloads = 0;
  page.on("download", () => { downloads += 1; });
  await page.locator('button[data-load="fdd0"]').click();
  await page.locator("#file-picker").setInputFiles({
    name: "raw-disk.2HD",
    mimeType: "application/octet-stream",
    buffer: Buffer.alloc(1_261_568),
  });
  await expect(page.locator("#fdd0-name")).toHaveText("raw-disk.2HD");
  await page.locator('button[data-eject="fdd0"]').click();
  await expect(page.locator("#fdd0-name")).toHaveText("空");
  expect(downloads).toBe(0);

  await page.locator("#clear-roms").click();
  await expect(page.locator("#status")).toContainText("保存IPL/SCSI ROMと共通CGROMをすべて消去しました");
  await page.reload();
  await expect(page.locator("#cgrom-name")).toHaveText("未読込");
});

test("X68000 keyboard map replaces legacy PC scan codes", async ({ page }) => {
  await page.goto("/wasm-68k/");
  await expect(page.locator("#status")).toContainText("準備完了");
  const keyMap = JSON.parse(await page.locator("#keymap").inputValue()) as Record<string, number>;
  expect(keyMap).toMatchObject({
    Enter: 0x1d,
    KeyZ: 0x2a,
    KeyX: 0x2b,
    Space: 0x35,
    ArrowLeft: 0x3b,
    ArrowUp: 0x3c,
    ArrowRight: 0x3d,
    ArrowDown: 0x3e,
    F1: 0x63,
    ShiftLeft: 0x70,
    ControlLeft: 0x71,
  });

  // 旧版が保存したPC/ATコード表を再現し、reload時の自動移行を確認する。
  await page.evaluate(async () => {
    const db = await new Promise<IDBDatabase>((resolve, reject) => {
      const request = indexedDB.open("wasm-68k", 1);
      request.onsuccess = () => resolve(request.result);
      request.onerror = () => reject(request.error);
    });
    await new Promise<void>((resolve, reject) => {
      const request = db.transaction("data", "readwrite").objectStore("data").put(
        new TextEncoder().encode(JSON.stringify({ Enter: 0x1c, Space: 0x39, ArrowUp: 0x48, F1: 0x3b })),
        "keymap",
      );
      request.onsuccess = () => resolve();
      request.onerror = () => reject(request.error);
    });
    db.close();
  });
  await page.reload();
  await expect(page.locator("#status")).toContainText("準備完了");
  const migrated = JSON.parse(await page.locator("#keymap").inputValue()) as Record<string, number>;
  expect(migrated.Enter).toBe(0x1d);
  expect(migrated.ArrowUp).toBe(0x3c);
  expect(migrated.F1).toBe(0x63);
});

test("browser right button maps to the X68000 right button", async ({ page }) => {
  await page.goto("/wasm-68k/?autopause=1");
  await expect(page.locator("#status")).toContainText("準備完了");
  const canvas = page.locator("#screen");

  await canvas.dispatchEvent("mousedown", { button: 2 });
  expect((await downloadDiagnostics(page)).mouse_buttons).toBe(0b010);

  await page.evaluate(() => window.dispatchEvent(new MouseEvent("mouseup", { button: 2 })));
  expect((await downloadDiagnostics(page)).mouse_buttons).toBe(0);
});

test("drag-and-drop mounts HDF and X68S state round-trips", async ({ page }) => {
  await page.goto("/wasm-68k/?autopause=1");
  await expect(page.locator("#status")).toContainText("準備完了");
  const transfer = await page.evaluateHandle(() => {
    const data = new DataTransfer();
    data.items.add(new File([new Uint8Array(1024)], "drop-test.hdf", {
      type: "application/octet-stream",
    }));
    return data;
  });
  await page.locator("#screen").dispatchEvent("drop", { dataTransfer: transfer });
  await expect(page.locator("#hdd0-name")).toHaveText("drop-test.hdf");

  await page.locator("#diagnostic-rom").click();
  await expect(page.locator("#ipl-name")).toHaveText("合成診断IPL");
  const state = await readDownload(page, "#export-state");
  expect(state.subarray(0, 4).toString("ascii")).toBe("X68S");
  expect(state.readUInt16LE(4)).toBe(10);

  await page.locator("#import-state").click();
  await page.locator("#file-picker").setInputFiles({
    name: "roundtrip.x68state",
    mimeType: "application/octet-stream",
    buffer: state,
  });
  await expect(page.locator("#status")).toContainText("保存状態を復元しました");

  await page.locator("#save-state").click();
  await expect(page.locator("#status")).toContainText("保存状態スロット0へ保存しました");
  await page.locator("#load-state").click();
  await expect(page.locator("#status")).toContainText("保存状態スロット0を復元しました");
});

test("invalid ROM and denied MIDI permission do not stop emulation", async ({ page }) => {
  await page.addInitScript(() => {
    Object.defineProperty(navigator, "requestMIDIAccess", {
      configurable: true,
      value: () => Promise.reject(new DOMException("permission denied", "NotAllowedError")),
    });
  });
  await page.goto("/wasm-68k/");
  await expect(page.locator("#status")).toContainText("準備完了");
  await page.locator('button[data-load="ipl"]').click();
  await page.locator("#file-picker").setInputFiles({
    name: "bad-ipl.rom",
    mimeType: "application/octet-stream",
    buffer: Buffer.alloc(17),
  });
  await expect(page.locator("#status")).toHaveClass(/error/);
  await expect(page.locator("#status")).toContainText("invalid Ipl ROM size");

  await expect(page.locator("#screen")).toHaveAttribute("data-machine-frame", /^[1-9]\d*$/);
  const before = Number(await page.locator("#screen").getAttribute("data-machine-frame"));
  await page.locator("#midi").evaluate((button: HTMLButtonElement) => button.click());
  await expect(page.locator("#status")).toHaveClass(/error/);
  await expect.poll(async () => Number(await page.locator("#screen").getAttribute("data-machine-frame")))
    .toBeGreaterThan(before);
});

test("rejects an internal SCSI ROM on an X68000 profile", async ({ page }) => {
  await page.goto("/wasm-68k/?model=x68000");
  await expect(page.locator("#status")).toContainText("準備完了");
  await page.locator('button[data-load="scsi"]').click();

  // SCSIINROM.DAT carries an FCxxxx reset vector (X68030 internal SCSI).
  // Keep the fixture synthetic so no copyrighted ROM is put in the test tree.
  const rom = Buffer.alloc(8192);
  rom[0] = 0x00;
  rom[1] = 0xfc;
  rom[2] = 0x00;
  rom[3] = 0x68;
  await page.locator("#file-picker").setInputFiles({
    name: "SCSIINROM.DAT",
    mimeType: "application/octet-stream",
    buffer: rom,
  });
  await expect(page.locator("#status")).toHaveClass(/error/);
  await expect(page.locator("#status")).toContainText("not compatible with X68000");
  await expect(page.locator("#scsi-name")).toHaveText("未読込");
  await expect.poll(async () => Number(await page.locator("#screen").getAttribute("data-machine-frame")))
    .toBeGreaterThan(0);
});

for (const [model, iplName, iplHash] of [
  ["x68000", "IPLROM.DAT", "8ead1d0f4ebb9c59a7fa118596f819e191c310442a00c56ab5ec5e9e7a189677"],
  ["xvi", "IPLROMXV.DAT", "743436ba571b73ba7d9e12cde2767d05f2885e1ec275fbc3cd0904994675b79a"],
  ["x68030", "IPLROM30.DAT", "bdba942ab9c633a3172fbf1a8579849df52c0eeb0da8a3411402f4564d035a27"],
] as const) {
  test(`${model} Sharp assets are hash-verified and mounted read-only`, async ({ page }) => {
    test.setTimeout(120_000);
    await page.goto(`/wasm-68k/?model=${model}&autopause=1`);
    await expect(page.locator("#status")).toContainText("準備完了");
    await expect(page.locator("#pause")).toHaveText("再開");
    await page.locator("#official-software").click();
    await expect(page.locator("#status")).toContainText("公式IPLとHuman68k 3.02を起動しました");
    await expect(page.locator("#ipl-name")).toHaveText(iplName);
    await expect(page.locator("#fdd0-name")).toHaveText("HUMAN302.XDF");
    await expect(page.locator("#write-protect")).toBeChecked();
    const report = await downloadDiagnostics(page);
    expect(report.cpu_pc).toBeGreaterThanOrEqual(0x00fe_0000);
    expect(report.cpu_pc).toBeLessThanOrEqual(0x00ff_ffff);
    expect(report.last_bus_fault).toBeNull();
    expect(report.first_bus_fault).toBeNull();
    expect(report.bus_fault_count).toBe(0);
    expect(report.fdc_st0).toBeGreaterThanOrEqual(0);
    expect(report.fdc_st1).toBeGreaterThanOrEqual(0);
    expect(report.fdc_st2).toBeGreaterThanOrEqual(0);
    expect(report.content).toEqual(expect.arrayContaining([
      { slot: "rom:ipl", sha256: iplHash },
      { slot: "fdd:0", sha256: "bc814dab949f517ec3fb5b5b0e71f2adb468107ae0c431ee92ec38b30b031833" },
    ]));
  });
}

for (const [value, name] of [
  ["x68000", "X68000"],
  ["xvi", "X68000 XVI"],
  ["x68030", "X68030"],
] as const) {
  test(`${name} runs the public diagnostic`, async ({ page }) => {
    test.setTimeout(120_000);
    await page.goto(`/wasm-68k/?model=${value}&autopause=1`);
    await expect(page.locator("#status")).toContainText("準備完了");
    await expect(page.locator("#screen")).toHaveAttribute("data-emulator-ready", "true");
    await expect(page.locator("#backend")).toContainText(name);
    await page.locator("#audio").click();
    await expect(page.locator("#audio")).toHaveText("音声有効");
    await page.locator("#diagnostic-rom").click();
    await expect(page.locator("#fdd0-name")).toHaveText("builtin-diagnostic.xdf");
    await expect(page.locator("#hdd0-name")).toHaveText("builtin-diagnostic.hdf");
    await expect(page.locator("#screen")).toHaveAttribute("data-machine-frame", /^[1-9]\d*$/);
    const report = await downloadDiagnostics(page);
    expect(report.frame).toBeGreaterThan(0);
    expect(report.cpu_pc).toBe(0x00fe_080c);
    expect(report.cpu_stopped).toBeTruthy();
    expect(report.audio_peak).toBeGreaterThan(0);
    await expect(page.locator("#pause")).toHaveText("再開");
  });
}
