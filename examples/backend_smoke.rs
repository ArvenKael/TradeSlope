//! Actual GPUI + WKWebView mapping check, using two deterministic typed providers.
//! cargo run --example backend_smoke
#[path = "../src/ui/backend_bridge.rs"]
mod backend_bridge;
#[path = "support/openalgo.rs"]
mod openalgo;

use gpui_kit::*;
use serde_json::{Value, json};
use std::{
    collections::BTreeMap,
    sync::{
        Arc,
        atomic::{AtomicUsize, Ordering},
        mpsc,
    },
    time::Duration,
};
use tradeslope_native_sdk::{CardContext, NativeCardFactory, NativeCardInstance};
use tradeslope_sdk::{
    Backend, BackendHost, BackendManifest, BackendResult, Bindings, BoundInputs, Field, InputPort,
    Operation, OperationKind, PortBinding, Schema,
};

struct Fixture {
    id: &'static str,
    price: f64,
    change: Arc<AtomicUsize>,
    delay_ms: Arc<AtomicUsize>,
    quotes: Arc<AtomicUsize>,
}
fn record(fields: &[(&str, Schema)]) -> Schema {
    Schema::Object {
        fields: fields
            .iter()
            .map(|(key, schema)| {
                (
                    (*key).into(),
                    Field {
                        schema: schema.clone(),
                        required: true,
                        description: String::new(),
                        default: None,
                    },
                )
            })
            .collect(),
    }
}
fn renamed_bars_schema() -> Schema {
    Schema::Array {
        items: Box::new(record(&[
            ("stamp", Schema::Integer),
            ("start", Schema::Number),
            ("maximum", Schema::Number),
            ("minimum", Schema::Number),
            ("finish", Schema::Number),
            ("amount", Schema::Number),
        ])),
    }
}
fn interval_schema() -> Schema {
    Schema::Enum {
        values: ["1m", "5m", "15m", "30m", "1h", "4h", "1d"]
            .into_iter()
            .map(str::to_owned)
            .collect(),
    }
}
fn bars_parameters(backend: bool) -> Schema {
    let Schema::Object { mut fields } = record(&[("interval", interval_schema())]) else {
        unreachable!()
    };
    if backend {
        let Schema::Object { fields: instrument } = record(&[("instrument", Schema::String)])
        else {
            unreachable!()
        };
        fields.extend(instrument);
    }
    for (name, description) in [
        (
            "end",
            "Inclusive upper candle timestamp in UTC Unix seconds",
        ),
        (
            "limit",
            "Optional page size, 1..1000; fixture default is 100",
        ),
    ] {
        fields.insert(
            name.into(),
            Field {
                schema: Schema::Integer,
                required: false,
                description: description.into(),
                default: None,
            },
        );
    }
    Schema::Object { fields }
}
impl Backend for Fixture {
    fn manifest(&self) -> BackendManifest {
        let parameters = record(&[("instrument", Schema::String)]);
        BackendManifest {
            id: self.id.into(),
            title: self.id.into(),
            version: "1".into(),
            operations: vec![
                Operation::query(
                    "records",
                    "Time records",
                    bars_parameters(true),
                    renamed_bars_schema(),
                ),
                Operation::query(
                    "latest",
                    "Snapshot",
                    parameters,
                    tradeslope_sdk::ticker_schema(),
                ),
            ],
        }
    }
    fn execute(&self, operation: &str, params: Value) -> BackendResult<Value> {
        std::thread::sleep(Duration::from_millis(
            self.delay_ms.load(Ordering::SeqCst) as u64
        ));
        let price = self.price + self.change.load(Ordering::SeqCst) as f64;
        Ok(match operation {
            "records" => {
                let seconds = match params["interval"].as_str().unwrap() {
                    "1m" => 60,
                    "5m" => 300,
                    "15m" => 900,
                    "30m" => 1800,
                    "1h" => 3600,
                    "4h" => 14400,
                    "1d" => 86400,
                    _ => panic!("input interval must be validated before execution"),
                };
                let limit = params.get("limit").map_or(Some(100), Value::as_i64);
                let end = params
                    .get("end")
                    .map_or(Some(1700000000i64 + 99 * seconds), Value::as_i64);
                let (Some(limit @ 1..=1000), Some(end @ 0..=253402300799)) = (limit, end) else {
                    return Err(tradeslope_sdk::BackendError::new(
                        "invalid_params",
                        "end must be UTC seconds and limit must be 1..1000",
                    ));
                };
                // Fixed interval grid, anchored to the existing latest fixture.
                // An explicit end selects the last slot at/before that bound;
                // subtracting one from the oldest candle yields a disjoint page.
                let last = 1700000000i64 + (end - 1700000000i64).div_euclid(seconds) * seconds;
                json!(
                    (0..limit)
                        .map(|i| json!({
                            "stamp":last - (limit - 1 - i) * seconds, "start":price,
                            "maximum":price + 2.0, "minimum":price - 2.0,
                            "finish":price + 1.0, "amount":200.0,
                        }))
                        .collect::<Vec<_>>()
                )
            }
            "latest" => {
                self.quotes.fetch_add(1, Ordering::SeqCst);
                json!({"symbol":params["instrument"],"last":price,"bid":price - 1.0,"ask":price + 1.0,"volume":200.0})
            }
            _ => panic!("unknown operation should never execute"),
        })
    }
}
fn bars_port() -> InputPort {
    InputPort {
        id: "bars".into(),
        title: "Bars".into(),
        kind: OperationKind::Query,
        parameters: bars_parameters(false),
        output: tradeslope_sdk::candle_schema(),
        required: true,
    }
}
fn chart_ports() -> Vec<InputPort> {
    let mut comparison = bars_port();
    comparison.id = "comparison".into();
    comparison.title = "Comparison".into();
    comparison.required = false;
    vec![bars_port(), comparison]
}
fn bars_binding(backend: &str) -> PortBinding {
    PortBinding {
        backend: backend.into(),
        operation: "records".into(),
        params: json!({"instrument":"USER-CHOICE"}),
        fields: [
            ("time", "stamp"),
            ("open", "start"),
            ("high", "maximum"),
            ("low", "minimum"),
            ("close", "finish"),
            ("volume", "amount"),
        ]
        .into_iter()
        .map(|(to, from)| (to.into(), from.into()))
        .collect(),
        parameter_fields: ["interval", "end", "limit"]
            .into_iter()
            .map(|name| (name.into(), name.into()))
            .collect(),
    }
}
fn context(connected: bool, revision: u64) -> Value {
    json!({"active":true,"theme":"dark","inputs":{
        "bars":{"connected":connected,"revision":revision,"label":"Fixture · USER-CHOICE"},
        "comparison":{"connected":connected && revision == 0,"revision":revision,"label":"Independent comparison"},
    }})
}
struct Smoke {
    web: backend_bridge::Bridge,
    native: Box<dyn NativeCardInstance>,
    _host: BackendHost,
}
impl Render for Smoke {
    fn render(&mut self, _: &mut Window, _: &mut Context<Self>) -> impl IntoElement {
        div()
            .flex()
            .size_full()
            .child(div().flex_1().min_w_0().h_full().child(self.native.view()))
            .child(
                div()
                    .flex_1()
                    .min_w_0()
                    .h_full()
                    .child(self.web.view.clone()),
            )
    }
}
const CHECK: &str = r#"
window.addEventListener('DOMContentLoaded', async () => {
    try {
        await TradeSlopeChart.ready;
        if (TradeSlope.backend !== undefined) throw new Error('raw provider API leaked to block');
        const bars = await TradeSlope.inputs.request('bars', {interval:'1h'});
        if (bars.length !== 100 || bars[0].close !== 203 || bars[0].time !== 1700000000 ||
            bars[1].time - bars[0].time !== 3600)
            throw new Error('user output-field mapping did not apply');
        const fiveMinuteBars = await TradeSlope.inputs.request('bars', {interval:'5m'});
        if (fiveMinuteBars.length !== 100 || fiveMinuteBars[0].close !== 203 ||
            fiveMinuteBars[1].time - fiveMinuteBars[0].time !== 300)
            throw new Error('dynamic interval mapping did not apply without recreating the binding');
        const history = await TradeSlope.inputs.request('bars', {
            interval:'1h', end:bars[0].time - 1, limit:200,
        });
        const previous = await TradeSlope.inputs.request('bars', {
            interval:'1h', end:history[0].time - 1, limit:60,
        });
        for (const [page, newer, expected] of [[history,bars,200],[previous,history,60]]) {
            if (page.length !== expected || page.at(-1).time !== newer[0].time - 3600 ||
                page.some((bar,i)=>bar.close !== 203 || !Number.isSafeInteger(bar.time) ||
                    (i > 0 && bar.time - page[i-1].time !== 3600)))
                throw new Error('typed historical end/limit mapping did not return older sorted nonoverlapping pages');
        }
        for (const params of [{}, {interval:'1w'}, {interval:'1h',instrument:'NOT-USER-CHOICE'},
            {interval:'1h',end:'invalid'}, {interval:'1h',limit:1.5}, {interval:'1h',limit:1001}]) {
            let invalid = false;
            try { await TradeSlope.inputs.request('bars', params); }
            catch (error) { invalid = error.code === 'invalid_params'; }
            if (!invalid) throw new Error('frontend bypassed the interval/source parameter contract');
        }
        let unbound = false;
        try { await TradeSlope.inputs.request('not-declared', {}); }
        catch (error) { unbound = error.code === 'unknown_input'; }
        if (!unbound) throw new Error('undeclared port was callable');
        for (let i=0; i<100 && !document.getElementById('fallback').hidden; i++)
            await new Promise(resolve=>setTimeout(resolve,50));
        if (!document.getElementById('fallback').hidden) throw new Error('mapped chart failed to load');
        if (document.querySelector('.badge').textContent !== 'MAPPED') throw new Error('not marked MAPPED');
        const comparison = await TradeSlope.inputs.request('comparison', {interval:'1h'});
        if (comparison[0].close !== 102) throw new Error('comparison did not use the independent provider');
        const comparisonHistory = await TradeSlope.inputs.request('comparison', {
            interval:'1h', end:comparison[0].time - 1, limit:200,
        });
        if (comparisonHistory.length !== 200 ||
            comparisonHistory.at(-1).time !== comparison[0].time - 3600 ||
            comparisonHistory.some((bar,i)=>bar.close !== 102 ||
                (i > 0 && bar.time - comparisonHistory[i-1].time !== 3600)))
            throw new Error('comparison historical request did not cross its independent typed binding');
        for (let i=0; i<100 && !TradeSlopeChart.getState().chart.series.some(series=>series.style?.title==='Comparison'); i++)
            await new Promise(resolve=>setTimeout(resolve,50));
        if (!TradeSlopeChart.getState().chart.series.some(series=>series.style?.title==='Comparison'))
            throw new Error('comparison line overlay was not created');
        for (const interval of ['1m','5m','15m','30m','1h','4h','1d']) {
            const control = document.querySelector(`[data-interval="${interval}"]`);
            if (!control || control.disabled) throw new Error(`missing enabled interval control: ${interval}`);
        }
        for (const interval of ['5m','1h']) {
            document.querySelector(`[data-interval="${interval}"]`).click();
            if (TradeSlopeChart.getState().interval !== interval)
                throw new Error('upstream interval event did not select the requested timeframe');
            for (let i=0; i<100 && !document.getElementById('fallback').hidden; i++)
                await new Promise(resolve=>setTimeout(resolve,50));
            if (!document.getElementById('fallback').hidden)
                throw new Error('widget interval change did not resubscribe successfully');
        }
        await new Promise((resolve, reject) => {
            window.__stopSmokeLive = TradeSlope.inputs.subscribe('bars', {interval:'1h'}, (value, error, metadata) => {
                if (metadata.status === 'loading') {
                    if (window.__sdkSmokeResetExpected) setTimeout(() => {
                        window.__sdkSmokeResult={stage:'reset',ok:
                            document.getElementById('fallback').textContent === 'Loading price data…' &&
                            !document.getElementById('fallback').hidden};
                    }, 100);
                    return;
                }
                if (error) { reject(error); window.__sdkSmokeResult={ok:false,error:String(error)}; }
                else if (value[0].close === 203) resolve();
                else if (value[0].close === 204) {
                    window.__sdkSmokeResult={stage:'pushed',ok:true};
                } else if (value[0].close === 205) {
                    window.__sdkSmokeResult={stage:'reset-live',ok:true};
                    window.__stopSmokeLive();
                } else window.__sdkSmokeResult={ok:false,error:'Unexpected live bars'};
            });
        });
        window.__sdkSmokeResult={stage:'mapped',ok:true,history:[history.length,previous.length],
            comparisonHistory:comparisonHistory.length};
    } catch (error) { window.__sdkSmokeResult={ok:false,error:String(error)}; }
}, {once:true});
"#;
fn main() {
    application().run(|cx| {
        gpui_kit::init(cx);
        let quotes = Arc::new(AtomicUsize::new(0));
        let change = Arc::new(AtomicUsize::new(0));
        let delay_ms = Arc::new(AtomicUsize::new(0));
        let host = BackendHost::new(vec![
            Arc::new(Fixture{id:"source-a",price:101.0,change:Arc::new(AtomicUsize::new(0)),delay_ms:Arc::new(AtomicUsize::new(0)),quotes:quotes.clone()}),
            Arc::new(Fixture{id:"source-b",price:202.0,change:change.clone(),delay_ms:delay_ms.clone(),quotes:Arc::new(AtomicUsize::new(0))}),
        ]).unwrap();
        let client = host.client(Default::default());
        let factory = tradeslope_market_card::MarketCardFactory;
        let native_inputs = BoundInputs::new(client.clone(), factory.manifest().inputs, BTreeMap::from([
            ("quote".into(), PortBinding{backend:"source-a".into(),operation:"latest".into(),
                params:json!({"instrument":"NATIVE-USER-CHOICE"}),fields:BTreeMap::new(),parameter_fields:BTreeMap::new()}),
        ])).unwrap();
        let web_inputs = BoundInputs::new(client.clone(),chart_ports(),BTreeMap::from([
            ("bars".into(),bars_binding("source-b")),
            ("comparison".into(),bars_binding("source-a")),
        ])).unwrap();
        let (_, smoke) = gpui_kit::open_window(WindowOptions {
            window_bounds:Some(WindowBounds::Windowed(Bounds::new(point(px(40.),px(40.)),size(px(1000.),px(600.))))),
            ..Default::default()
        },cx,|window,cx| {
            let native = factory.create(CardContext{id:"native".into(),inputs:native_inputs},window,cx);
            let web = backend_bridge::build(openalgo::builder("dark","Bars input").with_initialization_script(CHECK),
                "app://openalgo/".into(),web_inputs,context(true,0),window,cx).expect("input bridge builds");
            cx.new(|_| Smoke{web,native,_host:host})
        }).expect("smoke window opens");
        cx.activate(true);
        let view = smoke.read(cx).web.view.clone();
        let (tx,rx) = mpsc::channel();
        cx.spawn(async move |cx| {
            let mut stage = 0;
            for _ in 0..100 {
                cx.background_executor().timer(Duration::from_millis(200)).await;
                let tx = tx.clone();
                cx.update(|cx| view.read(cx).evaluate_script_with_callback(
                    "window.__sdkSmokeResult || null",move|result|{let _=tx.send(result);}
                ).expect("query smoke state"));
                if let Ok(result) = rx.try_recv() {
                    let result:Value=serde_json::from_str(&result).unwrap_or(Value::Null);
                    if result.is_null(){continue;}
                    if result["ok"]!=true {eprintln!("Mapping smoke failed: {result}");std::process::exit(1);}
                    if stage==0 && result["stage"]=="mapped" {
                        println!("Mapped frontend/field schema smoke: {result}");
                        change.store(1, Ordering::SeqCst);
                        client.invalidate("source-b");
                        stage=1;
                    } else if stage==1 && result["stage"]=="pushed" {
                        println!("Host invalidation pushed changed bars to JavaScript: {result}");
                        cx.update(|cx| view.read(cx).evaluate_script(
                            "window.__sdkSmokeResetExpected=true"
                        ).unwrap());
                        delay_ms.store(1000, Ordering::SeqCst);
                        change.store(2, Ordering::SeqCst);
                        client.reset("source-b");
                        stage=2;
                    } else if stage==2 && result["stage"]=="reset" {
                        println!("Account reset clears the chart before delayed fixture refresh: {result}");
                        stage=3;
                    } else if stage==3 && result["stage"]=="reset-live" {
                        println!("Post-reset data reaches JavaScript: {result}");
                        let inputs=BoundInputs::new(client.clone(),chart_ports(),Bindings::new()).unwrap();
                        cx.update(|cx| {
                            let smoke=smoke.read(cx);
                            smoke.web.replace_inputs(inputs);
                            smoke.web.set_context(context(false,1),cx);
                            smoke.web.view.read(cx).evaluate_script(
                                "setTimeout(()=>{window.__sdkSmokeResult={stage:'unbound',ok:document.querySelector('.badge').textContent==='UNBOUND' && !document.getElementById('fallback').hidden}},50)"
                            ).unwrap();
                        });
                        stage=4;
                    } else if stage==4 && result["stage"]=="unbound" {
                        println!("Unmapped block smoke: {result}");
                        let inputs=BoundInputs::new(client.clone(),chart_ports(),BTreeMap::from([("bars".into(),bars_binding("source-a"))])).unwrap();
                        cx.update(|cx| {
                            let smoke=smoke.read(cx);
                            smoke.web.replace_inputs(inputs);
                            smoke.web.set_context(context(true,2),cx);
                            smoke.web.view.read(cx).evaluate_script(
                                "TradeSlope.inputs.request('bars',{interval:'1h'}).then(bars=>{window.__sdkSmokeResult={stage:'remapped',ok:bars[0].close===102}}).catch(error=>{window.__sdkSmokeResult={ok:false,error:String(error)}})"
                            ).unwrap();
                        });
                        stage=5;
                    } else if stage==5 && result["stage"]=="remapped" {
                        println!("Remapped provider without frontend changes: {result}");
                        if quotes.load(Ordering::SeqCst)==0 {std::process::exit(1);}
                        cx.update(|cx| cx.quit());
                        return;
                    }
                }
            }
            eprintln!("Typed input mapping smoke timed out");
            std::process::exit(1);
        }).detach();
    });
}
