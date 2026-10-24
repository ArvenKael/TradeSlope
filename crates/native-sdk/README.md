# tradeslope-native-sdk

The small, **in-process Rust/GPUI** extension contract. No JavaScript, webview,
backend implementation, global client, or stable binary plugin ABI is involved.
Build extensions with the same GPUI version/features as the host. The host must
initialize `gpui_kit` before constructing views.
Native cards are compiled into the host with compatible SDK dependencies; they
are not independently loadable binary plugins.

## Implement a third-party factory

Add dependencies on `tradeslope-native-sdk`, `tradeslope-sdk`, and
`gpui-kit = { version = "0.8.0", features = ["gpui-fast"] }`.

```rust,ignore
use gpui_kit::{prelude::*, *};
use tradeslope_native_sdk::{CardContext, NativeCardFactory, NativeCardInstance};
use tradeslope_sdk::{CardManifest, InputPort, OperationKind, Renderer, Schema};

pub struct MyFactory;
struct MyView { context: CardContext }
struct MyInstance(Entity<MyView>);

impl Render for MyView {
    fn render(&mut self, _: &mut Window, _: &mut Context<Self>) -> impl IntoElement {
        div().child(format!("Native card {}", self.context.id))
    }
}

impl NativeCardInstance for MyInstance {
    fn view(&self) -> AnyView { self.0.clone().into() }

    fn set_context(&mut self, context: CardContext, _: &mut Window, cx: &mut App) {
        self.0.update(cx, |view, cx| {
            // A real data card must also clear cached data and invalidate
            // pending responses here, including when only the client changes.
            view.context = context;
            cx.notify();
        });
    }
}

impl NativeCardFactory for MyFactory {
    fn manifest(&self) -> CardManifest {
        CardManifest {
            id: "my-native-card".into(),
            title: "My native card".into(),
            renderer: Renderer::Native,
            inputs: vec![InputPort {
                id: "quote".into(),
                title: "Quote".into(),
                kind: OperationKind::Query,
                parameters: Schema::Object { fields: Default::default() },
                output: tradeslope_sdk::ticker_schema(),
                required: true,
            }],
        }
    }

    fn create(
        &self, context: CardContext, _: &mut Window, cx: &mut App,
    ) -> Box<dyn NativeCardInstance> {
        Box::new(MyInstance(cx.new(|_| MyView { context })))
    }
}
```

## Host registration and embedding

There is deliberately no global registry in this SDK. A host can own a registry
on its UI thread, for example:

```rust,ignore
let mut factories:
    std::collections::HashMap<String, Box<dyn NativeCardFactory>> =
    std::collections::HashMap::new();
let factory = MyFactory;
factories.insert(factory.manifest().id, Box::new(factory));

// Called inside host UI construction, with a host-supplied CardContext.
let instance = factories["my-native-card"].create(context, window, cx);
let view = instance.view(); // Embed as a child of the host's GPUI layout.
// Retain instance to forward later context changes through set_context().
```

This is an integration example, **not an existing root application registration
API**. Neither this crate nor the example market card edits or registers itself
in the root app. Factories and views need not be `Send`.

## Typed abstract inputs

Declare the schemas your card consumes, not provider IDs or API operations.
The user chooses a compatible backend API and maps its fields in Connections.
The host gives each instance a `CardContext { id, inputs }`; its `BoundInputs`
validates and remaps responses into the declared output schema. A native card
has no raw backend client. For example:

```rust,ignore
if !context.inputs.connected("quote") {
    // Ask the user to map the Quote input using Connections.
    return;
}
let mut subscription = context.inputs.subscribe("quote", serde_json::json!({}))?;
// Retain the subscription and drain subscription.try_recv() in a yielding
// UI task. Render InputSnapshot.value/error/status and notify on changes.
// Dropping it unsubscribes. The host owns query refresh and deduplication.
```

Clear displayed data when a snapshot has no value, including after an account
reset that coalesces directly into an error. Keep independent subscriptions for
multiple ports; each can map to a different backend.

For Actions and one-shot work use `submit` instead. Never automatically repeat
Actions. Yield while waiting for responses and reject results from old contexts.
The host remains responsible for permissions, backend configuration, and input
mappings. A disconnected input is not a request to use a default provider.

See `../market-card` for a working factory, entity, subscription consumption,
context replacement, and error handling.
