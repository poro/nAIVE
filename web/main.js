import init, { run_clear_demo, version } from './pkg/naive_web.js';

const statusEl = document.getElementById('status');
const log = (msg, cls = '') => {
  const line = document.createElement('div');
  if (cls) line.className = cls;
  line.textContent = msg;
  statusEl.appendChild(line);
};

(async () => {
  statusEl.textContent = '';
  if (!('gpu' in navigator)) {
    log('WebGPU not available in this browser. Try Chrome/Edge 113+ or Safari 18+.', 'err');
    return;
  }
  try {
    await init();
    log(`nAIVE web v${version()} loaded`, 'ok');
    await run_clear_demo('naive-canvas');
    log('clear frame rendered ✓', 'ok');
  } catch (err) {
    log(`error: ${err?.message || err}`, 'err');
    console.error(err);
  }
})();
