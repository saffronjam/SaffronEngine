//! The browser-process half of the JS↔native IPC transport via CEF's message router. The frontend
//! calls `window.cefQuery({ request, onSuccess, onFailure })` with `request = JSON{ command, args }`;
//! the router carries it to the browser process, where `CommandQueryHandler` parses it and
//! dispatches via [`crate::commands::dispatch`] on a worker thread (never the UI thread),
//! completing the query with the handler's reply. The browser side is forwarded from the `Client`
//! (`on_process_message_received`) and `LifeSpanHandler` (`on_before_close`, canceling pending
//! queries on browser destruction). The renderer side, which registers `cefQuery` on each V8
//! context, lives in `ipc_render.rs` so the macOS helper binary can include it too.

use crate::commands;
use crate::control::ControlError;
use crate::state::ShellState;
use cef::wrapper::message_router::{
    BrowserSideCallback, BrowserSideHandler, BrowserSideRouter, MessageRouterBrowserSide,
    MessageRouterBrowserSideHandlerCallbacks, MessageRouterConfig,
};
use cef::*;
use serde_json::{Value, json};
use std::sync::{Arc, Mutex};

/// Handles every `cefQuery`: `{ command, args }` → [`commands::dispatch`] → `onSuccess(result)` /
/// `onFailure(code, ControlFailureDto)`.
struct CommandQueryHandler {
    state: Arc<ShellState>,
}

impl BrowserSideHandler for CommandQueryHandler {
    fn on_query_str(
        &self,
        _browser: Option<Browser>,
        _frame: Option<Frame>,
        _query_id: i64,
        request: &str,
        _persistent: bool,
        callback: Arc<Mutex<dyn BrowserSideCallback>>,
    ) -> bool {
        let state = Arc::clone(&self.state);
        let request = request.to_string();
        // Handlers can block (the control round-trip is up to 5s; engine spawn touches the fs); run
        // off the UI thread. Callback methods may be executed on any browser-process thread.
        std::thread::spawn(move || {
            let parsed: Value = match serde_json::from_str(&request) {
                Ok(value) => value,
                Err(err) => {
                    let failure = ControlError::bridge(format!("bad query json: {err}"));
                    let payload =
                        serde_json::to_string(&failure).expect("ControlFailureDto serializes");
                    if let Ok(cb) = callback.lock() {
                        cb.failure(-1, &payload);
                    }
                    return;
                }
            };
            let command = parsed.get("command").and_then(|v| v.as_str()).unwrap_or("");
            let args = parsed.get("args").cloned().unwrap_or_else(|| json!({}));
            match commands::dispatch(&state, command, args) {
                Ok(result) => {
                    if let Ok(cb) = callback.lock() {
                        cb.success_str(&result.to_string());
                    }
                }
                Err(err) => {
                    let payload =
                        serde_json::to_string(&err).expect("ControlFailureDto serializes");
                    if let Ok(cb) = callback.lock() {
                        cb.failure(1, &payload);
                    }
                }
            }
        });
        true
    }
}

/// Create the browser-side router with the one command-query handler. Called once in the browser
/// process after `initialize`, on the UI thread.
pub fn browser_router(state: Arc<ShellState>) -> Arc<BrowserSideRouter> {
    let router = BrowserSideRouter::new(MessageRouterConfig::default());
    let _ = router.add_handler(Arc::new(CommandQueryHandler { state }), false);
    router
}

/// Forward `on_process_message_received` from the `Client` to the browser router.
pub fn browser_on_process_message(
    router: &Arc<BrowserSideRouter>,
    browser: Option<&mut Browser>,
    frame: Option<&mut Frame>,
    source_process: ProcessId,
    message: Option<&mut ProcessMessage>,
) -> ::std::os::raw::c_int {
    router.on_process_message_received(
        browser.cloned(),
        frame.cloned(),
        source_process,
        message.cloned(),
    ) as _
}

wrap_life_span_handler! {
    struct RouterLifeSpan {
        router: Arc<BrowserSideRouter>,
    }

    impl LifeSpanHandler {
        fn on_before_close(&self, browser: Option<&mut Browser>) {
            self.router.on_before_close(browser.cloned());
        }
    }
}

/// The `LifeSpanHandler` the `Client` returns, so the router cancels pending queries on close.
pub fn life_span_handler(router: Arc<BrowserSideRouter>) -> LifeSpanHandler {
    RouterLifeSpan::new(router)
}
