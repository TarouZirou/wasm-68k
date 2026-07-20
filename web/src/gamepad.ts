export interface JoystickSink {
  /** 指定したPPIポートのゲームパッドボタン状態をエミュレータへ通知する。 */
  set_joystick_button(port: number, button: number, pressed: boolean): void;
  /** 指定したPPIポートのアナログ軸を符号付き値としてエミュレータへ通知する。 */
  set_joystick_axis(port: number, axis: number, value: number): void;
}

export interface GamepadSettings {
  index: "auto" | string;
  port: number;
  deadzone: number;
  buttons: string;
}

/** Web Gamepadの状態差分をX68000 PPIの2ポートへ配送する。 */
export class GamepadController {
  private buttons: boolean[] = [];
  private axes: number[] = [];
  private port = 0;
  private source = "";

  /** 現在の状態を調べ、次の処理時点または利用可能な結果を返す。 */
  poll(pads: readonly (Gamepad | null)[], settings: GamepadSettings, sink: JoystickSink): string | undefined {
    const pad = settings.index === "auto"
      ? pads.find((candidate): candidate is Gamepad => candidate !== null)
      : pads[Number(settings.index)] ?? undefined;
    if (!pad) {
      this.release(sink);
      return undefined;
    }

    const deadzone = clamp(settings.deadzone, 0, 0.9);
    const buttonMap = parseGamepadButtons(settings.buttons);
    const source = `${pad.index}:${pad.id}:${settings.port}:${deadzone}:${buttonMap.join(",")}`;
    if (this.source && source !== this.source) this.release(sink);
    this.source = source;
    this.port = settings.port;

    buttonMap.forEach((physicalIndex, emulatedButton) => {
      const pressed = pad.buttons[physicalIndex]?.pressed ?? false;
      if (this.buttons[emulatedButton] !== pressed) {
        sink.set_joystick_button(this.port, emulatedButton, pressed);
        this.buttons[emulatedButton] = pressed;
      }
    });
    pad.axes.slice(0, 2).forEach((axis, index) => {
      const normalized = normalizeAxis(axis, deadzone);
      if (Math.abs((this.axes[index] ?? 0) - normalized) > 0.02) {
        sink.set_joystick_axis(this.port, index, Math.round(normalized * 32767));
        this.axes[index] = normalized;
      }
    });
    return `${pad.index}: ${pad.id}`;
  }

  /** 入力イベントを処理し、対応するエミュレータ状態と外部出力を更新する。 */
  release(sink?: JoystickSink): void {
    if (sink) {
      this.buttons.forEach((pressed, index) => {
        if (pressed) sink.set_joystick_button(this.port, index, false);
      });
      this.axes.forEach((axis, index) => {
        if (axis !== 0) sink.set_joystick_axis(this.port, index, 0);
      });
    }
    this.buttons = [];
    this.axes = [];
    this.source = "";
  }
}

/** 入力を解析し、後続処理で利用できる正規化済みの結果を返す。 */
export function parseGamepadButtons(value: string): number[] {
  const parsed = value.split(",").map((item) => Number(item.trim()));
  return parsed.length === 4 && parsed.every((item) => Number.isInteger(item) && item >= 0)
    ? parsed
    : [0, 1, 2, 3];
}

/** 入力値を対象形式へ変換し、有効範囲に収めた結果を返す。 */
function normalizeAxis(value: number, deadzone: number): number {
  if (Math.abs(value) <= deadzone) return 0;
  return Math.sign(value) * Math.min(1, (Math.abs(value) - deadzone) / (1 - deadzone));
}

/** 入力値を対象形式へ変換し、有効範囲に収めた結果を返す。 */
function clamp(value: number, minimum: number, maximum: number): number {
  return Number.isFinite(value) ? Math.min(maximum, Math.max(minimum, value)) : minimum;
}
