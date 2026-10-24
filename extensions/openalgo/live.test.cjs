const assert = require("node:assert/strict");
const vm = require("node:vm");
const fs = require("node:fs");

async function harness(hosted = true) {
  const elements = Object.fromEntries(["fallback", "description", "chart", "badge"].map(id =>
    [id, { textContent: "", hidden: false, style: {}, setAttribute() {},
      addEventListener(name, callback) { this[name] = callback; }, removeEventListener() {} }]));
  const requests = [];
  const pages = [];
  const callbacks = {};
  let contextListener;
  let bars = [];
  let comparedBars = [];
  const comparisonSeriesOptions = [];
  let destroyed = false;
  let fits = 0;
  const events = {};
  const chartEvents = {};
  let range = { from: 50, to: 150 };
  let now = 0;
  const times = () => [...new Set([...bars, ...comparedBars].map(bar => bar.time))].sort((a, b) => a - b);
  function interpolate(values, index) {
    if (!values.length) return NaN;
    if (values.length === 1) return values[0] + index;
    const i = Math.max(0, Math.min(values.length - 2, Math.floor(index)));
    return values[i] + (index - i) * (values[i + 1] - values[i]);
  }
  function timeIndex(time) {
    const values = times();
    if (values.length < 2) return values.length ? time - values[0] : NaN;
    let i = values.findIndex(value => value >= time) - 1;
    if (i < 0) i = time > values.at(-1) ? values.length - 2 : 0;
    return i + (time - values[i]) / (values[i + 1] - values[i]);
  }
  function series(get, set) {
    const merge = next => set([...new Map([...get(), ...next].map(bar => [bar.time, bar])).values()].sort((a, b) => a.time - b.time));
    function baseChange(write) {
      const before = times().length;
      write();
      const shift = times().length - before;
      range = { from: range.from + shift, to: range.to + shift };
    }
    return { setData: next => baseChange(() => set(next)), update: bar => merge([bar]),
      prependData: next => baseChange(() => merge(next)) };
  }
  const canvas = { contains: target => target === canvas, getBoundingClientRect: () => ({ left: 0, height: 500 }) };
  let symbol = "DEMO", interval = "1d", theme = "dark", options;
  const widget = {
    series: series(() => bars, next => { bars = next; }),
    chart: {
      on(name, callback) { chartEvents[name] = callback; return () => delete chartEvents[name]; },
      getVisibleLogicalRange: () => range,
      primaryPaneIndex: () => 0,
      plotRect: () => ({ left: 50, width: 800 }),
      dataLayer: { indexToTimeFloat: index => interpolate(times(), index), timeToIndexFloat: timeIndex },
      timeScale: {
        rightOffset: -1,
        setVisibleLogicalRange(next) { range = next; chartEvents.pan?.(); },
        zoomAtX(x, factor) { range = { from: range.to - (range.to - range.from) / factor, to: range.to }; chartEvents.zoom?.(); },
      },
      fitContent() { fits++; }, addIndicator() {},
      addSeries(type, options) {
        comparisonSeriesOptions.push({ type, options });
        return series(() => comparedBars, next => { comparedBars = next; });
      },
    },
    ready: Promise.resolve(),
    on(name, callback) { callbacks[name] = callback; return () => delete callbacks[name]; },
    setSymbol(next) { if (symbol !== next) { symbol = next; callbacks.symbol?.(); } },
    setInterval(next) { if (interval !== next) { interval = next; callbacks.interval?.(); } },
    setTheme(next) { theme = next; callbacks.theme?.(); },
    symbol: () => symbol, interval: () => interval, theme: () => theme,
    getState: () => ({ symbol, interval, theme }), destroy() { destroyed = true; },
  };
  const document = {
    documentElement: { dataset: {} },
    getElementById: id => elements[id],
    querySelector: selector => selector === ".oac-chart" ? canvas : elements.badge,
    querySelectorAll: () => [],
  };
  const window = {
    OpenAlgoCharts: { widget: { createWidget: (_, config) => {
      options = config; symbol = config.symbol; interval = config.interval; return widget;
    } }, draw: {}, indicators: {} },
    addEventListener: (name, callback) => { events[name] = callback; },
    removeEventListener() {},
    TradeSlope: {
      context: { inputs: {}, active: true, theme: "dark" },
      inputs: {
        request(port, params) {
          return new Promise((resolve, reject) => pages.push({ port, params, resolve, reject }));
        },
        subscribe(port, params, listener) {
        const request = { port, params, stopped: false,
          resolve: value => listener(value, undefined, { status: "live", hasValue: true }),
          reject: error => listener(undefined, error, { status: "error", hasValue: true }),
          rejectEmpty: error => listener(undefined, error, { status: "error", hasValue: false }),
          loading: () => listener(null, undefined, { status: "loading", hasValue: false }) };
        requests.push(request);
        return () => { request.stopped = true; };
      } },
      onContextChange(callback) { contextListener = callback; callback(this.context); return () => { contextListener = null; }; },
    },
  };
  if (!hosted) delete window.TradeSlope;
  vm.runInNewContext(fs.readFileSync("extensions/openalgo/card.js", "utf8"), {
    window, document, console, Date: class extends Date { static now() { return now; } },
    setInterval: () => { throw new Error("frontend must not schedule timers"); },
    setTimeout: () => { throw new Error("frontend must not schedule timers"); },
  });
  await window.TradeSlopeChart.ready;
  return {
    window, elements, requests, pages, events, options,
    pan: next => { range = next; chartEvents.pan?.(); },
    wheel: extra => {
      const event = { target: canvas, clientX: 400, deltaY: 100, deltaMode: 0,
        preventDefault() { this.prevented = true; }, stopPropagation() { this.stopped = true; }, ...extra };
      elements.chart.wheel(event);
      return event;
    },
    advance: ms => { now += ms; },
    get range() { return range; },
    context: next => contextListener(next),
    selectInterval: value => widget.setInterval(value),
    get bars() { return bars; },
    get comparedBars() { return comparedBars; },
    comparisonSeriesOptions,
    get fits() { return fits; },
    get destroyed() { return destroyed; },
  };
}
const tick = () => new Promise(setImmediate);
const candle = time => ({ time, open: 2, high: 3, low: 1, close: 2, volume: 10 });
async function test() {
  const h = await harness();
  const { requests, elements } = h;
  assert.equal(h.bars.length, 0, "host-unbound never shows demo data");
  assert.equal(elements.badge.textContent, "UNBOUND");
  assert.match(elements.fallback.textContent, /Choose a data source/);
  assert.equal(h.options.interval, "1h");
  assert.equal(JSON.stringify(h.options.intervals), '["1m","5m","15m","30m","1h","4h","1d"]');
  for (const interval of h.options.intervals) assert.equal(h.options.canSelectInterval(interval), true);
  assert.equal(h.options.canSelectInterval("1w"), false);
  assert.equal(h.options.typingNavigation, false);
  assert.throws(() => h.window.TradeSlopeChart.setSymbol("FAKE"), /source button/);
  const source = revision => ({ active: true, theme: "dark", inputs: { bars: { connected: true, revision } } });
  h.context(source(1));
  assert.equal(requests.length, 1, "one source change queues one load");
  assert.equal(h.bars.length, 0);
  assert.equal(elements.badge.textContent, "MAPPED");
  assert.equal(requests[0].port, "bars");
  assert.equal(JSON.stringify(requests[0].params), '{"interval":"1h"}');
  h.context(source(2));
  assert.equal(requests.length, 2);
  assert(requests[0].stopped, "remap cancels old subscription");
  requests[0].resolve([candle(1)]);
  await tick();
  assert.equal(h.bars.length, 0, "late pre-remap response is ignored");
  requests[1].resolve([]);
  assert.equal(h.bars.length, 0, "empty Candle[] is a valid typed value");
  assert.match(elements.fallback.textContent, /No bars yet/);
  assert.doesNotMatch(elements.fallback.textContent, /unavailable/i);
  const live = [candle(2)];
  requests[1].resolve(live);
  await tick();
  assert.equal(JSON.stringify(h.bars), JSON.stringify(live));
  h.context(source(2));
  assert.equal(requests.length, 2, "unchanged context keeps one subscription");
  requests[1].reject(new Error("offline"));
  await tick();
  assert.equal(JSON.stringify(h.bars), JSON.stringify(live), "failed refresh retains explicitly stale data, not demo prices");
  assert.match(elements.description.textContent, /stale/);
  requests[1].resolve([{ time: 3, close: 2 }]);
  await tick();
  assert.equal(JSON.stringify(h.bars), JSON.stringify(live), "malformed results must not replace the series");
  requests[1].loading();
  assert.equal(h.bars.length, 0, "account reset clears previously visible bars immediately");
  assert.equal(elements.fallback.textContent, "Loading price data…");
  assert.match(elements.description.textContent, /Loading/);
  requests[1].reject(new Error("new account offline"));
  assert.equal(h.bars.length, 0, "post-reset errors never restore previous account data");
  requests[1].resolve(live);
  assert.equal(JSON.stringify(h.bars), JSON.stringify(live), "new-generation live data recovers after reset");
  requests[1].rejectEmpty(new Error("fast reset failure"));
  assert.equal(h.bars.length, 0, "coalesced reset+error clears without a loading callback");
  assert.match(elements.description.textContent, /No current data/);
  requests[1].resolve(live);
  h.context({ ...source(2), active: false });
  assert(requests[1].stopped, "pause cancels host subscription");
  requests[1].resolve([candle(4)]);
  await tick();
  assert.equal(JSON.stringify(h.bars), JSON.stringify(live), "in-flight results are invalidated on pause");
  const count = requests.length;
  assert.equal(requests.length, count, "inactive cards do not subscribe");
  h.context(source(2));
  assert.equal(requests.length, count + 1, "resume refreshes immediately");
  h.context({ active: true, inputs: {}, theme: "light" });
  assert(requests[count].stopped);
  requests[count].reject(new Error("late failure"));
  await tick();
  assert.equal(h.bars.length, 0);
  assert.equal(elements.badge.textContent, "UNBOUND");
  assert.match(elements.fallback.textContent, /Choose a data source/);
  h.context(source("hash-revision"));
  h.events.pagehide();
  assert(requests.at(-1).stopped, "pagehide cancels live query");
  requests.at(-1).resolve([candle(5)]);
  await tick();
  assert(h.destroyed);
  assert.equal(h.bars.length, 0, "disposed response is ignored");

  const intervals = await harness();
  const labeledSource = { ...source(1), inputs: { bars: {
    ...source(1).inputs.bars, label: "Ourbit · BTC/USDT",
    controlValues: { interval: "1h" },
    controlOptions: { interval: ["1m", "5m", "15m", "30m", "1h", "4h", "1d"] },
  } } };
  intervals.context(labeledSource);
  intervals.requests[0].resolve([candle(1)]);
  assert.match(intervals.elements.description.textContent, /Ourbit · BTC\/USDT/);
  assert.equal(intervals.fits, 1);
  intervals.selectInterval("5m");
  assert(intervals.requests[0].stopped, "interval change cancels the old subscription");
  assert.equal(intervals.bars.length, 0, "interval change clears the old timeframe immediately");
  assert.equal(intervals.requests.length, 2);
  assert.equal(JSON.stringify(intervals.requests[1].params), '{"interval":"5m"}');
  intervals.requests[0].resolve([candle(99)]);
  intervals.requests[0].reject(new Error("late interval failure"));
  assert.equal(intervals.bars.length, 0, "canceled interval results cannot restore old bars");
  intervals.requests[1].resolve([candle(2)]);
  assert.equal(intervals.fits, 2, "first bars of the new interval fit the chart");
  intervals.requests[1].resolve([candle(3)]);
  assert.equal(intervals.fits, 2, "live updates preserve the viewport");
  intervals.context({ ...labeledSource, theme: "light" });
  assert.equal(intervals.window.TradeSlopeChart.getState().interval, "5m", "theme renders preserve local selection");
  assert.equal(intervals.requests.length, 2, "theme changes do not resubscribe");
  intervals.context({ ...labeledSource, inputs: { bars: { ...labeledSource.inputs.bars, revision: 2 } } });
  assert.equal(JSON.stringify(intervals.requests[2].params), '{"interval":"5m"}', "remapping preserves local selection");
  intervals.context({ ...labeledSource, inputs: { bars: {
    ...labeledSource.inputs.bars, controlValues: { interval: "4h" }, revision: 3,
  } } });
  assert.equal(JSON.stringify(intervals.requests[3].params), '{"interval":"4h"}', "explicit host control change applies once");
  intervals.selectInterval("60m");
  assert.equal(intervals.window.TradeSlopeChart.getState().interval, "1h");
  assert.equal(JSON.stringify(intervals.requests[4].params), '{"interval":"1h"}', "minute aliases subscribe with canonical hour codes");
  intervals.events.pagehide();

  const fixed = await harness();
  const fixedSource = interval => ({ ...source(1), inputs: { bars: {
    ...source(1).inputs.bars, controlValues: { interval },
    controlOptions: { interval: [interval] },
  } } });
  fixed.context(fixedSource("1d"));
  assert.equal(fixed.window.TradeSlopeChart.getState().interval, "1d");
  assert.equal(fixed.options.canSelectInterval("5m"), false);
  fixed.selectInterval("5m");
  assert.equal(fixed.window.TradeSlopeChart.getState().interval, "1d", "fixed daily data cannot be labeled five-minute");
  const beforeUnsupported = fixed.requests.length;
  fixed.context(fixedSource("1w"));
  assert.equal(fixed.requests.length, beforeUnsupported, "unsupported fixed timeframes cannot request mislabeled data");
  assert.equal(fixed.elements.chart.style.visibility, "hidden");
  assert.match(fixed.elements.fallback.textContent, /fixed to 1w/);
  fixed.context(source(2));
  assert.equal(fixed.elements.chart.style.visibility, "");
  assert.equal(fixed.requests.length, beforeUnsupported + 1, "a dynamic source restores chart controls");
  fixed.events.pagehide();

  const comparison = await harness();
  const mainInput = { connected: true, revision: 1 };
  const secondary = { connected: true, revision: 1, label: "Second instrument" };
  const pair = (extra = {}, active = true) => ({
    active, inputs: { bars: mainInput, comparison: { ...secondary, ...extra } },
  });
  comparison.context(pair());
  assert.equal(comparison.requests.length, 2, "both mapped ports subscribe");
  const [mainRequest, secondaryRequest] = comparison.requests;
  assert.equal(mainRequest.port, "bars");
  assert.equal(secondaryRequest.port, "comparison");
  assert.equal(JSON.stringify(secondaryRequest.params), '{"interval":"1h"}');
  assert.equal(JSON.stringify(comparison.comparisonSeriesOptions),
    '[{"type":"line","options":{"priceScaleId":"left","style":{"title":"Comparison"}}}]');
  const mainBars = [candle(10)];
  mainRequest.resolve(mainBars);
  secondaryRequest.resolve([candle(20)]);
  assert.equal(JSON.stringify(comparison.comparedBars), '[{"time":20,"value":2}]');
  assert.match(comparison.elements.description.textContent, /Comparison · Second instrument · Live updates/);
  mainRequest.resolve(mainBars);
  assert.match(comparison.elements.description.textContent, /Second instrument/, "main updates retain secondary status");
  comparison.context(pair({ revision: 2 }));
  assert.equal(comparison.requests.length, 3, "secondary remap does not refetch main");
  assert.equal(mainRequest.stopped, false);
  assert(secondaryRequest.stopped);
  assert.equal(JSON.stringify(comparison.bars), JSON.stringify(mainBars));
  assert.equal(comparison.comparedBars.length, 0);
  secondaryRequest.resolve([candle(99)]);
  assert.equal(comparison.comparedBars.length, 0, "late secondary remap response is ignored");
  const remapped = comparison.requests[2];
  remapped.resolve([candle(30)]);
  remapped.reject(new Error("secondary offline"));
  assert.equal(comparison.comparedBars.length, 1, "secondary errors retain stale history");
  assert.equal(JSON.stringify(comparison.bars), JSON.stringify(mainBars));
  assert.equal(comparison.elements.fallback.hidden, true);
  assert.match(comparison.elements.description.textContent, /secondary offline/);
  mainRequest.resolve(mainBars);
  assert.match(comparison.elements.description.textContent, /secondary offline/, "error status persists across primary updates");
  remapped.resolve([candle(30)]);
  remapped.loading();
  assert.equal(comparison.comparedBars.length, 0, "secondary reset clears the overlay");
  assert.equal(JSON.stringify(comparison.bars), JSON.stringify(mainBars));
  remapped.resolve([candle(30)]);
  remapped.rejectEmpty(new Error("reset failed"));
  assert.equal(comparison.comparedBars.length, 0, "hasValue false clears secondary data");
  remapped.resolve([{ time: 1, close: 5 }]);
  assert.equal(comparison.comparedBars.length, 0, "invalid secondary candles never render");
  comparison.selectInterval("5m");
  assert.equal(comparison.requests.length, 5, "timeframe change refreshes both ports once");
  for (const request of comparison.requests.slice(3)) {
    assert.equal(JSON.stringify(request.params), '{"interval":"5m"}');
  }
  assert(mainRequest.stopped && remapped.stopped);
  comparison.context(pair({ revision: 2 }, false));
  assert(comparison.requests[3].stopped && comparison.requests[4].stopped);
  comparison.context(pair({ revision: 2 }));
  assert.equal(comparison.requests.length, 7, "resume subscribes both ports exactly once");
  comparison.context(pair({ revision: 2 }));
  assert.equal(comparison.requests.length, 7);
  comparison.requests[5].resolve(mainBars);
  comparison.requests[6].resolve([candle(40)]);
  comparison.context(pair({ revision: 3, controlValues: { interval: "1h" } }));
  assert.equal(comparison.requests.length, 7, "incompatible fixed comparison must not fetch a wrong interval");
  assert.match(comparison.elements.description.textContent, /Comparison · This source cannot supply 5m/);
  assert.equal(JSON.stringify(comparison.bars), JSON.stringify(mainBars));
  assert.equal(comparison.comparedBars.length, 0);
  comparison.context(pair({ revision: 4, controlOptions: { interval: ["1d"] } }));
  assert.equal(comparison.requests.length, 7, "incompatible allowed options must not fetch");
  comparison.context(pair({ revision: 5, controlValues: { interval: "5m" } }));
  assert.equal(comparison.requests.length, 8, "compatible comparison recovers");
  comparison.requests[7].resolve([candle(50)]);
  comparison.context({ active: true, inputs: { bars: mainInput } });
  assert(comparison.requests[7].stopped, "unmapping stops secondary subscription");
  assert.equal(comparison.comparedBars.length, 0);
  assert.equal(JSON.stringify(comparison.bars), JSON.stringify(mainBars));
  assert.equal(comparison.requests.length, 8, "unmap does not refetch main");
  assert.doesNotMatch(comparison.elements.description.textContent, /Comparison/);
  comparison.context(pair({ revision: 6 }));
  comparison.events.pagehide();
  assert(comparison.requests[5].stopped && comparison.requests[8].stopped);
  comparison.requests[8].resolve([candle(60)]);
  assert.equal(comparison.comparedBars.length, 0);
  assert(comparison.destroyed);
  assert.equal(comparison.comparisonSeriesOptions.length, 1, "overlay is lazily created once and owned by widget");

  const resume = await harness();
  resume.context(pair());
  resume.context(pair({}, false));
  resume.context({
    active: true,
    inputs: { bars: { ...mainInput, revision: 2 }, comparison: { ...secondary, revision: 2 } },
  });
  assert.equal(resume.requests.length, 4, "resume with simultaneous remaps still subscribes each port only once");
  resume.requests[2].resolve(mainBars);
  resume.requests[3].resolve([candle(70)]);
  resume.context({
    active: true,
    inputs: { bars: { ...mainInput, revision: 3 }, comparison: { ...secondary, revision: 2 } },
  });
  assert.equal(resume.requests.length, 6, "main remap refreshes both inputs");
  assert(resume.requests[2].stopped && resume.requests[3].stopped);
  assert.equal(resume.bars.length, 0);
  assert.equal(resume.comparedBars.length, 0);
  resume.events.pagehide();

  const history = await harness();
  history.context(pair());
  const block = (start, count = 200) => Array.from({ length: count }, (_, i) => candle(start + i));
  history.requests[0].resolve(block(1000));
  history.requests[1].resolve(block(1000));
  assert.equal(JSON.stringify(history.options.navigation),
    '{"mousePan":"horizontal","panEnabled":true,"zoomEnabled":true}');
  history.pan({ from: 10, to: 110 });
  history.pan({ from: 9, to: 109 });
  await tick();
  assert.equal(history.pages.length, 2, "singleflight independently for both ports");
  assert.equal(JSON.stringify(history.pages[0].params), '{"interval":"1h","end":999,"limit":200}');
  const corrected = { ...candle(1000), close: 2.5 };
  history.requests[0].resolve([corrected, ...block(1001)]);
  history.pages[0].resolve([...block(800), candle(1000), candle(800)]);
  await tick();
  assert.equal(history.bars.length, 401, "older overlap deduplicates and live last200 merges");
  assert.equal(history.bars[0].time, 800);
  assert.equal(history.bars.find(bar => bar.time === 1000).close, 2.5, "history cannot overwrite newer correction");
  assert.equal(history.range.from, 209, "prepend shifts logical viewport to same time");
  assert.equal(history.range.to, 309);
  history.pages[1].resolve(block(800));
  await tick();
  assert.equal(history.range.from, 209, "shared comparison timeline does not double-shift");
  history.requests[0].resolve([candle(1202), candle(1201), corrected, candle(1202)]);
  assert.equal(history.bars.length, 403);
  assert.equal(history.bars[0].time, 800, "live refresh never discards history");
  assert.equal(history.range.from, 209, "live append away from right edge preserves viewport");
  assert.equal(history.fits, 1, "history and live refresh never refit");
  history.pan({ from: 0, to: 100 });
  await tick();
  assert.equal(history.pages.length, 4);
  history.pages[2].resolve(block(800));
  history.pages[3].resolve([]);
  await tick();
  history.pan({ from: -10, to: 90 });
  await tick();
  assert.equal(history.pages.length, 4, "identical and empty pages exhaust independently");
  const wheel = history.wheel({ ctrlKey: true });
  assert(wheel.prevented && wheel.stopped, "ctrl-wheel cannot reach page zoom or vendor price scaling");
  assert(history.range.to - history.range.from > 100, "wheel changes time span only");
  history.events.pagehide();

  for (const reset of ["unmap", "remap", "loading", "error"]) {
    const overlay = await harness();
    overlay.context(pair());
    overlay.requests[0].resolve(block(1000));
    overlay.requests[1].resolve(block(800, 400));
    overlay.pan({ from: 250, to: 350 });
    if (reset === "unmap") overlay.context({ active: true, inputs: { bars: mainInput } });
    if (reset === "remap") overlay.context(pair({ revision: 2 }));
    if (reset === "loading") overlay.requests[1].loading();
    if (reset === "error") overlay.requests[1].rejectEmpty(new Error("reset failed"));
    assert.equal(overlay.range.from, 50, `${reset} removes comparison-only slots but preserves main visible time`);
    assert.equal(overlay.range.to, 150);
    assert.equal(overlay.bars.length, 200);
    overlay.events.pagehide();
  }

  for (const invalidate of ["remap", "interval", "pause", "dispose", "reset"]) {
    const stale = await harness();
    stale.context(source(1));
    stale.requests[0].resolve(block(1000));
    stale.pan({ from: 0, to: 100 });
    await tick();
    if (invalidate === "remap") stale.context(source(2));
    if (invalidate === "interval") stale.selectInterval("5m");
    if (invalidate === "pause") stale.context({ ...source(1), active: false });
    if (invalidate === "dispose") stale.events.pagehide();
    if (invalidate === "reset") stale.requests[0].loading();
    const before = JSON.stringify(stale.bars);
    stale.pages[0].resolve(block(800));
    await tick();
    assert.equal(JSON.stringify(stale.bars), before, `${invalidate} ignores stale history`);
    if (invalidate !== "dispose") stale.events.pagehide();
  }

  const retry = await harness();
  retry.context(source(1));
  retry.requests[0].resolve(block(1000));
  retry.pan({ from: 0, to: 100 });
  await tick();
  retry.pages[0].reject(new Error("offline"));
  await tick();
  assert.match(retry.elements.description.textContent, /Pan again to retry/);
  retry.pan({ from: 0, to: 100 });
  await tick();
  assert.equal(retry.pages.length, 1, "failed requests do not tightly retry");
  retry.advance(1001);
  retry.pan({ from: 0, to: 100 });
  await tick();
  assert.equal(retry.pages.length, 2, "a later gesture explicitly retries after backoff");
  retry.pages[1].resolve(block(-9000, 10000));
  await tick();
  assert.equal(retry.bars.length, 10000, "history has a hard retention cap");
  retry.pan({ from: -100, to: 0 });
  await tick();
  assert.equal(retry.pages.length, 2, "cap stops further history demand");
  retry.events.pagehide();

  const standalone = await harness(false);
  assert.equal(standalone.options.interval, "1d");
  assert.equal(JSON.stringify(standalone.options.intervals), '["1d"]');
  assert(standalone.options.canSelectInterval("1d"));
  assert.equal(standalone.options.canSelectInterval("1h"), false);
  assert.equal(standalone.bars.length, 240);
  assert.equal(standalone.comparisonSeriesOptions.length, 0, "unmapped charts do not create an overlay");
  assert.equal(standalone.elements.badge.textContent, "DEMO");
  standalone.window.TradeSlopeChart.setSymbol("LABEL");
  assert.equal(standalone.bars.length, 240);
  standalone.window.TradeSlopeChart.setTheme("light");
  assert.equal(standalone.window.TradeSlopeChart.getState().theme, "light");
  standalone.events.pagehide();
  console.log("OpenAlgo input lifecycle and standalone demo: PASS");
}
async function browserTest() {
  // Optional real Chromium gestures, no project dependency:
  // PLAYWRIGHT_MODULE=/path/to/playwright node extensions/openalgo/live.test.cjs --browser
  const { chromium } = require(process.env.PLAYWRIGHT_MODULE || "playwright");
  const http = require("node:http");
  const path = require("node:path");
  const root = path.resolve("extensions/openalgo");
  const server = http.createServer((req, res) => {
    const file = path.resolve(root, "." + new URL(req.url, "http://localhost").pathname);
    if (!file.startsWith(root + path.sep)) { res.writeHead(403).end(); return; }
    try {
      let body = fs.readFileSync(file);
      if (file.endsWith("/card.js")) body = Buffer.concat([Buffer.from(`
        const originalCreate = OpenAlgoCharts.widget.createWidget;
        OpenAlgoCharts.widget.createWidget = (...args) => window.testWidget = originalCreate(...args);
      `), body]);
      res.setHeader("Content-Type", file.endsWith(".js") ? "text/javascript" : file.endsWith(".css") ? "text/css" : "text/html");
      res.end(body);
    } catch { res.writeHead(404).end(); }
  });
  await new Promise(resolve => server.listen(0, "127.0.0.1", resolve));
  let browser;
  try {
    browser = await chromium.launch({ executablePath: process.env.CHROMIUM || "/opt/homebrew/bin/chromium", headless: true });
    const page = await browser.newPage({ viewport: { width: 1200, height: 800 } });
    await page.addInitScript(() => {
      window.testRequests = [];
      window.TradeSlope = {
        context: { active: true, inputs: { bars: { connected: true, revision: 1 } } },
        inputs: {
          subscribe(port, params, listener) {
            if (port === "bars") window.testLive = listener;
            else window.testComparison = listener;
            return () => {};
          },
          request(port, params) {
            return new Promise(resolve => window.testRequests.push({ port, params, resolve }));
          },
        },
        onContextChange(listener) { window.testContext = listener; listener(this.context); return () => {}; },
      };
    });
    const errors = [];
    page.on("pageerror", error => errors.push(error.message));
    await page.goto(`http://127.0.0.1:${server.address().port}/index.html`);
    await page.evaluate(async () => {
      await TradeSlopeChart.ready;
      window.testBars = Array.from({ length: 200 }, (_, i) => ({
        time: 1700000000 + i * 3600, open: 100, high: 102, low: 98, close: 101, volume: 10,
      }));
      testLive(testBars, undefined, { status: "live", hasValue: true });
      testWidget.chart.setVisibleLogicalRange({ from: 80, to: 180 });
    });
    const box = await page.locator(".oac-chart").boundingBox();
    const range = () => page.evaluate(() => testWidget.chart.getVisibleLogicalRange());
    const beforeDrag = await range();
    await page.mouse.move(box.x + 300, box.y + 200);
    await page.mouse.down();
    await page.mouse.move(box.x + 500, box.y + 200, { steps: 15 });
    await page.mouse.up();
    const afterDrag = await range();
    assert(afterDrag.from < beforeDrag.from - 10, "native pointer drag pans older");
    // Freeze price scale so a wheel-induced price change cannot be mistaken for
    // normal autoscale as the visible candles change.
    const priceBefore = await page.evaluate(() => {
      const scale = testWidget.series.priceScale();
      scale.setAutoScale(false);
      return scale.priceRange();
    });
    const beforeWheel = await range();
    await page.mouse.move(box.x + box.width - 8, box.y + 200);
    await page.mouse.wheel(0, 120);
    await page.waitForTimeout(100);
    const afterWheel = await range();
    assert(afterWheel.to - afterWheel.from > beforeWheel.to - beforeWheel.from, "real wheel over price axis zooms time");
    assert.deepEqual(await page.evaluate(() => testWidget.series.priceScale().priceRange()), priceBefore, "wheel never scales price");
    await page.keyboard.down("Control");
    await page.mouse.wheel(0, 100);
    await page.keyboard.up("Control");
    await page.waitForTimeout(100);
    assert.equal(await page.evaluate(() => visualViewport.scale), 1, "ctrl-wheel does not zoom the page");
    await page.evaluate(() => testWidget.chart.setVisibleLogicalRange({ from: 35, to: 135 }));
    await page.mouse.move(box.x + 300, box.y + 200);
    await page.mouse.down();
    await page.mouse.move(box.x + 500, box.y + 200, { steps: 15 });
    await page.mouse.up();
    await page.waitForFunction(() => testRequests.length > 0);
    const evidence = await page.evaluate(async () => {
      const chart = testWidget.chart;
      const anchor = () => chart.timeToCoordinate(testBars[80].time);
      const before = anchor();
      const request = testRequests[0];
      request.resolve(Array.from({ length: 200 }, (_, i) => ({
        ...testBars[0], time: testBars[0].time - (200 - i) * 3600,
      })));
      await new Promise(resolve => setTimeout(resolve, 50));
      const after = anchor();
      testLive(testBars.map(bar => ({ ...bar, close: 100.5 })), undefined, { status: "live", hasValue: true });
      return { params: request.params, before, after, retained: testWidget.series.getData().length };
    });
    assert.equal(evidence.params.end, 1699999999);
    assert.equal(evidence.params.limit, 200);
    assert(Math.abs(evidence.before - evidence.after) < 0.01, "real vendor prepend preserves candle pixel coordinate");
    assert.equal(evidence.retained, 400, "live last200 retains older vendor data");
    const liveFollow = await page.evaluate(() => {
      const chart = testWidget.chart;
      chart.setVisibleLogicalRange({ from: 303, to: 403 });
      const before = chart.getVisibleLogicalRange();
      testLive([{ ...testBars.at(-1), time: testBars.at(-1).time + 3600 }], undefined, { status: "live", hasValue: true });
      const after = chart.getVisibleLogicalRange();
      return { before, after };
    });
    assert.equal(liveFollow.after.to, liveFollow.before.to + 1, "actual vendor update auto-follows at the right edge");
    const comparisonClear = await page.evaluate(() => {
      const chart = testWidget.chart;
      testContext({ active: true, inputs: { bars: { connected: true, revision: 1 },
        comparison: { connected: true, revision: 1 } } });
      testComparison(Array.from({ length: 200 }, (_, i) => ({
        ...testBars[0], time: testBars[0].time - (400 - i) * 3600,
      })), undefined, { status: "live", hasValue: true });
      chart.setVisibleLogicalRange({ from: 450, to: 550 });
      const before = chart.timeToCoordinate(testBars[80].time);
      testContext({ active: true, inputs: { bars: { connected: true, revision: 1 } } });
      return { before, after: chart.timeToCoordinate(testBars[80].time) };
    });
    assert(Math.abs(comparisonClear.before - comparisonClear.after) < 0.01,
      "real vendor comparison unmap preserves main candle pixel coordinate");
    assert.deepEqual(errors, []);
    console.log("Chromium native pointer/wheel/history evidence: PASS", JSON.stringify({ beforeDrag, afterDrag, beforeWheel, afterWheel, ...evidence, liveFollow, comparisonClear }));
  } finally {
    await browser?.close();
    await new Promise(resolve => server.close(resolve));
  }
}
test().then(() => process.argv.includes("--browser") && browserTest())
  .catch(error => { console.error(error); process.exitCode = 1; });
