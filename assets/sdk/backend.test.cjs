const assert = require("node:assert/strict");
const vm = require("node:vm");
const fs = require("node:fs");

async function test() {
  const requests = [];
  const events = {};
  const timers = new Map();
  const callbackErrors = [];
  let nextTimer = 0;
  const window = {
    ipc: { postMessage: message => requests.push(JSON.parse(message)) },
    addEventListener: (name, callback) => { events[name] = callback; },
    __tradeslopeInitialContext: { active: true, theme: "dark", inputs: {} },
  };
  const sandbox = {
    window, console: { ...console, error: error => callbackErrors.push(error) },
    setTimeout: (callback, delay) => { assert.equal(delay, 35000); const id = ++nextTimer; timers.set(id, callback); return id; },
    clearTimeout: id => timers.delete(id),
  };
  vm.runInNewContext(fs.readFileSync("assets/sdk/backend.js", "utf8"), sandbox);
  const api = window.TradeSlope;
  const beforeConnect = api.inputs.subscribe("quote", {}, () => {});
  assert.equal(requests.length, 0, "calls wait for the host document handshake");
  beforeConnect();
  window.__tradeslopeConnect(1);
  window.__tradeslopeConnect(2);
  assert.equal(api.version, 2);
  assert.equal(api.backend, undefined, "no raw request compatibility API");
  let context = null;
  const stop = api.onContextChange(next => { context = next; });
  assert.equal(context, window.__tradeslopeInitialContext, "subscription is immediate");
  window.__tradeslopeSetContext({ active: true, theme: "light", inputs: { quote: { connected: true, revision: 1 } } });
  assert.equal(context.inputs.quote.connected, true);
  stop();

  const result = api.inputs.request("quote");
  assert.equal(requests[0].version, 2);
  assert.deepEqual(Object.keys(requests[0]).sort(), ["version", "session", "document", "activity", "id", "port", "params"].sort());
  assert.equal(requests[0].document, 1, "an old document cannot adopt another generation");
  assert.equal(requests[0].port, "quote");
  assert.deepEqual(requests[0].params, {});
  assert.equal(typeof requests[0].session, "string");
  let resolved = false;
  result.then(() => { resolved = true; });
  window.__tradeslopeBackendResponse({ version: 2, session: "old-document", id: requests[0].id, result: { last: 9 } });
  window.__tradeslopeBackendResponse({ version: 1, session: requests[0].session, id: requests[0].id, result: { last: 9 } });
  await Promise.resolve();
  assert.equal(resolved, false, "old document responses must not resolve current promises");
  window.__tradeslopeBackendResponse({ version: 2, session: requests[0].session, id: requests[0].id, result: { last: 42 } });
  assert.equal((await result).last, 42);
  assert.equal(timers.size, 0);

  const denied = api.inputs.request("submit", { amount: 2 });
  window.__tradeslopeBackendResponse({
    version: 2, session: requests[1].session, id: requests[1].id,
    error: { code: "permission_denied", message: "Read-only card" },
  });
  await assert.rejects(denied, error => error.code === "permission_denied");

  const timeout = api.inputs.request("quote");
  const timedOut = assert.rejects(timeout, /timed out/);
  // Model the timer firing (native timers remove themselves on invocation).
  const [timerId, callback] = [...timers][0];
  timers.delete(timerId);
  callback();
  await timedOut;
  assert.equal(timers.size, 0);
  const postMessage = window.ipc.postMessage;
  window.ipc.postMessage = () => { throw new Error("IPC failed"); };
  await assert.rejects(api.inputs.request("quote"), /IPC failed/);
  assert.equal(timers.size, 0);
  window.ipc.postMessage = postMessage;
  const updates = [];
  const offLive = api.inputs.subscribe("quote", {}, (value, error, metadata) => updates.push({ value, error, metadata }));
  const liveRequest = requests.at(-1);
  assert.equal(liveRequest.op, "subscribe");
  assert.equal(timers.size, 0, "live queries have no frontend refresh timers");
  const push = payload => window.__tradeslopeBackendResponse({
    version: 2, session: liveRequest.session, id: liveRequest.id, op: "subscription", ...payload,
  });
  push({ session: "old-document", result: 1 });
  push({ version: 1, result: 1 });
  assert.equal(updates.length, 0);
  push({ result: { last: 10 } });
  push({ error: { code: "offline", message: "Disconnected" } });
  push({ result: { last: 11 } });
  assert.equal(updates[0].value.last, 10);
  assert.equal(updates[1].value, undefined);
  assert.equal(updates[1].error.code, "offline");
  assert.equal(updates[2].value.last, 11);
  assert.equal(updates[2].metadata.status, "live");
  push({ result: null, error: null, status: "loading", hasValue: false });
  assert.equal(updates[3].value, null, "status-only reset snapshots reach the listener");
  assert.equal(updates[3].error, undefined);
  assert.equal(updates[3].metadata.status, "loading");
  assert.equal(updates[3].metadata.hasValue, false);
  push({ result: null, status: "error", error: { code: "offline", message: "Reset account offline" } });
  assert.equal(updates[4].metadata.status, "error");
  assert.equal(updates[4].error.message, "Reset account offline");
  assert.equal(updates[4].metadata.hasValue, false);
  push({ result: null, error: null, status: "live", hasValue: true });
  assert.equal(updates[5].value, null);
  assert.equal(updates[5].metadata.hasValue, true, "valid JSON null is distinct from absent data");
  offLive();
  assert.equal(requests.at(-1).op, "unsubscribe");
  const afterOff = requests.length;
  offLive();
  assert.equal(requests.length, afterOff, "unsubscribe is idempotent");
  push({ result: 12 });
  assert.equal(updates.length, 6, "late updates cannot reach unsubscribed listeners");
  const errors = [];
  api.inputs.subscribe("", {}, (_, error) => errors.push(error));
  assert.match(errors[0].message, /nonempty/);
  window.ipc.postMessage = () => { throw new Error("live IPC failed"); };
  api.inputs.subscribe("quote", {}, (_, error) => errors.push(error));
  assert.match(errors[1].message, /live IPC failed/);
  window.ipc.postMessage = postMessage;
  assert.throws(() => api.inputs.subscribe("quote", {}, null), /Listener must be a function/);
  api.inputs.subscribe("", {}, () => { throw new Error("Immediate listener failure"); });
  assert.match(callbackErrors[0].message, /Immediate listener failure/);
  const offThrowing = api.inputs.subscribe("quote", {}, () => { throw new Error("Listener failure"); });
  window.__tradeslopeBackendResponse({ ...requests.at(-1), op: "subscription", result: 1 });
  assert.match(callbackErrors[1].message, /Listener failure/);
  offThrowing();
  const cyclic = {};
  cyclic.self = cyclic;
  api.inputs.subscribe("quote", cyclic, (_, error) => errors.push(error));
  assert.match(errors.at(-1).message, /circular/i);
  const capacityErrorIndex = errors.length;
  for (let i = 0; i < 32; i++) {
    api.inputs.subscribe("action", {}, (_, error) => assert.equal(error.code, "not_query"));
    const rejected = requests.at(-1);
    window.__tradeslopeBackendResponse({
      ...rejected, op: "subscription", terminal: true,
      error: { code: "not_query", message: "Only queries can be subscribed" },
    });
  }
  const liveStops = Array.from({ length: 32 }, () => api.inputs.subscribe("quote", {}, () => {}));
  api.inputs.subscribe("quote", {}, (_, error) => errors.push(error));
  assert.match(errors[capacityErrorIndex].message, /Too many live/);
  liveStops[0]();
  let hiddenUpdates = 0;
  api.inputs.subscribe("quote", {}, () => { hiddenUpdates++; });
  const hiddenRequest = requests.at(-1);
  const pending = Array.from({ length: 32 }, () => api.inputs.request("quote"));
  // Install rejection handlers before pagehide rejects the requests.
  const settled = Promise.allSettled(pending);
  await assert.rejects(api.inputs.request("quote"), /Too many pending/);
  events.pagehide();
  assert.equal(requests.at(-1).op, "close");
  window.__tradeslopeBackendResponse({ ...hiddenRequest, op: "subscription", result: 42 });
  assert.equal(hiddenUpdates, 0, "pagehide clears live listeners");
  api.inputs.subscribe("quote", {}, (_, error) => errors.push(error));
  assert.match(errors.at(-1).message, /Card closed/);
  assert((await settled).every(result => result.status === "rejected"));
  assert.equal(timers.size, 0);
  await assert.rejects(api.inputs.request("quote"), /Card closed/);
  const oldContext = context;
  window.__tradeslopeSetContext({ inputs: {} });
  assert.equal(context, oldContext, "unsubscribed listeners are not called");
  const otherRequests = [];
  const otherWindow = {
    crypto: { randomUUID: () => "new-document-nonce" },
    ipc: { postMessage: message => otherRequests.push(JSON.parse(message)) },
    addEventListener() {},
  };
  vm.runInNewContext(fs.readFileSync("assets/sdk/backend.js", "utf8"), { ...sandbox, window: otherWindow });
  const other = otherWindow.TradeSlope.inputs.request("quote");
  assert.equal(otherRequests.length, 0, "startup requests queue until authenticated by host");
  otherWindow.__tradeslopeConnect(2);
  assert.equal(otherRequests[0].document, 2);
  assert.equal(otherRequests[0].id, 1, "IDs restart per document");
  assert.notEqual(otherRequests[0].session, requests[0].session);
  let otherResolved = false;
  other.then(() => { otherResolved = true; });
  otherWindow.__tradeslopeBackendResponse({ ...requests[0], result: 99 });
  await Promise.resolve();
  assert.equal(otherResolved, false, "a real prior-document response cannot cross sessions");
  otherWindow.__tradeslopeBackendResponse({ ...otherRequests[0], result: 7 });
  assert.equal(await other, 7);
  assert.equal(timers.size, 0);
  const startupRequests = [];
  let startupClose;
  const startupWindow = {
    ipc: { postMessage: message => startupRequests.push(JSON.parse(message)) },
    addEventListener: (_, callback) => { startupClose = callback; },
  };
  vm.runInNewContext(fs.readFileSync("assets/sdk/backend.js", "utf8"), { ...sandbox, window: startupWindow });
  const startupPending = startupWindow.TradeSlope.inputs.request("quote");
  const startupRejected = assert.rejects(startupPending, /Card closed/);
  startupWindow.TradeSlope.inputs.subscribe("quote", {}, () => assert.fail("closed startup listener"));
  startupClose();
  startupWindow.__tradeslopeConnect(3);
  await startupRejected;
  assert.equal(startupRequests.length, 0, "pagehide before handshake drops all queued messages");
  assert.equal(timers.size, 0);

  // An external card has no context handler at all. Model host token admission
  // and updates, including scripts queued before suspension.
  const sent = [];
  const tokens = new Map();
  let hidden = false;
  let epoch = 0;
  let executed = 0;
  let close;
  const externalWindow = {
    ipc: { postMessage: json => {
      const message = JSON.parse(json);
      sent.push(message);
      if (message.op === "unsubscribe") tokens.delete(message.id);
      else if (message.op === "close") tokens.clear();
      else {
        assert.equal(hidden, false, "SDK must not send hidden data operations");
        assert.equal(message.activity, epoch);
        if (message.op === "subscribe") {
          assert(!tokens.has(message.id));
          tokens.set(message.id, message);
        } else executed++;
      }
    } },
    addEventListener: (_, callback) => { close = callback; },
  };
  vm.runInNewContext(fs.readFileSync("assets/sdk/backend.js", "utf8"), { ...sandbox, window: externalWindow });
  const external = externalWindow.TradeSlope;
  const deliveries = [];
  const params = { symbol: "original" };
  const offExternal = external.inputs.subscribe("quote", params, (value, error, metadata) => deliveries.push({ value, error, metadata }));
  params.symbol = "mutated";
  const queuedAction = external.inputs.request("submit", { amount: 1 });
  const actionRejected = assert.rejects(queuedAction, error => error.code === "inactive");
  const setActive = active => {
    if (!active && !hidden) { epoch++; tokens.clear(); }
    hidden = !active;
    externalWindow.__tradeslopeSetContext({ active, inputs: {} }, epoch);
  };
  setActive(false);
  externalWindow.__tradeslopeConnect(10);
  await actionRejected;
  assert.equal(sent.length, 0, "startup suspension discards queued subscriptions and Actions");
  for (const port of ["private", "submit"]) {
    await assert.rejects(external.inputs.request(port), error => error.code === "inactive");
  }
  assert.equal(executed, 0);
  setActive(true);
  assert.equal(tokens.size, 1);
  const first = [...tokens.values()][0];
  assert.equal(first.params.symbol, "original", "resume uses immutable subscription parameters");
  const deliver = (wire, payload) => externalWindow.__tradeslopeBackendResponse({ ...wire, op: "subscription", ...payload });
  deliver(first, { result: 42 });
  assert.equal(deliveries.at(-1).value, 42);
  const inflight = external.inputs.request("private");
  const inflightRejected = assert.rejects(inflight, error => error.code === "inactive");
  const oldRequest = sent.at(-1);
  setActive(false);
  await inflightRejected;
  assert.equal(tokens.size, 0);
  assert.equal(sent.at(-1).op, "unsubscribe");
  const deliveryCount = deliveries.length;
  deliver(first, { result: 99 });
  const hiddenOff = external.inputs.subscribe("other", {}, () => assert.fail("removed hidden listener"));
  hiddenOff();
  setActive(false);
  setActive(true);
  setActive(true);
  assert.equal(tokens.size, 1, "repeated contexts cannot duplicate a host subscription");
  const resumed = [...tokens.values()][0];
  assert.notEqual(resumed.id, first.id);
  deliver(first, { terminal: true, error: { code: "disconnected", message: "old token" } });
  deliver(first, { result: 100 });
  externalWindow.__tradeslopeBackendResponse({ ...oldRequest, result: "stale private result" });
  assert.equal(deliveries.length, deliveryCount, "stale wire IDs never deliver or terminate resumed listeners");
  deliver(resumed, { result: null, status: "loading", hasValue: false });
  assert.equal(deliveries.at(-1).metadata.hasValue, false);
  deliver(resumed, { result: 43 });
  assert.equal(deliveries.at(-1).value, 43);
  assert.equal(executed, 1, "only the active private request ran; no request was replayed");
  offExternal();
  assert.equal(tokens.size, 0);

  // Builtin consumers still manually replace listeners on context changes.
  let manualOff = () => {};
  const stopManual = external.onContextChange(next => {
    manualOff();
    manualOff = next.active === false ? () => {} : external.inputs.subscribe("quote", {}, () => {});
  });
  assert.equal(tokens.size, 1);
  setActive(false);
  setActive(true);
  assert.equal(tokens.size, 1, "manual off/on plus central resume creates exactly one token");
  const beforeClose = [...tokens.values()][0];
  close();
  assert.equal(tokens.size, 0);
  deliver(beforeClose, { result: 55 });
  setActive(true);
  externalWindow.__tradeslopeConnect(11);
  assert.equal(tokens.size, 0, "pagehide cannot reconnect or resume the closed session");
  stopManual();
  assert.equal(timers.size, 0);

  const replayed = [];
  const replayWindow = {
    ipc: { postMessage: json => replayed.push(JSON.parse(json)) },
    addEventListener() {},
  };
  vm.runInNewContext(fs.readFileSync("assets/sdk/backend.js", "utf8"), { ...sandbox, window: replayWindow });
  const startupAction = replayWindow.TradeSlope.inputs.request("submit");
  const startupActionRejected = assert.rejects(startupAction, error => error.code === "inactive");
  replayWindow.TradeSlope.inputs.subscribe("quote", {}, () => {});
  // Host hid and resumed while loading; only the latest context is replayed.
  replayWindow.__tradeslopeSetContext({ active: true, inputs: {} }, 2);
  replayWindow.__tradeslopeConnect(12);
  await startupActionRejected;
  assert.equal(replayed.length, 1);
  assert.equal(replayed[0].op, "subscribe", "context replay must not replay pre-suspension Actions");
  assert.equal(replayed[0].activity, 2);
  assert.equal(timers.size, 0);

  const remapMessages = [];
  const remapWindow = {
    ipc: { postMessage: json => remapMessages.push(JSON.parse(json)) },
    addEventListener() {},
    __tradeslopeInitialContext: { active: true, inputs: {
      primary: { connected: true, revision: 1 },
      comparison: { connected: true, revision: 1 },
    } },
  };
  vm.runInNewContext(fs.readFileSync("assets/sdk/backend.js", "utf8"), { ...sandbox, window: remapWindow });
  remapWindow.__tradeslopeConnect(20);
  const remapApi = remapWindow.TradeSlope;
  const primaryUpdates = [];
  const comparisonUpdates = [];
  remapApi.inputs.subscribe("primary", {}, value => primaryUpdates.push(value));
  remapApi.inputs.subscribe("comparison", {}, value => comparisonUpdates.push(value));
  const [primaryWire, comparisonWire] = remapMessages;
  const remapContext = (revision, connected = true) => remapWindow.__tradeslopeSetContext({
    active: true, inputs: {
      primary: { connected: true, revision: 1 },
      comparison: { connected, revision },
    },
  });
  const remapPush = (wire, payload) => remapWindow.__tradeslopeBackendResponse({ ...wire, op: "subscription", ...payload });
  remapPush(primaryWire, { result: "primary" });
  remapPush(comparisonWire, { result: "old comparison" });
  remapContext(2);
  assert.equal(remapMessages.length, 4, "remapping one port only detaches and resubscribes that port");
  assert.equal(remapMessages[2].op, "unsubscribe");
  assert.equal(remapMessages[2].id, comparisonWire.id);
  const newComparisonWire = remapMessages[3];
  assert.equal(newComparisonWire.port, "comparison");
  assert.notEqual(newComparisonWire.id, comparisonWire.id);
  remapPush(comparisonWire, { result: "stale comparison" });
  remapPush(comparisonWire, { terminal: true, error: { code: "disconnected", message: "old" } });
  remapPush(newComparisonWire, { result: "new comparison" });
  remapPush(primaryWire, { result: "same primary wire" });
  assert.deepEqual(primaryUpdates, ["primary", "same primary wire"]);
  assert.deepEqual(comparisonUpdates, ["old comparison", "new comparison"]);
  remapContext(2);
  assert.equal(remapMessages.length, 4, "unchanged per-port revisions keep both wires");
  remapContext(3, false);
  assert.equal(remapMessages.length, 5, "disconnected port retains its listener without resubscribing");
  remapContext(4);
  assert.equal(remapMessages.length, 6);
  remapPush(remapMessages[5], { result: "reconnected comparison" });
  assert.equal(comparisonUpdates.at(-1), "reconnected comparison");

  let manualRemapOff = () => {};
  let manualRevision;
  remapApi.onContextChange(next => {
    const revision = next.inputs.comparison.revision;
    if (revision === manualRevision) return;
    manualRevision = revision;
    manualRemapOff();
    manualRemapOff = remapApi.inputs.subscribe("comparison", {}, () => {});
  });
  const beforeRemap = remapMessages.length;
  remapContext(5);
  const remapOps = remapMessages.slice(beforeRemap);
  assert.equal(remapOps.filter(message => message.op === "unsubscribe").length, 2);
  assert.equal(remapOps.filter(message => message.op === "subscribe").length, 2,
    "external listener and manually replaced listener each get exactly one host subscription");
  assert(remapOps.every(message => message.port === "comparison" || message.op === "unsubscribe"));
  console.log("JavaScript frontend SDK: PASS");
}
test().catch(error => { console.error(error); process.exitCode = 1; });
