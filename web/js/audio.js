// 声音播放
// 由 web/index.html 拆分而来；经典脚本，按 index.html 中的顺序加载，共享全局作用域。
'use strict';

let AUDIO_CODEC = 'raw', AUDIO_CTX = null;
(async () => {
  try {
    if (window.AudioDecoder && (await AudioDecoder.isConfigSupported({ codec: 'opus', sampleRate: 48000, numberOfChannels: 2 })).supported) AUDIO_CODEC = 'opus';
  } catch {}
})();
function audioCtx() {
  if (!AUDIO_CTX) AUDIO_CTX = new (window.AudioContext || window.webkitAudioContext)({ sampleRate: 48000, latencyHint: 'interactive' });
  if (AUDIO_CTX.state === 'suspended') AUDIO_CTX.resume().catch(() => {});
  return AUDIO_CTX;
}
// 浏览器要求用户操作后才能播放声音
addEventListener('pointerdown', () => { if (AUDIO_CTX && AUDIO_CTX.state === 'suspended') AUDIO_CTX.resume().catch(() => {}); }, true);

class AudioOut {
  constructor(codec) {
    this.codec = codec; this.ctx = audioCtx(); this.next = 0;
    this.gain = this.ctx.createGain(); this.gain.connect(this.ctx.destination);
    // 录屏用：声音同时送到这个目的节点
    this.capture = this.ctx.createMediaStreamDestination();
    this.gain.connect(this.capture);
    if (codec !== 'raw') this.dec = new AudioDecoder({ output: a => this.onData(a), error: e => console.warn('audio decoder', e) });
  }
  packet(config, pts, data) {
    if (this.codec === 'raw') return this.pcm(data);
    if (config) {
      try { this.dec.configure({ codec: 'opus', sampleRate: 48000, numberOfChannels: 2, description: data.slice() }); this.ready = true; } catch (e) { console.warn(e); }
      return;
    }
    if (!this.ready) { try { this.dec.configure({ codec: 'opus', sampleRate: 48000, numberOfChannels: 2 }); this.ready = true; } catch { return; } }
    try { this.dec.decode(new EncodedAudioChunk({ type: 'key', timestamp: pts, data })); } catch {}
  }
  onData(a) {
    const n = a.numberOfFrames, ch = a.numberOfChannels, buf = this.ctx.createBuffer(ch, n, a.sampleRate);
    for (let i = 0; i < ch; i++) { const tmp = new Float32Array(n); a.copyTo(tmp, { planeIndex: i, format: 'f32-planar' }); buf.copyToChannel(tmp, i); }
    a.close();
    this.schedule(buf);
  }
  pcm(data) {
    const frames = data.byteLength >> 2;
    if (!frames) return;
    const dv = new DataView(data.buffer, data.byteOffset, frames * 4), buf = this.ctx.createBuffer(2, frames, 48000);
    const l = buf.getChannelData(0), r = buf.getChannelData(1);
    for (let i = 0; i < frames; i++) { l[i] = dv.getInt16(i * 4, true) / 32768; r[i] = dv.getInt16(i * 4 + 2, true) / 32768; }
    this.schedule(buf);
  }
  schedule(buf) {
    const now = this.ctx.currentTime;
    if (this.next < now + 0.02) this.next = now + 0.06;   // 欠载：留一点缓冲
    else if (this.next > now + 0.4) return;               // 积压：丢弃以控制延迟
    const src = this.ctx.createBufferSource();
    src.buffer = buf; src.connect(this.gain); src.start(this.next);
    this.next += buf.duration;
  }
  close() {
    try { if (this.dec && this.dec.state !== 'closed') this.dec.close(); } catch {}
    try { this.gain.disconnect(); } catch {}
  }
}
