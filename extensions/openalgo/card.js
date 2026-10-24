/* TradeSlope wrapper. Vendored OpenAlgo Charts files are unmodified. */
(() => {
  "use strict";

  const fallback = document.getElementById("fallback");
  const description = document.getElementById("description");
  let widget = null;
  let disposed = false;
  const hosted = !!window.TradeSlope;
  const intervals = hosted ? ["1m", "5m", "15m", "30m", "1h", "4h", "1d"] : ["1d"];
  let input = window.TradeSlope?.context?.inputs?.bars || {};
  let selectedInterval = hosted ? "1h" : "1d";
  let controlError = null;
  let inputsKey = JSON.stringify(input);
  let comparisonInput = window.TradeSlope?.context?.inputs?.comparison || {};
  let comparisonKey = JSON.stringify(comparisonInput);
  let comparisonSeries = null;
  let comparisonGeneration = 0;
  let stopComparison = null;
  let comparisonStatus = "";
  let mainStatus = "";
  let connected = window.TradeSlope?.context?.inputs?.bars?.connected === true;
  let active = window.TradeSlope?.context?.active !== false;
  let generation = 0;
  let fitNext = true;
  let stopLive = null;
  const unsubscribe = [];
  // One retained window per current source/timeframe. Remaps release it rather
  // than caching account data. At most 10,000 candles per port; keep the newest.
  const MAX_HISTORY = 10000;
  const history = { bars: newHistory(), comparison: newHistory() };
  let writing = false;

  function newHistory() {
    return { bars: [], epoch: 0, pending: null, exhausted: false, retryAt: 0, failures: 0 };
  }

  function invalidateHistory(port, clear = false) {
    const state = history[port];
    state.epoch++;
    state.pending = null;
    if (clear) Object.assign(state, newHistory(), { epoch: state.epoch });
  }

  function timelineAnchor() {
    const chart = widget.chart;
    const range = chart.getVisibleLogicalRange();
    return {
      from: chart.dataLayer.indexToTimeFloat(range.from),
      to: chart.dataLayer.indexToTimeFloat(range.to),
    };
  }

  function restoreAnchor(anchor) {
    if (!Number.isFinite(anchor.from) || !Number.isFinite(anchor.to)) return;
    const chart = widget.chart;
    chart.timeScale.setVisibleLogicalRange({
      from: chart.dataLayer.timeToIndexFloat(anchor.from),
      to: chart.dataLayer.timeToIndexFloat(anchor.to),
    });
  }

  function mergeBars(port, bars, older = false) {
    validateBars(bars);
    const state = history[port];
    const merged = new Map(state.bars.map(bar => [bar.time, bar]));
    const existing = new Set(merged.keys());
    // A slow history response must not overwrite a newer live correction.
    for (const bar of bars) if (!older || !merged.has(bar.time)) merged.set(bar.time, bar);
    const all = [...merged.values()].sort((a, b) => a.time - b.time);
    const pruned = all.length > MAX_HISTORY;
    const next = all.slice(-MAX_HISTORY);
    const anchor = timelineAnchor();
    const follow = !older && port === "bars" && widget.chart.timeScale.rightOffset >= 0;
    const series = port === "bars" ? widget.series : comparisonSeries;
    const display = bar => port === "bars" ? bar : { time: bar.time, value: bar.close };
    writing = true;
    try {
      if (!state.bars.length || pruned) series.setData(next.map(display));
      else if (older) series.prependData(next.filter(bar => !existing.has(bar.time)).map(display));
      else for (const bar of bars) series.update(display(bar));
      state.bars = next;
      if (!follow || older) restoreAnchor(anchor);
    } finally { writing = false; }
    if (next.length >= MAX_HISTORY) state.exhausted = true;
    return next;
  }

  // Only gestures demand pages: neither live refreshes nor prepend-generated
  // viewport events recursively fetch. A further gesture retries after backoff.
  function demandHistory() {
    if (!hosted || disposed || !active || writing || controlError || fitNext) return;
    const chart = widget.chart;
    const range = chart.getVisibleLogicalRange();
    const demandTime = chart.dataLayer.indexToTimeFloat(range.from - 30);
    for (const port of ["bars", "comparison"]) {
      const state = history[port];
      if (!(port === "bars" ? connected && stopLive : stopComparison) ||
          !state.bars.length || state.pending || state.exhausted || Date.now() < state.retryAt ||
          demandTime > state.bars[0].time) continue;
      const epoch = state.epoch;
      const oldest = state.bars[0].time;
      const token = {};
      state.pending = token;
      Promise.resolve().then(() => {
        if (epoch !== state.epoch || disposed || !active) return null;
        return window.TradeSlope.inputs.request(port, { interval: selectedInterval, end: oldest - 1, limit: 200 });
      }).then(bars => {
        if (epoch !== state.epoch || disposed || !active) return;
        validateBars(bars);
        const older = bars.filter(bar => bar.time < oldest);
        if (!older.length) state.exhausted = true;
        else mergeBars(port, older, true);
        state.failures = 0;
        state.retryAt = 0;
        if (port === "bars" && mainStatus.startsWith("Older history failed:")) mainStatus = "";
        if (port === "comparison" && comparisonStatus.startsWith("Comparison · Older history failed:")) {
          comparisonStatus = `Comparison · ${comparisonInput.label || "Comparison input"} · Live updates`;
        }
        updateLabel();
      }).catch(error => {
        if (epoch !== state.epoch || disposed || !active) return;
        state.retryAt = Date.now() + Math.min(60000, 1000 * 2 ** Math.min(state.failures++, 6));
        const message = `Older history failed: ${error instanceof Error ? error.message : String(error)}. Pan again to retry.`;
        if (port === "bars") mainStatus = message;
        else comparisonStatus = `Comparison · ${message}`;
        updateLabel();
      }).finally(() => {
        if (state.pending === token) state.pending = null;
      });
    }
  }

  function onWheel(event) {
    // Upstream treats wheel over either price axis as price scaling, including
    // ctrl-wheel. Capture only its canvas container; never steal toolbar input.
    const canvas = document.querySelector(".oac-chart");
    if (!canvas || !canvas.contains(event.target) || disposed) return;
    event.preventDefault();
    event.stopPropagation();
    const rect = canvas.getBoundingClientRect();
    const plot = widget.chart.plotRect(widget.chart.primaryPaneIndex());
    if (!plot) return;
    const delta = event.deltaY || event.deltaX;
    const pixels = delta * (event.deltaMode === 1 ? 16 : event.deltaMode === 2 ? rect.height : 1);
    const x = Math.max(0, Math.min(plot.width, event.clientX - rect.left - plot.left));
    widget.chart.timeScale.zoomAtX(x, Math.exp(-Math.max(-1, Math.min(1, pixels / 500))));
    demandHistory();
  }

  // The upstream widget uses hour codes too; accept minute aliases at the boundary.
  function normalizeInterval(code) {
    return ({ "60m": "1h", "240m": "4h" })[code] || code;
  }

  function canSelectInterval(code) {
    const normalized = normalizeInterval(code);
    const supported = input.controlOptions?.interval;
    return intervals.includes(normalized) &&
      (!hosted || !Array.isArray(supported) || supported.some(value => normalizeInterval(value) === normalized));
  }

  function sourceLabel() {
    return typeof input.label === "string" && input.label.trim() ? input.label.trim() : "Price chart";
  }

  function configureInterval(previousValue, initial = false) {
    controlError = null;
    const raw = input.controlValues?.interval;
    const value = normalizeInterval(raw);
    if (raw !== undefined && !canSelectInterval(value)) {
      controlError = `This source is fixed to ${String(raw)}, which this chart cannot display. Change the source or its advanced mapping.`;
      return;
    }
    if (raw !== undefined && (initial || raw !== previousValue)) selectedInterval = value;
    if (!canSelectInterval(selectedInterval)) {
      const next = intervals.find(canSelectInterval);
      if (!next) {
        controlError = "This source does not expose a usable chart timeframe. Change the source or its advanced mapping.";
        return;
      }
      selectedInterval = next;
    }
  }

  function showError(error) {
    fallback.textContent = `Chart unavailable: ${error instanceof Error ? error.message : String(error)}`;
    fallback.hidden = false;
    fallback.setAttribute("role", "alert");
  }

  // Fixed UTC daily bars, not prices for any real instrument. No random/clock input.
  function demoBars() {
    const start = Date.UTC(2025, 0, 1) / 1000;
    let previous = 100;
    return Array.from({ length: 240 }, (_, i) => {
      const open = previous;
      const close = Math.round((100 + i * 0.085 + 5 * Math.sin(i / 11) + 2 * Math.sin(i / 3)) * 100) / 100;
      previous = close;
      return {
        time: start + i * 86400,
        open,
        high: Math.round((Math.max(open, close) + 0.8 + (i % 7) * 0.12) * 100) / 100,
        low: Math.round((Math.min(open, close) - 0.8 - (i % 5) * 0.14) * 100) / 100,
        close,
        volume: 10000 + ((i * 7919) % 40000),
      };
    });
  }

  function currentWidget() {
    if (!widget || disposed) throw new Error("Chart is not ready or has been disposed");
    return widget;
  }

  function setTheme(theme) {
    if (theme !== "dark" && theme !== "light") throw new TypeError("Theme must be dark or light");
    currentWidget().setTheme(theme);
  }

  function setSymbol(symbol) {
    if (typeof symbol !== "string" || !symbol.trim()) {
      throw new TypeError("Symbol must be a nonempty string");
    }
    if (hosted) throw new Error("Choose an instrument using this card's source button");
    // Standalone compatibility: symbol changes restore the synthetic daily bars.
    currentWidget().setSymbol(symbol.trim(), "DEMO");
  }

  function updateLabel() {
    const badge = document.querySelector(".badge");
    badge.textContent = hosted ? (connected ? "MAPPED" : "UNBOUND") : "DEMO";
    description.textContent = hosted
      ? `${sourceLabel()} · ${selectedInterval} · ` +
        (mainStatus || (connected ? "Live updates" : "Choose a data source for this card")) +
        (comparisonStatus ? ` · ${comparisonStatus}` : "")
      : `${widget.symbol()} · DEMO synthetic daily OHLCV · No live feed or trading`;
  }

  function stopComparisonSubscription() {
    comparisonGeneration++;
    invalidateHistory("comparison");
    stopComparison?.();
    stopComparison = null;
  }

  function subscribeComparison() {
    if (!widget || disposed || !active || stopComparison || controlError ||
        comparisonInput.connected !== true) return;
    const fixed = comparisonInput.controlValues?.interval;
    const allowed = comparisonInput.controlOptions?.interval;
    if ((fixed !== undefined && normalizeInterval(fixed) !== selectedInterval) ||
        (Array.isArray(allowed) && !allowed.some(value => normalizeInterval(value) === selectedInterval))) {
      comparisonStatus = `Comparison · This source cannot supply ${selectedInterval}. Change the source or its advanced mapping.`;
      updateLabel();
      return;
    }
    const subscriptionGeneration = comparisonGeneration;
    if (!comparisonSeries) {
      comparisonSeries = widget.chart.addSeries("line", {
        priceScaleId: "left", style: { title: "Comparison" },
      });
    }
    comparisonStatus = "Comparison · Loading";
    updateLabel();
    stopComparison = window.TradeSlope.inputs.subscribe("comparison", { interval: selectedInterval }, (bars, error, metadata) => {
      if (disposed || subscriptionGeneration !== comparisonGeneration) return;
      if (metadata?.hasValue === false) {
        clearComparison();
      }
      if (metadata?.status === "loading") {
        comparisonStatus = "Comparison · Loading";
        updateLabel();
        return;
      }
      try {
        if (error) throw error;
        const retained = mergeBars("comparison", bars);
        const label = typeof comparisonInput.label === "string" && comparisonInput.label.trim()
          ? comparisonInput.label.trim() : "Comparison input";
        comparisonStatus = `Comparison · ${label} · ${retained.length ? "Live updates" : "No bars yet"}`;
      } catch (error) {
        // Never hide or replace the main price series for a secondary failure.
        comparisonStatus = `Comparison · Update failed: ${error instanceof Error ? error.message : String(error)}. ` +
          (history.comparison.bars.length ? "Existing prices may be stale." : "No current data.");
      }
      updateLabel();
    });
  }

  function clearComparison() {
    const anchor = timelineAnchor();
    invalidateHistory("comparison", true);
    writing = true;
    try {
      comparisonSeries?.setData([]);
      restoreAnchor(anchor);
    } finally { writing = false; }
  }

  function resetComparison() {
    stopComparisonSubscription();
    clearComparison();
    comparisonStatus = comparisonInput.connected === true ? "Comparison · Waiting for data" : "";
    updateLabel();
    subscribeComparison();
  }

  function stopSubscription() {
    generation++;
    invalidateHistory("bars");
    stopLive?.();
    stopLive = null;
  }

  function subscribe() {
    if (!widget || disposed || !connected || !active || stopLive || controlError) return;
    const subscriptionGeneration = generation;
    stopLive = window.TradeSlope.inputs.subscribe("bars", { interval: selectedInterval }, (bars, error, metadata) => {
      if (disposed || subscriptionGeneration !== generation) return;
      if (metadata?.hasValue === false) {
        invalidateHistory("bars", true);
        widget.series.setData([]);
        fitNext = true;
      }
      if (metadata?.status === "loading") {
        fallback.textContent = "Loading price data…";
        fallback.hidden = false;
        mainStatus = "Loading";
        updateLabel();
        return;
      }
      try {
        if (error) throw error;
        const retained = mergeBars("bars", bars);
        if (retained.length === 0) {
          fitNext = true;
          fallback.textContent = "No bars yet. Waiting for live updates…";
          fallback.hidden = false;
          mainStatus = "No bars yet";
          updateLabel();
          return;
        }
        if (fitNext) { widget.chart.fitContent(); fitNext = false; }
        fallback.hidden = true;
        mainStatus = "";
        updateLabel();
      } catch (error) {
        showError(error);
        mainStatus = metadata?.hasValue === false
          ? "Update failed. No current data."
          : "Update failed. Existing prices may be stale.";
        updateLabel();
      }
    });
  }

  // Validate the normalized Candle[] contract before changing the visible series.
  function validateBars(bars) {
    if (!Array.isArray(bars)) throw new TypeError("Bars input must return Candle[]");
    for (const bar of bars) {
      if (!bar || !["time", "open", "high", "low", "close", "volume"].every(key =>
        typeof bar[key] === "number" && Number.isFinite(bar[key])) ||
        !Number.isSafeInteger(bar.time) || bar.volume < 0 ||
        bar.high < Math.max(bar.open, bar.close) || bar.low > Math.min(bar.open, bar.close) ||
        bar.low > bar.high) throw new TypeError("Bars input returned invalid candles");
    }
  }

  function resetInput() {
    stopSubscription();
    invalidateHistory("bars", true);
    fitNext = true;
    widget.series.setData([]);
    mainStatus = "";
    updateLabel();
    document.getElementById("chart").style.visibility = controlError ? "hidden" : "";
    if (controlError) {
      fallback.textContent = controlError;
      fallback.hidden = false;
      mainStatus = String(input.controlValues?.interval || "Timeframe unavailable");
      resetComparison();
      return;
    }
    if (hosted) {
      fallback.textContent = connected ? "Loading price data…" : "Choose a data source for this card";
      fallback.hidden = false;
      subscribe();
    } else {
      widget.series.setData(demoBars());
      widget.chart.fitContent();
      fallback.hidden = true;
    }
    resetComparison();
  }

  function applyContext(context) {
    if (!widget || disposed) return;
    const nextInput = context.inputs?.bars || {};
    const nextComparison = context.inputs?.comparison || {};
    const nextKey = JSON.stringify(nextInput);
    const nextComparisonKey = JSON.stringify(nextComparison);
    const changed = nextKey !== inputsKey;
    const comparisonChanged = nextComparisonKey !== comparisonKey;
    const wasActive = active;
    active = context.active !== false;
    comparisonInput = nextComparison;
    comparisonKey = nextComparisonKey;
    if (context.theme) setTheme(context.theme);
    if (active !== wasActive) {
      stopSubscription();
      stopComparisonSubscription();
    }
    if (changed) {
      inputsKey = nextKey;
      const previousControlValue = input.controlValues?.interval;
      input = nextInput;
      connected = input.connected === true;
      configureInterval(previousControlValue);
      // setInterval emits synchronously; the listener sees the already selected value.
      if (!controlError) widget.setInterval(selectedInterval);
      if (hosted) widget.setSymbol(sourceLabel(), "");
      resetInput();
    } else if (comparisonChanged) {
      resetComparison();
    }
    if (active !== wasActive) {
      if (active) {
        subscribe();
        subscribeComparison();
      }
    }
  }

  function dispose() {
    if (disposed) return;
    disposed = true;
    stopSubscription();
    stopComparisonSubscription();
    for (const off of unsubscribe.splice(0)) off();
    window.removeEventListener("error", onError);
    window.removeEventListener("unhandledrejection", onRejection);
    window.removeEventListener("pagehide", dispose);
    widget?.destroy();
    widget = null;
  }

  function onError(event) { showError(event.error || event.message || "Local resource failed"); }
  function onRejection(event) { showError(event.reason); }
  window.addEventListener("error", onError);
  window.addEventListener("unhandledrejection", onRejection);
  window.addEventListener("pagehide", dispose);

  const ready = Promise.resolve().then(async () => {
    if (disposed) throw new Error("Page was disposed before chart initialization");
    const charts = window.OpenAlgoCharts;
    if (!charts?.widget?.createWidget || !charts.draw || !charts.indicators) {
      throw new Error("OpenAlgo Charts local bundles did not load");
    }
    configureInterval(undefined, true);
    widget = charts.widget.createWidget(document.getElementById("chart"), {
      symbol: hosted ? sourceLabel() : "DEMO",
      exchange: "",
      interval: selectedInterval,
      intervals,
      canSelectInterval,
      typingNavigation: false,
      navigation: { mousePan: "horizontal", panEnabled: true, zoomEnabled: true },
      ranges: [],
      timezone: "UTC",
      theme: "dark",
      persist: false,
      storage: null,
      layouts: false,
      // No feed, onOrder, account, news or quotes: this card cannot fetch or trade.
    });
    unsubscribe.push(widget.on("theme", () => {
      document.documentElement.dataset.theme = widget.theme();
    }));
    unsubscribe.push(widget.on("symbol", () => {
      if (!hosted) resetInput();
    }));
    unsubscribe.push(widget.on("interval", () => {
      if (controlError) return;
      const next = normalizeInterval(widget.interval());
      if (!canSelectInterval(next)) {
        widget.setInterval(selectedInterval);
        resetInput();
        return;
      }
      if (next === selectedInterval && widget.interval() === next) return;
      selectedInterval = next;
      if (widget.interval() !== next) widget.setInterval(next);
      resetInput();
    }));
    await widget.ready;
    if (disposed) throw new Error("Page was disposed during chart initialization");
    unsubscribe.push(widget.chart.on("pan", demandHistory), widget.chart.on("zoom", demandHistory));
    const chartElement = document.getElementById("chart");
    chartElement.addEventListener("wheel", onWheel, { capture: true, passive: false });
    unsubscribe.push(() => chartElement.removeEventListener("wheel", onWheel, { capture: true }));
    widget.chart.addIndicator("sma", { length: 20 });
    if (hosted) {
      // Hide upstream data-source editors, including responsive mobile controls.
      document.documentElement.dataset.hosted = "true";
      for (const control of document.querySelectorAll(".oac-sym__input, .oac-mobile__symbol")) {
        control.disabled = true;
      }
    }
    resetInput();
    if (window.TradeSlope) unsubscribe.push(window.TradeSlope.onContextChange(applyContext));
    // Upstream observes its chart container with ResizeObserver, including DPR.
    // The flex layout gives that observer the remaining card height.
  });
  ready.catch(showError);

  window.TradeSlopeChart = Object.freeze({
    version: "2.6.7",
    ready,
    setTheme,
    setSymbol,
    dispose,
    getState: () => currentWidget().getState(),
  });
})();
