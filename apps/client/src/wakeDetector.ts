// openWakeWord "hey_jarvis" detector, running in the shared webview via
// onnxruntime-web (WASM). One implementation → works on every device (ADR-026).
//
// Pipeline (16 kHz mono), per 80 ms step:
//   audio → melspectrogram.onnx  → 8 mel frames (×32), transformed x/10 + 2
//   76-frame mel window (stride 8) → embedding_model.onnx → 96-dim vector
//   16 embeddings → hey_jarvis_v0.1.onnx → score in [0,1]
//
// The ORT WASM is bundled by Vite; the three models are served from /models —
// run `scripts/setup-wakeword.sh` once to download them. Without the models the
// detector fails to load and the controller falls back to the manual hotkey.
//
// NOTE: the framing constants match openWakeWord; the exact threshold and any
// edge behaviour still want validation with real audio on-device (see ADR-026).

const SAMPLE_RATE = 16000;
const STEP_SAMPLES = 1280; // 80 ms of new audio per step
const MEL_CONTEXT = 480; // extra lookback so melspec yields exactly 8 frames
const MEL_WINDOW = STEP_SAMPLES + MEL_CONTEXT; // 1760 samples → 8 mel frames
const MEL_BINS = 32;
const EMB_FRAMES = 76; // mel frames per embedding window
const EMB_DIM = 96;
const WW_EMB = 16; // embeddings per wakeword window
const DEFAULT_THRESHOLD = 0.5;
const REFRACTORY_STEPS = 25; // ~2 s of silence after a hit before re-arming

/* eslint-disable @typescript-eslint/no-explicit-any */
// The wasm-only entry: CPU backend, no bundled WebGPU/JSEP wasm blob.
type OrtModule = typeof import("onnxruntime-web/wasm");
type Session = any;
/* eslint-enable @typescript-eslint/no-explicit-any */

export interface DetectorCallbacks {
  /** Fired once when the wake word crosses the threshold. */
  onWake: () => void;
  /** Every scored step (for live tuning / meters). Optional. */
  onScore?: (score: number) => void;
}

export class WakeDetector {
  private ort: OrtModule | null = null;
  private melSess: Session = null;
  private embSess: Session = null;
  private wwSess: Session = null;

  private ctx: AudioContext | null = null;
  private stream: MediaStream | null = null;
  private node: ScriptProcessorNode | null = null;

  // Rolling raw audio (16 kHz), mel frames, and embedding vectors.
  private audio: Float32Array = new Float32Array(0);
  private pending = 0; // new samples since the last step
  private mel: Float32Array[] = []; // each entry: 32 floats
  private emb: Float32Array[] = []; // each entry: 96 floats
  private cooldown = 0;

  public threshold = DEFAULT_THRESHOLD;

  constructor(private cb: DetectorCallbacks) {}

  /** Load ORT + the three models. Throws if assets are missing. */
  async load(): Promise<void> {
    if (this.wwSess) return;
    const ort = (await import("onnxruntime-web/wasm")) as OrtModule;
    // Vite bundles+serves the wasm next to the ORT glue, so no wasmPaths needed.
    ort.env.wasm.numThreads = 1; // single-threaded: no SharedArrayBuffer / COOP-COEP
    this.ort = ort;

    const opts = { executionProviders: ["wasm"] as const };
    [this.melSess, this.embSess, this.wwSess] = await Promise.all([
      ort.InferenceSession.create("/models/melspectrogram.onnx", opts),
      ort.InferenceSession.create("/models/embedding_model.onnx", opts),
      ort.InferenceSession.create("/models/hey_jarvis_v0.1.onnx", opts),
    ]);
  }

  /** Open the mic at 16 kHz and start scoring. Call load() first. */
  async start(): Promise<void> {
    if (this.node) return;
    if (!this.wwSess) await this.load();

    this.stream = await navigator.mediaDevices.getUserMedia({ audio: true });
    // Ask for a 16 kHz context so no resampling is needed. Fall back if the
    // platform refuses the rate.
    try {
      this.ctx = new AudioContext({ sampleRate: SAMPLE_RATE });
    } catch {
      this.ctx = new AudioContext();
    }
    const source = this.ctx.createMediaStreamSource(this.stream);
    const node = this.ctx.createScriptProcessor(2048, 1, 1);
    const mute = this.ctx.createGain();
    mute.gain.value = 0;

    const nativeRate = this.ctx.sampleRate;
    node.onaudioprocess = (e) => {
      let block = e.inputBuffer.getChannelData(0);
      if (nativeRate !== SAMPLE_RATE) block = downsample(block, nativeRate);
      this.feed(block);
    };
    source.connect(node);
    node.connect(mute);
    mute.connect(this.ctx.destination);

    this.node = node;
    this.reset();
  }

  async stop(): Promise<void> {
    this.node?.disconnect();
    this.node = null;
    this.stream?.getTracks().forEach((t) => t.stop());
    this.stream = null;
    if (this.ctx) {
      await this.ctx.close();
      this.ctx = null;
    }
    this.reset();
  }

  private reset(): void {
    this.audio = new Float32Array(0);
    this.pending = 0;
    this.mel = [];
    this.emb = [];
    this.cooldown = 0;
  }

  /** Accumulate audio and run a step every STEP_SAMPLES samples. */
  private feed(block: Float32Array): void {
    const merged = new Float32Array(this.audio.length + block.length);
    merged.set(this.audio);
    merged.set(block, this.audio.length);
    // Keep only what a mel window can need.
    this.audio =
      merged.length > MEL_WINDOW ? merged.slice(merged.length - MEL_WINDOW) : merged;
    this.pending += block.length;

    while (this.pending >= STEP_SAMPLES) {
      this.pending -= STEP_SAMPLES;
      void this.step();
    }
  }

  private stepping = false;
  private async step(): Promise<void> {
    // Serialize inference; drop overlapping steps rather than queueing lag.
    if (this.stepping || !this.ort || !this.melSess) return;
    this.stepping = true;
    try {
      if (this.audio.length < MEL_WINDOW) return;
      const ort = this.ort;

      // 1) melspectrogram → 8 frames × 32, transformed.
      const melIn = new ort.Tensor("float32", this.audio.slice(-MEL_WINDOW), [
        1,
        MEL_WINDOW,
      ]);
      const melOut = await this.melSess.run({
        [this.melSess.inputNames[0]]: melIn,
      });
      const melData = melOut[this.melSess.outputNames[0]].data as Float32Array;
      const frames = melData.length / MEL_BINS;
      for (let f = 0; f < frames; f++) {
        const frame = new Float32Array(MEL_BINS);
        for (let b = 0; b < MEL_BINS; b++) {
          frame[b] = melData[f * MEL_BINS + b] / 10 + 2;
        }
        this.mel.push(frame);
      }
      if (this.mel.length > EMB_FRAMES) this.mel = this.mel.slice(-EMB_FRAMES);
      if (this.mel.length < EMB_FRAMES) return;

      // 2) embedding over the 76-frame window → 96-dim.
      const embInput = new Float32Array(EMB_FRAMES * MEL_BINS);
      for (let i = 0; i < EMB_FRAMES; i++) embInput.set(this.mel[i], i * MEL_BINS);
      const embOut = await this.embSess.run({
        [this.embSess.inputNames[0]]: new ort.Tensor("float32", embInput, [
          1,
          EMB_FRAMES,
          MEL_BINS,
          1,
        ]),
      });
      const embData = embOut[this.embSess.outputNames[0]].data as Float32Array;
      this.emb.push(new Float32Array(embData.slice(0, EMB_DIM)));
      if (this.emb.length > WW_EMB) this.emb = this.emb.slice(-WW_EMB);
      if (this.emb.length < WW_EMB) return;

      // 3) wakeword score over the 16-embedding window.
      const wwInput = new Float32Array(WW_EMB * EMB_DIM);
      for (let i = 0; i < WW_EMB; i++) wwInput.set(this.emb[i], i * EMB_DIM);
      const wwOut = await this.wwSess.run({
        [this.wwSess.inputNames[0]]: new ort.Tensor("float32", wwInput, [
          1,
          WW_EMB,
          EMB_DIM,
        ]),
      });
      const score = (wwOut[this.wwSess.outputNames[0]].data as Float32Array)[0];
      this.cb.onScore?.(score);

      if (this.cooldown > 0) {
        this.cooldown--;
      } else if (score >= this.threshold) {
        this.cooldown = REFRACTORY_STEPS;
        this.cb.onWake();
      }
    } catch (err) {
      // A single bad step shouldn't kill the stream; surface once via console.
      console.error("[wake] step failed:", err);
    } finally {
      this.stepping = false;
    }
  }
}

/** Cheap linear downsample to 16 kHz for the rare context that ignores the rate. */
function downsample(input: Float32Array, inRate: number): Float32Array {
  const ratio = inRate / SAMPLE_RATE;
  const outLen = Math.floor(input.length / ratio);
  const out = new Float32Array(outLen);
  for (let i = 0; i < outLen; i++) {
    const idx = i * ratio;
    const i0 = Math.floor(idx);
    const frac = idx - i0;
    out[i] = (input[i0] ?? 0) * (1 - frac) + (input[i0 + 1] ?? input[i0] ?? 0) * frac;
  }
  return out;
}
