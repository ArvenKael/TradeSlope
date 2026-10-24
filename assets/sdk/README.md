# JavaScript input API

`TradeSlope.inputs.request(port, params = {})` returns a Promise for one query
result or action result. Actions must continue to use `request`.

`TradeSlope.inputs.subscribe(port, params, listener)` starts a **query-only**
live subscription and returns an idempotent `unsubscribe()` function:

```js
const unsubscribe = TradeSlope.inputs.subscribe("quote", {}, (value, error, metadata) => {
  if (!metadata.hasValue) clearQuote();
  if (metadata.status === "loading") return;
  if (error) showError(error.message);
  else renderQuote(value);
});
```

Callbacks receive `(value, error, { status, hasValue })`, where status is `"loading"`,
`"live"` or `"error"`. Existing two-argument callbacks remain callable, but
frontends must handle loading to avoid retaining data across account/settings
resets. Loading is delivered even when the value is `null`: **clear old data**
before waiting for a new live value. Do not validate/render loading as a value.
Errors after a reset must leave the old account's data cleared.
Also clear old data whenever `hasValue === false`, even on an error: rapid reset
and refresh can coalesce into a single error snapshot without a loading callback.
`hasValue` describes whether the host has a current-generation value, preserving
the distinction between no value and a valid JSON `null` value.

Live callbacks receive a value and no error; error callbacks receive no value
and an `Error`. A transient error may retain same-generation last-good data on
screen, but must never restore data cleared by loading/reset.
Backend errors include `error.code`. Immediate validation, capacity and IPC errors
are delivered to the callback before `subscribe` returns; callback exceptions are
reported to the console, not propagated. A backend error is an update, not an
automatic unsubscribe: a later successful value can recover the display.
An initial host rejection or stopped backend is terminal: the host marks that
response `terminal`, and the SDK releases the listener and its capacity slot.
The returned unsubscribe function remains safe to call.

The host owns refreshing and change detection; there are no frontend polling
timers. The latest changed snapshot is delivered, so slow consumers may skip
intermediate states. Each document can have at most 32 logical live subscriptions.
The SDK automatically suspends subscriptions when `context.active === false`,
retaining listeners without host tokens. On resume it subscribes each retained
listener once, with the original serialized parameters and a fresh wire ID.
Late updates and terminal errors for previous wire IDs are ignored. Repeated
active contexts do not resubscribe. Existing manual context-driven unsubscribe/
subscribe code remains supported; context handlers run before automatic resume.
Unsubscribe permanently removes the logical listener, including while inactive.

While inactive, `request` rejects with `error.code === "inactive"` without
sending IPC, including private queries and Actions. Suspension also rejects
pending requests and discards unsent startup requests. **Requests and Actions
are never automatically replayed on resume.** An Action already admitted by
the host may have executed; suspension cannot undo it, so do not blindly retry.
The host independently cancels subscription tokens and refuses inactive data
operations, even for cards that ignore context. An activity generation prevents
queued pre-suspension operations from executing after resume.

Page hide, navigation and host teardown cancel all host subscriptions.
Input remapping cancels only subscriptions for changed ports; unrelated ports
keep their existing host tokens. The SDK compares each port's `connected` and
`revision` context fields, detaches affected wire IDs before context callbacks,
then reconnects retained active listeners after callbacks. Disconnected ports
retain logical listeners without a host token until connected again.
Frontends should still clear displayed data on their input revision changes;
the host's new loading/reset snapshots also signal that old data is invalid.
Responses and cancellations are scoped to the document's random session and ID.
They also carry a host-controlled navigation generation, established by a
page-load handshake. Calls made during startup queue until that handshake;
queued cancellations and request timeouts remove their unsent calls.
