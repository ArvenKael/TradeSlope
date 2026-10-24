//! The JS frontend adapter uses the same client and permissions as native cards.
use std::cell::{Cell, RefCell};
use std::collections::BTreeMap;
use std::rc::Rc;
use std::sync::mpsc::TryRecvError;
use std::time::{Duration, Instant};

use gpui_kit::*;
use gpui_webview::WebView;
use serde_json::Value;
use tradeslope_sdk::{
    BackendError, BackendResponse, BoundInputs, INPUT_PROTOCOL_VERSION, InputRequest,
};

#[derive(serde::Deserialize)]
struct DocumentRequest {
    session: String,
    document: u64,
    activity: u64,
    version: u32,
    #[serde(default)]
    id: u64,
    #[serde(default)]
    op: String,
    #[serde(default)]
    port: String,
    #[serde(default)]
    params: Value,
}

#[derive(Default)]
struct LiveState {
    session: Option<String>,
    closed: bool,
    suspended: bool,
    activity: u64,
    tokens: BTreeMap<u64, Rc<Cell<bool>>>,
    token_ports: BTreeMap<u64, String>,
}

impl LiveState {
    fn set_active(&mut self, active: bool) -> bool {
        let suspending = !active && !self.suspended;
        self.suspended = !active;
        if suspending {
            self.activity = self.activity.wrapping_add(1);
            self.cancel();
        }
        suspending
    }

    fn permits_data(&self, activity: u64) -> bool {
        !self.closed && !self.suspended && self.activity == activity
    }

    fn accept_session(&mut self, session: &str) -> bool {
        if self.closed {
            return false;
        }
        match &self.session {
            Some(current) => current == session,
            None => {
                self.session = Some(session.to_owned());
                true
            }
        }
    }

    fn navigate(&mut self) {
        self.cancel();
        self.session = None;
        self.closed = false;
    }

    fn cancel(&mut self) {
        for token in self.tokens.values() {
            token.set(true);
        }
        self.tokens.clear();
        self.token_ports.clear();
    }

    fn cancel_changed_ports(
        &mut self,
        old: &tradeslope_sdk::Bindings,
        new: &tradeslope_sdk::Bindings,
    ) {
        self.token_ports.retain(|id, port| {
            if old.get(port) == new.get(port) {
                return true;
            }
            if let Some(token) = self.tokens.remove(id) {
                token.set(true);
            }
            false
        });
    }
}

pub struct Bridge {
    pub view: Entity<WebView>,
    inputs: Rc<RefCell<BoundInputs>>,
    revision: Rc<Cell<u64>>,
    context: Rc<RefCell<Value>>,
    live: Rc<RefCell<LiveState>>,
}

impl Bridge {
    pub fn replace_inputs(&self, inputs: BoundInputs) {
        let mut current = self.inputs.borrow_mut();
        self.live
            .borrow_mut()
            .cancel_changed_ports(current.bindings(), inputs.bindings());
        *current = inputs;
        self.revision.set(self.revision.get().wrapping_add(1));
    }

    pub fn set_context(&self, context: Value, cx: &App) {
        let mut live = self.live.borrow_mut();
        if live.set_active(context["active"] != false) {
            // Also invalidate results from requests admitted before suspension.
            // Already executed Actions cannot be undone and are never retried.
            self.revision.set(self.revision.get().wrapping_add(1));
        }
        let activity = live.activity;
        drop(live);
        *self.context.borrow_mut() = context.clone();
        let _ = self
            .view
            .read(cx)
            .evaluate_script(&context_script(&context, activity));
    }
}

impl Drop for Bridge {
    fn drop(&mut self) {
        self.live.borrow_mut().cancel();
        self.revision.set(self.revision.get().wrapping_add(1));
    }
}

fn context_script(context: &Value, activity: u64) -> String {
    format!("window.__tradeslopeSetContext?.({context}, {activity});")
}

pub fn build(
    builder: wry::WebViewBuilder<'static>,
    origin: String,
    inputs: BoundInputs,
    context: Value,
    window: &mut Window,
    cx: &mut App,
) -> Result<Bridge, String> {
    let target: Rc<RefCell<Option<WeakEntity<WebView>>>> = Rc::new(RefCell::new(None));
    let slot = target.clone();
    let app = cx.to_async();
    let inputs = Rc::new(RefCell::new(inputs));
    let routed_inputs = inputs.clone();
    let revision = Rc::new(Cell::new(0u64));
    let current_revision = revision.clone();
    let context_state = Rc::new(RefCell::new(context.clone()));
    let load_context = context_state.clone();
    let load_target = target.clone();
    let load_app = app.clone();
    let outstanding = Rc::new(Cell::new(0usize));
    let live = Rc::new(RefCell::new(LiveState::default()));
    live.borrow_mut().set_active(context["active"] != false);
    let routed_live = live.clone();
    let load_live = live.clone();
    let load_revision = revision.clone();
    // Unlike a frontend nonce, this generation cannot be reclaimed by a queued
    // old-document IPC message after navigation starts.
    let document = Rc::new(Cell::new(1u64));
    let load_document = document.clone();
    let routed_document = document.clone();
    let initialization = format!(
        "window.__tradeslopeInitialContext = {context}; {}",
        include_str!("../../assets/sdk/backend.js")
    );
    let builder = builder
        .with_initialization_script(initialization)
        .with_on_page_load_handler(move |event, _| {
            if matches!(event, wry::PageLoadEvent::Started) {
                load_live.borrow_mut().navigate();
                load_document.set(load_document.get().wrapping_add(1));
                load_revision.set(load_revision.get().wrapping_add(1));
            }
            if matches!(event, wry::PageLoadEvent::Finished) {
                let target = load_target.borrow().clone();
                let context = load_context.clone();
                let generation = load_document.get();
                let document = load_document.clone();
                let live = load_live.clone();
                load_app
                    .spawn(async move |cx| {
                        if generation != document.get() {
                            return;
                        }
                        if let Some(target) = target {
                            let _ = target.update(cx, |view, _| {
                                let _ = view.evaluate_script(&format!(
                                    "{} window.__tradeslopeConnect?.({generation});",
                                    context_script(&context.borrow(), live.borrow().activity)
                                ));
                            });
                        }
                    })
                    .detach();
            }
        })
        .with_ipc_handler(move |message| {
            // Wry supplies the document URL. Never expose a client to navigation
            // outside the registered local card origin.
            let uri = message.uri().to_string();
            let windows_origin = origin.replacen("app://", "http://app.", 1);
            if !(uri.starts_with(&origin) || uri.starts_with(&windows_origin))
                || message.body().len() > 64 * 1024
            {
                return;
            }
            let Ok(message) =
                serde_json::from_str::<DocumentRequest>(message.body())
            else {
                return;
            };
            let DocumentRequest { session, document, activity, version, id, op, port, params } = message;
            if document != routed_document.get() || session.is_empty() || session.len() > 128 || version != INPUT_PROTOCOL_VERSION {
                return;
            }
            {
                let mut live = routed_live.borrow_mut();
                if !live.accept_session(&session) {
                    return;
                }
                if op == "close" {
                    live.cancel();
                    live.closed = true;
                    current_revision.set(current_revision.get().wrapping_add(1));
                    return;
                }
                if op == "unsubscribe" {
                    live.token_ports.remove(&id);
                    if let Some(token) = live.tokens.remove(&id) {
                        token.set(true);
                    }
                    return;
                }
            }
            let Some(target) = slot.borrow().clone() else {
                return;
            };
            if op == "subscribe" {
                let token = Rc::new(Cell::new(false));
                let queued = {
                    let mut live = routed_live.borrow_mut();
                    if !live.permits_data(activity) {
                        Err(BackendError::new("inactive", "Card is inactive or request predates suspension"))
                    } else if live.tokens.len() >= 32 || live.tokens.contains_key(&id) {
                        Err(BackendError::new("busy", "Too many live subscriptions or duplicate ID"))
                    } else {
                        let result = routed_inputs.borrow().subscribe(&port, params);
                        if result.is_ok() {
                            live.tokens.insert(id, token.clone());
                            live.token_ports.insert(id, port.clone());
                        }
                        result
                    }
                };
                let live = routed_live.clone();
                app.spawn(async move |cx| {
                    let subscription = match queued {
                        Ok(subscription) => Some(subscription),
                        Err(error) => {
                            let envelope = serde_json::json!({
                                "version": INPUT_PROTOCOL_VERSION, "session": session,
                                "id": id, "op": "subscription", "error": error, "terminal": true,
                            });
                            let _ = target.update(cx, |view, _| {
                                let _ = view.evaluate_script(&format!(
                                    "window.__tradeslopeBackendResponse?.({envelope});"
                                ));
                            });
                            None
                        }
                    };
                    if let Some(mut subscription) = subscription {
                        loop {
                            if token.get() || target.upgrade().is_none() {
                                break;
                            }
                            match subscription.try_recv() {
                                Ok(snapshot) => {
                                    // A loading snapshot with no value is a reset barrier:
                                    // the frontend must clear data from the previous account.
                                    let envelope = serde_json::json!({
                                        "version": INPUT_PROTOCOL_VERSION, "session": session,
                                        "id": id, "op": "subscription", "status": snapshot.status,
                                        "hasValue": snapshot.value.is_some(),
                                        "result": snapshot.value, "error": snapshot.error,
                                    });
                                    let _ = target.update(cx, |view, _| {
                                        let _ = view.evaluate_script(&format!(
                                            "window.__tradeslopeBackendResponse?.({envelope});"
                                        ));
                                    });
                                }
                                Err(TryRecvError::Disconnected) => {
                                    let envelope = serde_json::json!({
                                        "version": INPUT_PROTOCOL_VERSION, "session": session,
                                        "id": id, "op": "subscription", "terminal": true,
                                        "error": BackendError::new("disconnected", "Backend stopped"),
                                    });
                                    let _ = target.update(cx, |view, _| {
                                        let _ = view.evaluate_script(&format!(
                                            "window.__tradeslopeBackendResponse?.({envelope});"
                                        ));
                                    });
                                    break;
                                }
                                Err(TryRecvError::Empty) => {}
                            }
                            // Yield even after a value; never retain the view across await.
                            cx.background_executor().timer(Duration::from_millis(50)).await;
                        }
                    }
                    let mut live = live.borrow_mut();
                    if live.tokens.get(&id).is_some_and(|current| Rc::ptr_eq(current, &token)) {
                        live.tokens.remove(&id);
                        live.token_ports.remove(&id);
                    }
                }).detach();
                return;
            }
            if !op.is_empty() && op != "request" {
                return;
            }
            let request = InputRequest { version, id, port, params };
            let queued = if !routed_live.borrow().permits_data(activity) {
                Err(BackendError::new("inactive", "Card is inactive or request predates suspension"))
            } else if outstanding.get() >= 32 {
                Err(BackendError::new(
                    "busy",
                    "Too many pending requests for this card",
                ))
            } else {
                routed_inputs.borrow().submit_request(request)
            };
            let request_revision = current_revision.get();
            let revision = current_revision.clone();
            outstanding.set(outstanding.get() + 1);
            let count = outstanding.clone();
            app.spawn(async move |cx| {
                let result = match queued {
                    Err(error) => Err(error),
                    Ok(pending) => {
                        let deadline = Instant::now() + Duration::from_secs(30);
                        loop {
                            if revision.get() != request_revision {
                                break Err(BackendError::new(
                                    "binding_changed",
                                    "Input mapping changed",
                                ));
                            }
                            if target.upgrade().is_none() {
                                count.set(count.get().saturating_sub(1));
                                return;
                            }
                            match pending.try_recv() {
                                Ok(result) => break result,
                                Err(TryRecvError::Disconnected) => {
                                    break Err(BackendError::new(
                                        "disconnected",
                                        "Backend stopped",
                                    ));
                                }
                                Err(TryRecvError::Empty) if Instant::now() >= deadline => {
                                    break Err(BackendError::new(
                                        "timeout",
                                        "Backend request timed out",
                                    ));
                                }
                                Err(TryRecvError::Empty) => {
                                    cx.background_executor()
                                        .timer(Duration::from_millis(50))
                                        .await
                                }
                            }
                        }
                    }
                };
                count.set(count.get().saturating_sub(1));
                let (result, error) = match result {
                    Ok(value) => (Some(value), None),
                    Err(error) => (None, Some(error)),
                };
                let response = BackendResponse {
                    version: INPUT_PROTOCOL_VERSION,
                    id,
                    result,
                    error,
                };
                if let Ok(mut envelope) = serde_json::to_value(&response) {
                    envelope["session"] = Value::String(session);
                    let json = envelope.to_string();
                    let _ = target.update(cx, |view, _| {
                        let _ = view.evaluate_script(&format!(
                            "window.__tradeslopeBackendResponse?.({json});"
                        ));
                    });
                }
            })
            .detach();
        });
    let view = WebView::build(builder, window, cx).map_err(|error| error.to_string())?;
    let entity = cx.new(|_| view);
    *target.borrow_mut() = Some(entity.downgrade());
    Ok(Bridge {
        view: entity,
        inputs,
        revision,
        context: context_state,
        live,
    })
}

#[cfg(test)]
mod tests {
    use super::LiveState;
    use std::{cell::Cell, rc::Rc};

    #[test]
    fn document_sessions_do_not_replace_each_other() {
        let mut live = LiveState::default();
        assert!(live.accept_session("first"));
        assert!(live.accept_session("first"));
        assert!(!live.accept_session("other"));
        let token = Rc::new(Cell::new(false));
        live.tokens.insert(1, token.clone());
        live.navigate();
        assert!(token.get());
        assert!(live.tokens.is_empty());
        assert!(live.accept_session("second"));
        assert!(!live.accept_session("first"));
    }

    #[test]
    fn cancellation_releases_all_slots_without_changing_the_document() {
        let mut live = LiveState::default();
        assert!(live.accept_session("document"));
        let tokens: Vec<_> = (0..32).map(|_| Rc::new(Cell::new(false))).collect();
        for (id, token) in tokens.iter().enumerate() {
            live.tokens.insert(id as u64, token.clone());
        }
        live.cancel();
        live.cancel();
        assert!(tokens.iter().all(|token| token.get()));
        assert!(live.tokens.is_empty());
        assert!(!live.accept_session("other"));
        live.closed = true;
        assert!(!live.accept_session("document"));
        live.navigate();
        assert!(live.accept_session("new-document"));
    }

    #[test]
    fn suspension_blocks_data_and_pre_suspend_queued_operations() {
        let mut live = LiveState::default();
        assert!(live.accept_session("document"));
        assert!(live.permits_data(0));
        let token = Rc::new(Cell::new(false));
        live.tokens.insert(1, token.clone());
        assert!(live.set_active(false));
        assert!(token.get());
        assert!(live.tokens.is_empty());
        assert!(!live.permits_data(0));
        assert!(!live.permits_data(1));
        assert!(!live.set_active(false));
        assert_eq!(live.activity, 1);
        assert!(!live.set_active(true));
        assert!(
            !live.permits_data(0),
            "old queued requests must not execute on resume"
        );
        assert!(live.permits_data(1));
        assert!(!live.accept_session("other"));
        live.set_active(false);
        live.navigate();
        assert!(
            !live.permits_data(2),
            "navigation does not reactivate a hidden card"
        );
        assert!(live.accept_session("new-document"));
        live.set_active(true);
        assert!(live.permits_data(2));
        live.closed = true;
        assert!(!live.permits_data(2));
    }

    #[test]
    fn remapping_cancels_only_tokens_for_changed_ports() {
        let mut live = LiveState::default();
        let binding = |operation: &str| tradeslope_sdk::PortBinding {
            backend: "backend".into(),
            operation: operation.into(),
            params: serde_json::json!({}),
            fields: Default::default(),
            parameter_fields: Default::default(),
        };
        let old = tradeslope_sdk::Bindings::from([
            ("primary".into(), binding("first")),
            ("comparison".into(), binding("second")),
        ]);
        let mut new = old.clone();
        new.insert("comparison".into(), binding("replacement"));
        let primary = Rc::new(Cell::new(false));
        let comparison = Rc::new(Cell::new(false));
        live.tokens.insert(1, primary.clone());
        live.tokens.insert(2, comparison.clone());
        live.token_ports.insert(1, "primary".into());
        live.token_ports.insert(2, "comparison".into());
        live.cancel_changed_ports(&old, &new);
        assert!(!primary.get());
        assert!(comparison.get());
        assert_eq!(live.tokens.len(), 1);
        assert_eq!(live.token_ports.len(), 1);
        live.cancel_changed_ports(&new, &new);
        assert!(!primary.get());
        live.cancel_changed_ports(&new, &Default::default());
        assert!(primary.get(), "disconnect cancels the remaining port");
        assert!(live.tokens.is_empty());
        assert!(live.token_ports.is_empty());
    }
}
