// Push-to-talk mic with voice-activity detection.
//
// `level` (0..1) follows your voice so the UI can uplight while you speak; the
// mic auto-stops after a few seconds of silence. Best-effort transcription uses
// the webview SpeechRecognition when present (often absent in WKWebView — real
// STT is DEC-009); `onTranscript` fires with recognised text.
import { ref, onBeforeUnmount } from "vue";

const AC =
  window.AudioContext ??
  (window as unknown as { webkitAudioContext?: typeof AudioContext }).webkitAudioContext;
const SR =
  (window as unknown as { webkitSpeechRecognition?: new () => unknown }).webkitSpeechRecognition ??
  (window as unknown as { SpeechRecognition?: new () => unknown }).SpeechRecognition;

const SILENCE_MS = 5000;
const SPEAK_LEVEL = 0.06;

export function useMic(onTranscript: (said: string) => void) {
  const available = !!(navigator.mediaDevices && AC);
  const listening = ref(false);
  const level = ref(0);

  let stream: MediaStream | null = null;
  let ctx: AudioContext | null = null;
  let analyser: AnalyserNode | null = null;
  let data: Uint8Array | null = null;
  let raf = 0;
  let silence: number | undefined;
  // eslint-disable-next-line @typescript-eslint/no-explicit-any
  let recog: any = null;

  function armSilence() {
    clearTimeout(silence);
    silence = window.setTimeout(stop, SILENCE_MS); // auto-off after 5s of quiet
  }

  function loop() {
    if (!analyser || !data) return;
    analyser.getByteTimeDomainData(data);
    let sum = 0;
    for (let i = 0; i < data.length; i++) {
      const x = (data[i] - 128) / 128;
      sum += x * x;
    }
    const rms = Math.sqrt(sum / data.length);
    level.value = Math.max(level.value * 0.7, Math.min(1, rms * 4));
    if (level.value > SPEAK_LEVEL) armSilence();
    raf = requestAnimationFrame(loop);
  }

  function startRecognition() {
    if (!SR) return;
    try {
      recog = new SR();
      recog.lang = "nl-NL";
      recog.interimResults = false;
      recog.continuous = true;
      recog.onresult = (e: { results: ArrayLike<ArrayLike<{ transcript: string }>> }) => {
        const last = e.results[e.results.length - 1];
        const said = last && last[0] ? last[0].transcript : "";
        if (said.trim()) onTranscript(said);
        armSilence();
      };
      recog.onend = () => {
        recog = null;
      };
      recog.start();
    } catch {
      recog = null;
    }
  }

  async function start() {
    const md = navigator.mediaDevices;
    if (!md || !AC) return;
    try {
      stream = await md.getUserMedia({ audio: true });
    } catch {
      return; // permission denied / no mic
    }
    ctx = new AC();
    const src = ctx.createMediaStreamSource(stream);
    analyser = ctx.createAnalyser();
    analyser.fftSize = 512;
    src.connect(analyser);
    data = new Uint8Array(analyser.frequencyBinCount);
    listening.value = true;
    armSilence();
    loop();
    startRecognition();
  }

  function stop() {
    clearTimeout(silence);
    cancelAnimationFrame(raf);
    if (recog) {
      try {
        recog.stop();
      } catch {
        /* ignore */
      }
      recog = null;
    }
    if (stream) {
      stream.getTracks().forEach((t) => t.stop());
      stream = null;
    }
    if (ctx) {
      ctx.close().catch(() => {});
      ctx = null;
    }
    analyser = null;
    data = null;
    level.value = 0;
    listening.value = false;
  }

  function toggle() {
    if (listening.value) stop();
    else start();
  }

  onBeforeUnmount(stop);

  return { available, listening, level, toggle, stop };
}
