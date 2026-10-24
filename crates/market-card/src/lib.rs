//! Native quote card with independently mapped primary and comparison inputs.

use std::{
    collections::BTreeMap,
    time::{Duration, SystemTime, UNIX_EPOCH},
};

use gpui_kit::{
    AnyView, App, Context, Entity, IntoElement, Render, Task, Window,
    component::{ActiveTheme, button::Button},
    div,
    prelude::*,
};
use serde_json::json;
use tradeslope_native_sdk::{CardContext, NativeCardFactory, NativeCardInstance};
use tradeslope_sdk::{
    BoundInputs, CardManifest, InputPort, InputSnapshot, InputSubscription, OperationKind,
    Renderer, Schema, SubscriptionStatus, Ticker,
};

/// Register this factory with the host's native factory registry.
pub struct MarketCardFactory;

fn quote_port(id: &str, title: &str, required: bool) -> InputPort {
    InputPort {
        id: id.into(),
        title: title.into(),
        kind: OperationKind::Query,
        parameters: Schema::Object {
            fields: BTreeMap::new(),
        },
        output: tradeslope_sdk::ticker_schema(),
        required,
    }
}

impl NativeCardFactory for MarketCardFactory {
    fn manifest(&self) -> CardManifest {
        CardManifest {
            id: "market-overview".into(),
            title: "Market overview".into(),
            renderer: Renderer::Native,
            inputs: vec![
                quote_port("quote", "Quote", true),
                quote_port("comparison", "Quote comparison", false),
            ],
        }
    }

    fn create(
        &self,
        context: CardContext,
        _window: &mut Window,
        cx: &mut App,
    ) -> Box<dyn NativeCardInstance> {
        let entity = cx.new(|cx: &mut Context<MarketCard>| {
            let mut card = MarketCard {
                primary: QuoteInput::new("quote", &context.inputs),
                comparison: QuoteInput::new("comparison", &context.inputs),
                context,
                updates_task: None,
            };
            card.connect();
            // This task only consumes subscription updates. Fetch scheduling,
            // single-flight and cancellation belong to the shared SDK hub.
            // Dropping the entity drops both this task and its subscription.
            card.updates_task = Some(cx.spawn(async move |entity, cx| {
                loop {
                    if entity.update(cx, |card, cx| card.consume(cx)).is_err() {
                        break;
                    }
                    cx.background_executor()
                        .timer(Duration::from_millis(100))
                        .await;
                }
            }));
            card
        });
        Box::new(MarketCardInstance { entity })
    }
}

struct MarketCardInstance {
    entity: Entity<MarketCard>,
}

impl NativeCardInstance for MarketCardInstance {
    fn view(&self) -> AnyView {
        self.entity.clone().into()
    }

    fn set_context(&mut self, context: CardContext, _window: &mut Window, cx: &mut App) {
        self.entity.update(cx, |card, cx| {
            // Always invalidate: identical mappings may use a replacement client.
            card.primary = QuoteInput::new("quote", &context.inputs);
            card.comparison = QuoteInput::new("comparison", &context.inputs);
            card.context = context;
            card.connect();
            cx.notify();
        });
    }
}

struct Snapshot {
    ticker: Ticker,
    wall_time: SystemTime,
}

/// Pure connection/response state, independent of GPUI and backend implementations.
struct State {
    connected: bool,
    snapshot: Option<Snapshot>,
    error: Option<String>,
    status: SubscriptionStatus,
}

impl State {
    fn new(connected: bool) -> Self {
        Self {
            connected,
            snapshot: None,
            error: None,
            status: SubscriptionStatus::Loading,
        }
    }

    fn apply(&mut self, update: InputSnapshot) {
        self.status = update.status;
        self.error = update
            .error
            .map(|error| error.to_string().chars().take(320).collect());
        if update.value.is_none() {
            // Lifecycle reset must not display a quote from the old account.
            self.snapshot = None;
        }
        if let Some(value) = update.value {
            match serde_json::from_value::<Ticker>(value) {
                Ok(ticker) => {
                    if self.snapshot.as_ref().map(|snapshot| &snapshot.ticker) != Some(&ticker) {
                        self.snapshot = Some(Snapshot {
                            ticker,
                            wall_time: SystemTime::now(),
                        });
                    }
                }
                Err(error) => {
                    self.status = SubscriptionStatus::Error;
                    self.error = Some(format!("Invalid normalized ticker response: {error}"));
                }
            }
        }
    }
}

struct QuoteInput {
    port: &'static str,
    state: State,
    subscription: Option<InputSubscription>,
}

impl QuoteInput {
    fn new(port: &'static str, inputs: &BoundInputs) -> Self {
        Self {
            port,
            state: State::new(inputs.connected(port)),
            subscription: None,
        }
    }

    fn connect(&mut self, inputs: &BoundInputs) {
        if self.state.connected {
            match inputs.subscribe(self.port, json!({})) {
                Ok(subscription) => self.subscription = Some(subscription),
                Err(error) => {
                    self.state.status = SubscriptionStatus::Error;
                    self.state.error = Some(error.to_string());
                }
            }
        }
    }

    fn consume(&mut self) -> bool {
        if let Some(subscription) = &mut self.subscription
            && let Ok(update) = subscription.try_recv()
        {
            self.state.apply(update);
            return true;
        }
        false
    }

    fn refresh(&mut self, inputs: &BoundInputs) {
        if let Some(subscription) = &self.subscription {
            subscription.refresh();
        } else {
            self.connect(inputs);
        }
    }
}

struct MarketCard {
    context: CardContext,
    primary: QuoteInput,
    comparison: QuoteInput,
    updates_task: Option<Task<()>>,
}

impl MarketCard {
    fn connect(&mut self) {
        self.primary.connect(&self.context.inputs);
        self.comparison.connect(&self.context.inputs);
    }

    fn consume(&mut self, cx: &mut Context<Self>) {
        // Consume both cursors even when the primary changed.
        let primary_changed = self.primary.consume();
        let comparison_changed = self.comparison.consume();
        if primary_changed || comparison_changed {
            cx.notify();
        }
    }
}

fn number(value: Option<f64>) -> String {
    value
        .filter(|value| value.is_finite())
        .map(|value| value.to_string())
        .unwrap_or_else(|| "—".into())
}

fn quote_view(state: &State, title: &str) -> impl IntoElement {
    let mut body = div().flex().flex_col().gap_2().child(title.to_owned());

    if !state.connected {
        return body.child(format!(
            "Choose a data source for {title} from this card's source bar"
        ));
    }

    let status = match state.status {
        SubscriptionStatus::Loading => "Connecting…",
        SubscriptionStatus::Error => "Connection error · shared subscription will retry",
        SubscriptionStatus::Live => "Connected · shared live query",
    };
    body = body.child(status);
    if let Some(error) = &state.error {
        body = body.child(format!("Error: {error}"));
    }
    if let Some(snapshot) = &state.snapshot {
        let seconds = snapshot
            .wall_time
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_secs();
        body = body
            .child(format!("Symbol: {}", snapshot.ticker.symbol))
            .child(format!("Price: {}", number(Some(snapshot.ticker.last))))
            .child(format!("Bid: {}", number(snapshot.ticker.bid)))
            .child(format!("Ask: {}", number(snapshot.ticker.ask)))
            .child(format!("Volume: {}", number(snapshot.ticker.volume)))
            .child(format!(
                "Last changed: {:02}:{:02}:{:02} UTC{}",
                seconds / 3600 % 24,
                seconds / 60 % 60,
                seconds % 60,
                if state.error.is_some() {
                    " · STALE"
                } else {
                    ""
                },
            ));
    } else {
        body = body.child("No ticker snapshot received.");
    }
    body
}

impl Render for MarketCard {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let mut body = div()
            .id("market-card-body")
            .flex()
            .flex_col()
            .gap_4()
            .p_4()
            .size_full()
            .min_h_0()
            .min_w_0()
            .overflow_y_scroll()
            .bg(cx.theme().background)
            .text_color(cx.theme().foreground)
            .child(div().text_lg().child("Market overview"))
            .child("REST fallback polls every 5 seconds by default; provider notices can refresh early.")
            .child(quote_view(&self.primary.state, "Quote"));
        if self.comparison.state.connected {
            body = body.child(quote_view(&self.comparison.state, "Quote comparison"));
        } else {
            body = body.child("Use + Add source to compare another instrument or connection.");
        }
        body.child(
            Button::new("refresh-market")
                .label(
                    if self.primary.state.error.is_some() || self.comparison.state.error.is_some() {
                        "Retry"
                    } else {
                        "Refresh"
                    },
                )
                .on_click(cx.listener(|card, _, _, cx| {
                    card.primary.refresh(&card.context.inputs);
                    card.comparison.refresh(&card.context.inputs);
                    card.consume(cx);
                })),
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ticker() -> Ticker {
        serde_json::from_value(json!({
            "symbol": "BTC/USDT", "last": 123.45,
            "bid": null, "ask": null, "volume": null,
        }))
        .unwrap()
    }

    #[test]
    fn changed_snapshots_and_errors_are_consumed_without_fetch_state() {
        let mut state = State::new(true);
        let value = serde_json::to_value(ticker()).unwrap();
        state.apply(InputSnapshot {
            value: Some(value.clone()),
            error: None,
            status: SubscriptionStatus::Live,
        });
        let changed = state.snapshot.as_ref().unwrap().wall_time;
        state.apply(InputSnapshot {
            value: Some(value.clone()),
            error: Some(tradeslope_sdk::BackendError::new(
                "disabled",
                "Backend disabled",
            )),
            status: SubscriptionStatus::Error,
        });
        assert!(state.error.as_ref().unwrap().contains("disabled"));
        assert_eq!(state.snapshot.as_ref().unwrap().wall_time, changed);
        state.apply(InputSnapshot {
            value: Some(value),
            error: None,
            status: SubscriptionStatus::Live,
        });
        assert!(state.error.is_none());
        // Unchanged successful values must not masquerade as a data change.
        assert_eq!(state.snapshot.as_ref().unwrap().wall_time, changed);
        state.apply(InputSnapshot {
            value: None,
            error: None,
            status: SubscriptionStatus::Loading,
        });
        assert!(state.snapshot.is_none());
        assert_eq!(state.status, SubscriptionStatus::Loading);
        // Context replacement creates a clean state; old subscription is dropped.
        state = State::new(false);
        assert!(!state.connected);
        assert!(state.snapshot.is_none());
    }

    #[test]
    fn invalid_ticker_is_visible_and_missing_numbers_are_not_zero() {
        let mut state = State::new(true);
        state.apply(InputSnapshot {
            value: Some(json!({"last":"bad"})),
            error: None,
            status: SubscriptionStatus::Live,
        });
        assert_eq!(state.status, SubscriptionStatus::Error);
        assert!(state.error.unwrap().contains("Invalid normalized ticker"));
        assert_eq!(number(None), "—");
        assert_eq!(number(Some(f64::NAN)), "—");
        assert_eq!(number(Some(0.0)), "0");
        assert_eq!(number(Some(0.00000001)), "0.00000001");
    }

    #[test]
    fn comparison_error_and_reset_leave_primary_quote_unchanged() {
        let mut primary = State::new(true);
        let mut comparison = State::new(true);
        let primary_ticker = ticker();
        let mut secondary_ticker = ticker();
        secondary_ticker.symbol = "ETH/USDT".into();
        secondary_ticker.last = 456.0;
        primary.apply(InputSnapshot {
            value: Some(serde_json::to_value(&primary_ticker).unwrap()),
            error: None,
            status: SubscriptionStatus::Live,
        });
        comparison.apply(InputSnapshot {
            value: Some(serde_json::to_value(&secondary_ticker).unwrap()),
            error: None,
            status: SubscriptionStatus::Live,
        });
        comparison.apply(InputSnapshot {
            value: Some(serde_json::to_value(&secondary_ticker).unwrap()),
            error: Some(tradeslope_sdk::BackendError::new(
                "offline",
                "Comparison unavailable",
            )),
            status: SubscriptionStatus::Error,
        });
        assert_eq!(
            comparison.snapshot.as_ref().unwrap().ticker,
            secondary_ticker
        );
        assert!(comparison.error.is_some());
        assert_eq!(primary.snapshot.as_ref().unwrap().ticker, primary_ticker);
        assert!(primary.error.is_none());
        comparison.apply(InputSnapshot {
            value: None,
            error: None,
            status: SubscriptionStatus::Loading,
        });
        assert!(comparison.snapshot.is_none());
        assert_eq!(primary.snapshot.as_ref().unwrap().ticker, primary_ticker);
        assert_eq!(primary.status, SubscriptionStatus::Live);
        // A comparison-only remap/reconnection cannot overwrite primary state.
        comparison = State::new(true);
        assert!(comparison.snapshot.is_none());
        assert_eq!(primary.snapshot.as_ref().unwrap().ticker, primary_ticker);
    }

    #[test]
    fn manifest_identifies_a_native_market_card() {
        let manifest = MarketCardFactory.manifest();
        assert_eq!(manifest.id, "market-overview");
        assert_eq!(manifest.title, "Market overview");
        assert!(matches!(manifest.renderer, Renderer::Native));
        assert_eq!(
            manifest.inputs,
            vec![
                InputPort {
                    id: "quote".into(),
                    title: "Quote".into(),
                    kind: OperationKind::Query,
                    parameters: Schema::Object {
                        fields: BTreeMap::new()
                    },
                    output: tradeslope_sdk::ticker_schema(),
                    required: true,
                },
                InputPort {
                    id: "comparison".into(),
                    title: "Quote comparison".into(),
                    kind: OperationKind::Query,
                    parameters: Schema::Object {
                        fields: BTreeMap::new()
                    },
                    output: tradeslope_sdk::ticker_schema(),
                    required: false,
                }
            ]
        );
    }
}
