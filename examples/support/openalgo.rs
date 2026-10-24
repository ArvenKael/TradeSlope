//! A packaged web document, not a replacement for the GPUI workspace.
//! No source checkout, development server, CDN, or OpenAlgo backend is needed.
use std::borrow::Cow;

use rust_embed::RustEmbed;
use wry::http::{Request, Response, StatusCode};

#[derive(RustEmbed)]
#[folder = "extensions/openalgo/"]
struct ChartAssets;

const DOCUMENT_URL: &str = "app://openalgo/index.html";

fn asset_response(request: Request<Vec<u8>>) -> Response<Cow<'static, [u8]>> {
    let path = request.uri().path().trim_start_matches('/');
    let file = ChartAssets::get(path);
    let (status, body) = match file {
        Some(file) => (StatusCode::OK, file.data),
        None => (
            StatusCode::NOT_FOUND,
            Cow::Borrowed(b"Asset not found".as_slice()),
        ),
    };
    let content_type = match path.rsplit('.').next() {
        Some("html") => "text/html; charset=utf-8",
        Some("js" | "mjs") => "text/javascript; charset=utf-8",
        Some("css") => "text/css; charset=utf-8",
        Some("svg") => "image/svg+xml",
        Some("json") => "application/json",
        _ => "application/octet-stream",
    };
    Response::builder()
        .status(status)
        .header("Content-Type", content_type)
        .header("X-Content-Type-Options", "nosniff")
        .body(body)
        .expect("static asset headers are valid")
}

/// JSON encoding keeps page names/symbols as data, never executable JavaScript.
pub fn configuration_script(theme: &str, symbol: &str) -> String {
    let theme = serde_json::to_string(theme).expect("strings are JSON serializable");
    let symbol = serde_json::to_string(symbol).expect("strings are JSON serializable");
    format!(
        "window.__chartConfiguration = {{ theme: {theme}, symbol: {symbol} }}; \
         window.__applyChartConfiguration?.();"
    )
}

pub fn builder(theme: &str, symbol: &str) -> wry::WebViewBuilder<'static> {
    let configuration = configuration_script(theme, symbol);
    // The handler runs before navigation. Defer host calls until the packaged
    // document and its widget exist; later updates reuse that same widget.
    let initialization = format!(
        "{configuration} \
         window.__applyChartConfiguration = () => {{ \
             if (!window.TradeSlopeChart) return; \
             window.TradeSlopeChart.ready.then(() => {{ \
                 const config = window.__chartConfiguration; \
                 window.TradeSlopeChart.setTheme(config.theme); \
                 if (!window.TradeSlope) window.TradeSlopeChart.setSymbol(config.symbol); \
             }}).catch(console.error); \
         }}; \
         window.addEventListener('DOMContentLoaded', window.__applyChartConfiguration, {{ once: true }});"
    );
    wry::WebViewBuilder::new()
        .with_custom_protocol("app".into(), |_, request| asset_response(request))
        .with_initialization_script(initialization)
        .with_navigation_handler(|url| {
            url == DOCUMENT_URL || url == "http://app.openalgo/index.html"
        })
        .with_new_window_req_handler(|_, _| wry::NewWindowResponse::Deny)
        .with_url(DOCUMENT_URL)
}

#[cfg(test)]
mod tests {
    use super::{asset_response, configuration_script};
    use wry::http::{Request, StatusCode};

    #[test]
    fn packaged_document_and_scripts_have_browser_mime_types() {
        for (path, mime) in [
            ("index.html", "text/html; charset=utf-8"),
            ("card.js", "text/javascript; charset=utf-8"),
            ("card.css", "text/css; charset=utf-8"),
        ] {
            let response = asset_response(
                Request::get(format!("app://openalgo/{path}"))
                    .body(vec![])
                    .unwrap(),
            );
            assert_eq!(response.status(), StatusCode::OK);
            assert_eq!(response.headers()["Content-Type"], mime);
            assert!(!response.body().is_empty());
        }
    }

    #[test]
    fn missing_or_traversing_asset_is_not_served() {
        for path in ["missing.js", "../Cargo.toml", "%2e%2e/Cargo.toml"] {
            let response = asset_response(
                Request::get(format!("app://openalgo/{path}"))
                    .body(vec![])
                    .unwrap(),
            );
            assert_eq!(response.status(), StatusCode::NOT_FOUND);
        }
    }

    #[test]
    fn host_strings_are_json_encoded() {
        let symbol = "BTC\";alert('oops');\n";
        assert!(
            configuration_script("dark", symbol).contains(&serde_json::to_string(symbol).unwrap())
        );
    }
}
