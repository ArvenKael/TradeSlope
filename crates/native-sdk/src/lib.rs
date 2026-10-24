//! Small, in-process extension interface for native GPUI cards.
//!
//! The host owns connection mappings and injects typed input access. Factories
//! and instances live on the UI thread; neither requires `Send`. This is a Rust
//! source-level API, not a stable dynamic-library ABI.

use gpui_kit::{AnyView, App, Window};
use tradeslope_sdk::BoundInputs;

/// Per-instance identity and access to the card's user-mapped input ports.
#[derive(Clone)]
pub struct CardContext {
    pub id: String,
    pub inputs: BoundInputs,
}

/// Creates native cards. Register factories with the host on its UI thread.
pub trait NativeCardFactory {
    fn manifest(&self) -> tradeslope_sdk::CardManifest;

    fn create(
        &self,
        context: CardContext,
        window: &mut Window,
        cx: &mut App,
    ) -> Box<dyn NativeCardInstance>;
}

/// An owned native view whose context can be replaced by the host.
pub trait NativeCardInstance {
    fn view(&self) -> AnyView;

    /// Invalidate old requests and data even if only the mapped client changed.
    fn set_context(&mut self, context: CardContext, window: &mut Window, cx: &mut App);
}
