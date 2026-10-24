const assert = require("node:assert/strict");
const fs = require("node:fs");
const vm = require("node:vm");

async function test() {
  const manifest = JSON.parse(fs.readFileSync("examples/extensions/quote/extension.json", "utf8"));
  assert.equal(manifest.schema_version, 2);
  assert.equal(manifest.card.backend_operations, undefined);
  const port = manifest.card.inputs[0];
  assert.equal(port.id, "quote");
  assert.equal(port.kind, "query");
  assert.equal(port.required, true);
  assert.deepEqual(port.parameters, { type: "object", fields: {} });
  for (const name of ["symbol", "last", "bid", "ask", "volume"]) {
    assert.equal(port.output.fields[name].required, true);
  }
  for (const name of ["bid", "ask", "volume"]) {
    assert.deepEqual(port.output.fields[name].schema, { type: "nullable", inner: { type: "number" } });
  }
  const comparisonPort = manifest.card.inputs[1];
  assert.equal(comparisonPort.id, "comparison");
  assert.equal(comparisonPort.kind, "query");
  assert.equal(comparisonPort.required, false);
  assert.deepEqual(comparisonPort.parameters, port.parameters);
  assert.deepEqual(comparisonPort.output, port.output);

  const output = { textContent: "" };
  const comparison = { textContent: "", hidden: true };
  const requests = [];
  let listener, close;
  const TradeSlope = {
    inputs: { subscribe(port, params, callback) {
      const request = { port, params, stopped: false,
        resolve: value => callback(value, undefined, { status: "live", hasValue: true }),
        reject: error => callback(undefined, error, { status: "error", hasValue: true }),
        rejectEmpty: error => callback(undefined, error, { status: "error", hasValue: false }),
        loading: () => callback(null, undefined, { status: "loading", hasValue: false }) };
      requests.push(request);
      return () => { request.stopped = true; };
    } },
    onContextChange(callback) {
      listener = callback;
      callback({ active: true, inputs: {} });
      return () => { listener = null; };
    },
  };
  vm.runInNewContext(fs.readFileSync("examples/extensions/quote/quote.js", "utf8"), {
    TradeSlope,
    document: { getElementById: id => id === "quote" ? output : comparison },
    window: { addEventListener: (_, callback) => { close = callback; } },
    setInterval: () => { throw new Error("frontend must not poll"); },
  });
  assert.match(output.textContent, /Choose a data source/);
  assert.equal(comparison.hidden, true, "unbound optional input is hidden");
  const context = revision => ({ active: true, inputs: { quote: { connected: true, revision } } });
  listener(context(1));
  listener(context(1));
  assert.equal(requests.length, 1);
  assert.equal(requests[0].port, "quote");
  assert.equal(JSON.stringify(requests[0].params), "{}");
  listener(context(2));
  assert(requests[0].stopped);
  requests[0].resolve({ symbol: "OLD" });
  await new Promise(setImmediate);
  assert.equal(output.textContent, "Loading Quote input…");
  requests[1].resolve({ symbol: "NEW", last: 2, bid: null, ask: null, volume: null });
  await new Promise(setImmediate);
  assert.equal(JSON.parse(output.textContent).symbol, "NEW");
  requests[1].resolve({ symbol: "PUSH", last: 3 });
  assert.equal(JSON.parse(output.textContent).symbol, "PUSH", "same subscription delivers changes");
  requests[1].reject(new Error("offline"));
  assert.equal(output.textContent, "offline");
  listener({ ...context(2), active: false });
  assert(requests[1].stopped);
  assert.equal(requests.length, 2);
  listener({ active: true, inputs: {} });
  assert.match(output.textContent, /Choose a data source/);
  listener(context("revision"));
  const dual = revision => ({ ...context("revision"), inputs: {
    ...context("revision").inputs, comparison: { connected: true, revision },
  } });
  listener(dual(1));
  assert.equal(requests.length, 4);
  assert.equal(requests[2].port, "quote");
  assert.equal(requests[3].port, "comparison");
  assert.equal(requests[2].stopped, false, "mapping comparison does not restart primary");
  requests[2].resolve({ symbol: "PRIMARY", last: 10 });
  requests[3].resolve({ symbol: "SECONDARY", last: 20 });
  assert.equal(JSON.parse(output.textContent).symbol, "PRIMARY");
  assert.equal(comparison.textContent, "Comparison · SECONDARY: 20");
  assert.equal(comparison.hidden, false);
  requests[2].loading();
  requests[3].loading();
  assert.equal(output.textContent, "Loading Quote input…", "reset clears primary account data");
  assert.equal(comparison.textContent, "Loading Comparison input…", "reset clears secondary account data");
  requests[2].reject(new Error("primary reset offline"));
  requests[3].reject(new Error("secondary reset offline"));
  assert.equal(output.textContent, "primary reset offline");
  assert.equal(comparison.textContent, "secondary reset offline");
  requests[2].resolve({ symbol: "PRIMARY", last: 10 });
  requests[3].resolve({ symbol: "SECONDARY", last: 20 });
  requests[2].rejectEmpty(new Error("primary fast reset failure"));
  requests[3].rejectEmpty(new Error("secondary fast reset failure"));
  assert.equal(output.textContent, "primary fast reset failure", "coalesced reset error clears primary");
  assert.equal(comparison.textContent, "secondary fast reset failure", "coalesced reset error clears comparison");
  requests[2].resolve({ symbol: "PRIMARY", last: 10 });
  requests[3].resolve({ symbol: "SECONDARY", last: 20 });
  listener(dual(2));
  assert.equal(requests[3].stopped, true);
  assert.equal(requests[2].stopped, false);
  requests[3].resolve({ symbol: "OLD", last: 0 });
  requests[4].reject(new Error("comparison offline"));
  assert.equal(comparison.textContent, "comparison offline");
  assert.equal(JSON.parse(output.textContent).last, 10, "comparison error does not affect primary");
  listener({ ...dual(2), active: false });
  assert(requests[2].stopped && requests[4].stopped, "pause cleans up both ports");
  listener(dual(2));
  assert.equal(requests.length, 7, "resume subscribes both ports");
  requests[5].resolve({ symbol: "PRIMARY", last: 11 });
  listener(context("revision"));
  assert(requests[6].stopped, "unmapping optional input cancels only its subscription");
  assert.equal(requests[5].stopped, false);
  assert.equal(comparison.hidden, true);
  close();
  assert(requests[5].stopped);
  requests[2].reject(new Error("late failure"));
  await new Promise(setImmediate);
  assert.equal(listener, null);
  assert.equal(JSON.parse(output.textContent).last, 11);
  console.log("Quote input manifest and lifecycle: PASS");
}
test().catch(error => { console.error(error); process.exitCode = 1; });
