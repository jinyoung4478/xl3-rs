// Main-thread UI. The renderer runs in a Web Worker so the main
// thread stays responsive — `wasm-pack --target web` + `init()`
// happens once inside the worker, then each Run button posts a
// scenario name and waits for the worker's reply.

const worker = new Worker(new URL('./worker.js', import.meta.url), {
  type: 'module',
});

const pending = new Map();
let nextId = 1;

worker.addEventListener('message', (event) => {
  const { id, ok, median, samples, error, log } = event.data;
  const entry = pending.get(id);
  if (!entry) return;
  pending.delete(id);
  if (!ok) {
    entry.reject(new Error(error));
  } else {
    entry.resolve({ median, samples, log });
  }
});

function call(scenario) {
  const id = nextId++;
  return new Promise((resolve, reject) => {
    pending.set(id, { resolve, reject });
    worker.postMessage({ id, scenario });
  });
}

for (const button of document.querySelectorAll('button[data-scenario]')) {
  button.addEventListener('click', async () => {
    const scenario = button.dataset.scenario;
    const card = button.closest('.scenario');
    const resultEl = card.querySelector('.result strong');
    const logEl = card.querySelector('.log');
    button.disabled = true;
    resultEl.textContent = '…';
    logEl.textContent = '';
    try {
      const { median, samples, log } = await call(scenario);
      resultEl.textContent = median.toFixed(0);
      logEl.textContent =
        'samples: ' + samples.map((n) => n.toFixed(1)).join(' / ') +
        (log ? '\n' + log : '');
    } catch (e) {
      resultEl.textContent = '×';
      logEl.textContent = e.message;
    } finally {
      button.disabled = false;
    }
  });
}
