/* TradeSlope JavaScript frontend SDK v2. Frontends request declared input ports. */
(() => {
  "use strict";
  let nextId = 1;
  // IDs restart on navigation; a per-document nonce prevents an old response
  // from resolving a different request in a reloaded card.
  const session = window.crypto?.randomUUID?.() ||
    `${Date.now().toString(36)}-${Math.random().toString(36).slice(2)}-${Math.random().toString(36).slice(2)}`;
  let context = window.__tradeslopeInitialContext || { active: true, theme: "dark", inputs: {} };
  let closed = false;
  let documentGeneration;
  let activity = 0;
  const pending = new Map();
  const subscriptions = new Set();
  const wires = new Map();
  const outbound = new Map();
  const listeners = new Set();

  function post(message) {
    // Validate serialization now, even while waiting for the host handshake.
    const json = JSON.stringify(message);
    if (documentGeneration === undefined) outbound.set(message.id, JSON.parse(json));
    else window.ipc.postMessage(JSON.stringify({ ...message, document: documentGeneration, activity }));
  }

  Object.defineProperty(window, "__tradeslopeConnect", { value: generation => {
    if (closed || documentGeneration !== undefined) return;
    documentGeneration = generation;
    for (const message of outbound.values()) {
      try { post(message); }
      catch (error) {
        const entry = wires.get(message.id);
        if (entry) {
          wires.delete(message.id);
          subscriptions.delete(entry);
          notify(entry.listener, undefined, error);
        }
        const call = pending.get(message.id);
        if (call) { clearTimeout(call.timer); pending.delete(message.id); call.reject(error); }
      }
    }
    outbound.clear();
  } });

  function backendError(detail) {
    const error = new Error(detail.message);
    error.code = detail.code;
    return error;
  }

  // Live callbacks include status metadata so loading/reset can clear old data.
  // Errors (including immediate validation/IPC errors) are delivered safely to
  // the listener, never thrown through the host's response handler.
  function notify(listener, value, error, metadata = { status: "error", hasValue: false }) {
    try { listener(value, error, metadata); } catch (failure) { console.error(failure); }
  }

  function inactiveError() {
    return backendError({ code: "inactive", message: "Card is inactive" });
  }

  function detach(entry) {
    const id = entry.id;
    entry.id = undefined;
    if (!wires.delete(id) || outbound.delete(id)) return;
    try { post({ version: 2, session, id, op: "unsubscribe" }); }
    catch (_) { /* Host suspension/navigation also cancels subscriptions. */ }
  }

  function attach(entry) {
    if (closed || context.active === false || !subscriptions.has(entry) || entry.id !== undefined) return;
    if (context.inputs?.[entry.port]?.connected === false) return;
    entry.id = nextId++;
    wires.set(entry.id, entry);
    try {
      post({ version: 2, session, id: entry.id, op: "subscribe", port: entry.port, params: entry.params });
    } catch (error) {
      wires.delete(entry.id);
      entry.id = undefined;
      subscriptions.delete(entry);
      notify(entry.listener, undefined, error);
    }
  }

  function subscribe(port, params = {}, listener) {
    if (typeof listener !== "function") throw new TypeError("Listener must be a function");
    let entry;
    const off = () => {
      if (!subscriptions.delete(entry)) return;
      detach(entry);
    };
    try {
      if (closed) throw new Error("Card closed");
      if (typeof port !== "string" || !port.trim()) throw new TypeError("Input port must be a nonempty string");
      if (!window.ipc?.postMessage) throw new Error("Input host is unavailable");
      if (subscriptions.size >= 32) throw new Error("Too many live input subscriptions");
      // Snapshot params once: automatic resume must not change the query.
      entry = { port, params: JSON.parse(JSON.stringify(params)), listener };
      subscriptions.add(entry);
      attach(entry);
    } catch (error) {
      subscriptions.delete(entry);
      notify(listener, undefined, error);
    }
    return off;
  }

  function request(port, params = {}) {
    if (closed) return Promise.reject(new Error("Card closed"));
    if (context.active === false) return Promise.reject(inactiveError());
    if (typeof port !== "string" || !port.trim()) return Promise.reject(new TypeError("Input port must be a nonempty string"));
    if (!window.ipc?.postMessage) return Promise.reject(new Error("Input host is unavailable"));
    if (pending.size >= 32) return Promise.reject(new Error("Too many pending input requests"));
    const id = nextId++;
    return new Promise((resolve, reject) => {
      const timer = setTimeout(() => {
        outbound.delete(id);
        pending.delete(id);
        reject(new Error("Input request timed out"));
      }, 35000);
      pending.set(id, { resolve, reject, timer });
      try {
        post({ version: 2, session, id, port, params });
      } catch (error) {
        clearTimeout(timer);
        pending.delete(id);
        reject(error);
      }
    });
  }

  Object.defineProperty(window, "__tradeslopeBackendResponse", { value: response => {
    if (!response || response.version !== 2 || response.session !== session) return;
    if (response.op === "subscription") {
      const entry = wires.get(response.id);
      if (!entry || context.active === false) return;
      if (response.terminal) {
        wires.delete(response.id);
        subscriptions.delete(entry);
        entry.id = undefined;
      }
      notify(entry.listener, response.error ? undefined : response.result,
        response.error ? backendError(response.error) : undefined,
        { status: response.status || (response.error ? "error" : "live"),
          hasValue: response.hasValue ?? (response.result !== undefined && response.result !== null) });
      return;
    }
    const call = pending.get(response.id);
    if (!call) return;
    clearTimeout(call.timer);
    pending.delete(response.id);
    if (response.error) {
      call.reject(backendError(response.error));
    } else {
      call.resolve(response.result);
    }
  } });
  Object.defineProperty(window, "__tradeslopeSetContext", { value: (next, epoch = activity) => {
    if (closed) return;
    const previous = context;
    const suspendedSinceLastContext = epoch !== activity;
    activity = epoch;
    context = next;
    // A page-load replay may skip the inactive context entirely. The host's
    // epoch still invalidates pre-suspension startup Actions and wire IDs.
    if (context.active === false || suspendedSinceLastContext) {
      for (const entry of subscriptions.values()) detach(entry);
      // Drop unsent Actions as well as in-flight results; never replay requests.
      for (const [id, call] of pending) {
        outbound.delete(id);
        clearTimeout(call.timer);
        call.reject(inactiveError());
      }
      pending.clear();
    } else {
      for (const entry of subscriptions.values()) {
        const before = previous.inputs?.[entry.port];
        const after = context.inputs?.[entry.port];
        if (before?.connected !== after?.connected || before?.revision !== after?.revision) detach(entry);
      }
    }
    for (const listener of listeners) {
      try { listener(context); } catch (error) { console.error(error); }
    }
    // Run after context callbacks so existing manual off/on consumers can
    // replace their listeners without briefly creating duplicate host tokens.
    if (context.active !== false) {
      for (const entry of subscriptions.values()) attach(entry);
    }
  } });
  Object.defineProperty(window, "TradeSlope", { value: Object.freeze({
    version: 2,
    get context() { return context; },
    inputs: Object.freeze({ request, subscribe }),
    onContextChange(listener) {
      if (typeof listener !== "function") throw new TypeError("Listener must be a function");
      if (closed) return () => {};
      listeners.add(listener);
      listener(context);
      return () => listeners.delete(listener);
    },
  }) });
  window.addEventListener("pagehide", () => {
    closed = true;
    subscriptions.clear();
    wires.clear();
    outbound.clear();
    try {
      if (documentGeneration !== undefined) post({ version: 2, session, op: "close" });
    }
    catch (_) { /* The host also cancels on navigation. */ }
    for (const call of pending.values()) {
      clearTimeout(call.timer);
      call.reject(new Error("Card closed"));
    }
    pending.clear();
    listeners.clear();
  }, { once: true });
})();
