//! Real-window smoke check (not headless): cargo run --example openalgo_smoke
//! Exercises packaged assets in the same native webview used by cards.
#[path = "support/openalgo.rs"]
mod openalgo;

use std::sync::{
    Arc,
    atomic::{AtomicBool, Ordering},
    mpsc,
};
use std::time::{Duration, Instant};

use gpui_kit::*;
use gpui_webview::WebView;

struct Smoke {
    chart: Entity<WebView>,
}

impl Render for Smoke {
    fn render(&mut self, _: &mut Window, _: &mut Context<Self>) -> impl IntoElement {
        div().size_full().child(self.chart.clone())
    }
}

const CHECK_READY: &str = r#"
window.addEventListener('DOMContentLoaded', async () => {
    try {
        await window.TradeSlopeChart.ready;
        await new Promise(requestAnimationFrame);
        await new Promise(requestAnimationFrame);
        const state = window.TradeSlopeChart.getState();
        const canvas = document.querySelector('canvas');
        window.ipc.postMessage(JSON.stringify({
            stage: 'ready',
            ok: !!canvas && canvas.width > 0 && canvas.height > 0
                && window.__initialHostConfigurationCorrect
                && state.symbol === 'EARLY-HOST' && state.theme === 'dark',
            state
        }));
    } catch (error) {
        window.ipc.postMessage(JSON.stringify({ stage: 'error', error: String(error) }));
    }
}, { once: true });
"#;

fn main() {
    let passed = Arc::new(AtomicBool::new(false));
    let result = passed.clone();
    application().run(move |cx| {
        gpui_kit::init(cx);
        let (tx, rx) = mpsc::channel::<String>();
        let (handle, smoke) = gpui_kit::open_window(
            WindowOptions {
                window_bounds: Some(WindowBounds::Windowed(Bounds::new(
                    point(px(80.), px(80.)),
                    size(px(800.), px(600.)),
                ))),
                ..Default::default()
            },
            cx,
            |window, cx| {
                let early_update = format!(
                    "window.__initialHostConfigurationCorrect = \
                        window.__chartConfiguration?.symbol === 'INITIAL-HOST' && \
                        window.__chartConfiguration?.theme === 'light'; {}",
                    openalgo::configuration_script("dark", "EARLY-HOST")
                );
                let builder = openalgo::builder("light", "INITIAL-HOST")
                    .with_initialization_script(early_update)
                    .with_initialization_script(CHECK_READY)
                    .with_ipc_handler(move |request| {
                        let _ = tx.send(request.body().clone());
                    });
                let chart = WebView::build(builder, window, cx).expect("native webview builds");
                let chart = cx.new(|_| chart);
                cx.new(|_| Smoke { chart })
            },
        )
        .expect("smoke window opens");
        cx.activate(true);
        let chart = smoke.read(cx).chart.clone();
        cx.spawn(async move |cx| {
            let deadline = Instant::now() + Duration::from_secs(20);
            let mut resized = false;
            while Instant::now() < deadline {
                cx.background_executor()
                    .timer(Duration::from_millis(100))
                    .await;
                if let Ok(message) = rx.try_recv() {
                    println!("OpenAlgo native smoke: {message}");
                    let data: serde_json::Value = serde_json::from_str(&message).expect("JSON IPC");
                    if data["ok"] != true {
                        // AppKit's quit terminates the process, so an assertion
                        // after application().run() would not report a failure.
                        std::process::exit(1);
                    }
                    if data["stage"] == "ready" && !resized {
                        cx.update(|cx| {
                            chart
                                .read(cx)
                                .evaluate_script(&format!(
                                    "{} \
                                 window.TradeSlopeChart.ready.then(() => {{ \
                                     const state = window.TradeSlopeChart.getState(); \
                                     window.ipc.postMessage(JSON.stringify({{ \
                                         stage: 'configured', \
                                         ok: state.symbol === 'SMOKE' && state.theme === 'light' \
                                     }})); \
                                 }});",
                                    openalgo::configuration_script("light", "SMOKE")
                                ))
                                .expect("production configuration update runs");
                        });
                    } else if data["stage"] == "configured" && !resized {
                        cx.update(|cx| {
                            chart.read(cx).set_visible(false).expect("hide succeeds");
                            assert!(!chart.read(cx).visible());
                            handle
                                .update(cx, |_, window, _| {
                                    window.resize(size(px(360.), px(320.)));
                                })
                                .expect("window resizes");
                        });
                        cx.background_executor()
                            .timer(Duration::from_millis(200))
                            .await;
                        cx.update(|cx| {
                            chart.update(cx, |view, cx| {
                                view.set_visible(true).expect("show succeeds");
                                cx.notify();
                            });
                            assert!(chart.read(cx).visible());
                            handle
                                .update(cx, |_, window, _| {
                                    println!("Resized GPUI viewport: {:?}", window.viewport_size());
                                    window.refresh();
                                })
                                .expect("window remains open");
                        });
                        cx.background_executor()
                            .timer(Duration::from_millis(500))
                            .await;
                        cx.update(|cx| {
                            chart
                                .read(cx)
                                .evaluate_script(
                                    r#"
                            const canvas = document.querySelector('canvas');
                            const state = window.TradeSlopeChart.getState();
                            window.ipc.postMessage(JSON.stringify({
                                stage: 'resized',
                                ok: !!canvas && canvas.width > 0 && canvas.height > 0
                                    && innerWidth <= 400 && state.symbol === 'SMOKE'
                                    && state.theme === 'light',
                                width: innerWidth, height: innerHeight
                            }));
                        "#,
                                )
                                .expect("resize check script runs")
                        });
                        resized = true;
                    } else if data["stage"] == "resized" {
                        result.store(true, Ordering::SeqCst);
                        cx.update(|cx| cx.quit());
                        return;
                    }
                }
            }
            eprintln!("OpenAlgo native smoke timed out");
            std::process::exit(1);
        })
        .detach();
    });
    assert!(
        passed.load(Ordering::SeqCst),
        "native OpenAlgo smoke failed"
    );
}
