class X68kAudioProcessor extends AudioWorkletProcessor {
  constructor() {
    super();
    this.queue = [];
    this.offset = 0;
    this.port.onmessage = (event) => {
      if (event.data instanceof Float32Array) this.queue.push(event.data);
    };
  }

  process(_inputs, outputs) {
    const output = outputs[0];
    if (!output || output.length < 2) return true;
    for (let frame = 0; frame < output[0].length; frame += 1) {
      while (this.queue.length && this.offset >= this.queue[0].length) {
        this.queue.shift();
        this.offset = 0;
      }
      const block = this.queue[0];
      output[0][frame] = block ? block[this.offset] ?? 0 : 0;
      output[1][frame] = block ? block[this.offset + 1] ?? 0 : 0;
      this.offset += 2;
    }
    return true;
  }
}

registerProcessor("x68k-audio", X68kAudioProcessor);
