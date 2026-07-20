class X68kAudioProcessor extends AudioWorkletProcessor {
  /** PCMキューを初期化し、MessagePortから届くステレオブロックの受信処理を登録する。 */
  constructor() {
    super();
    this.queue = [];
    this.head = 0;
    this.offset = 0;
    this.queuedFrames = 0;
    this.playing = false;
    // メインスレッドから届いたPCMまたはリセット要求を音声スレッドのキューへ反映する。
    this.port.onmessage = (event) => {
      if (event.data?.type === "reset") {
        this.resetQueue();
        return;
      }
      if (!(event.data instanceof Float32Array) || event.data.length < 2) return;
      this.queue.push(event.data);
      this.queuedFrames += Math.floor(event.data.length / 2);
      // AudioContext停止中に古い音が無制限に溜まった場合は、200ms以内へ
      // 追い付かせる。通常再生中はproducer/consumerとも48kHzなので通らない。
      while (this.queuedFrames > 9_600 && this.head < this.queue.length - 1) {
        const block = this.queue[this.head];
        this.queuedFrames -= Math.floor((block.length - this.offset) / 2);
        this.head += 1;
        this.offset = 0;
      }
    };
  }

  /** 未再生PCMを破棄し、再バッファ開始前の無音状態へ戻す。 */
  resetQueue() {
    this.queue = [];
    this.head = 0;
    this.offset = 0;
    this.queuedFrames = 0;
    this.playing = false;
  }

  /** 消費済みPCMブロックをまとめて除去し、キューの増大を抑える。 */
  compactQueue() {
    if (this.head >= 32) {
      this.queue = this.queue.slice(this.head);
      this.head = 0;
    }
  }

  /** AudioWorkletの出力周期ごとにPCMを左右チャンネルへ供給する。 */
  process(_inputs, outputs) {
    const output = outputs[0];
    if (!output || output.length < 2) return true;
    output[0].fill(0);
    output[1].fill(0);

    // 3 emulation frames（約50ms）を先に確保し、RAFやGCの短い揺れを
    // AudioWorkletの128-frame周期へ露出させない。
    if (!this.playing) {
      if (this.queuedFrames < 2_400) return true;
      this.playing = true;
    }

    for (let frame = 0; frame < output[0].length; frame += 1) {
      while (this.head < this.queue.length && this.offset >= this.queue[this.head].length) {
        this.head += 1;
        this.offset = 0;
      }
      const block = this.queue[this.head];
      if (!block) {
        // 空の間にoffsetを増やすと、次に届くblockまで消費済み扱いで
        // 捨て続けてしまう。無音化して再バッファし、次のblockは先頭から読む。
        this.playing = false;
        this.offset = 0;
        this.compactQueue();
        break;
      }
      output[0][frame] = block[this.offset] ?? 0;
      output[1][frame] = block[this.offset + 1] ?? 0;
      this.offset += 2;
      this.queuedFrames -= 1;
    }
    this.compactQueue();
    return true;
  }
}

registerProcessor("x68k-audio", X68kAudioProcessor);
