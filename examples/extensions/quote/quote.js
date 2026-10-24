/* Each source role connects independently using the card's source chooser. */
let disposed = false;
const slots = ["quote", "comparison"].map(id => ({
  id, output: document.getElementById(id), key: undefined,
  active: false, generation: 0, stopLive: null,
}));

function stopSubscription(slot) {
  slot.generation++;
  slot.stopLive?.();
  slot.stopLive = null;
}

const unsubscribe = TradeSlope.onContextChange(context => {
  if (disposed) return;
  for (const slot of slots) {
    const input = context.inputs?.[slot.id];
    const nextKey = JSON.stringify(input || {});
    const nextActive = context.active !== false;
    const connected = input?.connected === true;
    if (slot.key !== nextKey || slot.active !== nextActive) {
      stopSubscription(slot);
      if (slot.key !== nextKey) slot.output.textContent = `Loading ${slot.id === "quote" ? "Quote" : "Comparison"} input…`;
    }
    slot.key = nextKey;
    slot.active = nextActive;
    if (slot.id === "comparison") slot.output.hidden = !connected;
    if (!connected) {
      slot.output.textContent = slot.id === "quote" ? "Choose a data source for this card." : "";
      continue;
    }
    if (!slot.active || slot.stopLive) continue;
    const generation = slot.generation;
    slot.stopLive = TradeSlope.inputs.subscribe(slot.id, {}, (ticker, error, metadata) => {
      if (disposed || generation !== slot.generation) return;
      if (metadata?.hasValue === false) slot.output.textContent = "";
      if (metadata?.status === "loading") {
        slot.output.textContent = `Loading ${slot.id === "quote" ? "Quote" : "Comparison"} input…`;
        return;
      }
      slot.output.textContent = error ? error.message : slot.id === "quote"
        ? JSON.stringify(ticker, null, 2)
        : `Comparison · ${ticker.symbol}: ${ticker.last}`;
    });
  }
});

window.addEventListener("pagehide", () => {
  disposed = true;
  for (const slot of slots) stopSubscription(slot);
  unsubscribe();
}, { once: true });
