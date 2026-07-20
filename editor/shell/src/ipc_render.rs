//! The renderer-process half of the `cefQuery` message router: registers `window.cefQuery` on
//! each V8 context. This runs in whatever binary hosts CEF's render subprocess — the shell
//! itself on Linux (single-executable re-exec), the helper binary on macOS — so it depends on
//! nothing but `cef` and is included from both crate roots.

use cef::wrapper::message_router::{
    MessageRouterConfig, MessageRouterRendererSide, MessageRouterRendererSideHandlerCallbacks,
    RendererSideRouter,
};
use cef::*;
use std::sync::Arc;

wrap_render_process_handler! {
    struct RouterRenderProcess {
        router: Arc<RendererSideRouter>,
    }

    impl RenderProcessHandler {
        fn on_context_created(
            &self,
            browser: Option<&mut Browser>,
            frame: Option<&mut Frame>,
            context: Option<&mut V8Context>,
        ) {
            self.router
                .on_context_created(browser.cloned(), frame.cloned(), context.cloned());
        }

        fn on_context_released(
            &self,
            browser: Option<&mut Browser>,
            frame: Option<&mut Frame>,
            context: Option<&mut V8Context>,
        ) {
            self.router
                .on_context_released(browser.cloned(), frame.cloned(), context.cloned());
        }

        fn on_process_message_received(
            &self,
            browser: Option<&mut Browser>,
            frame: Option<&mut Frame>,
            source_process: ProcessId,
            message: Option<&mut ProcessMessage>,
        ) -> ::std::os::raw::c_int {
            self.router.on_process_message_received(
                browser.cloned(),
                frame.cloned(),
                Some(source_process),
                message.cloned(),
            ) as _
        }
    }
}

/// The `RenderProcessHandler` the `App` returns (invoked in the render subprocess): it registers
/// `cefQuery` on each V8 context via the renderer-side router. The router is a **process-stable
/// singleton** — the V8 `cefQuery` handler holds only a `Weak` ref to it, so a fresh router per
/// call (with CEF retaining just the latest handler) would drop it and make every query silently
/// no-op.
pub fn render_process_handler() -> RenderProcessHandler {
    thread_local! {
        static ROUTER: Arc<RendererSideRouter> =
            RendererSideRouter::new(MessageRouterConfig::default());
    }
    ROUTER.with(|router| RouterRenderProcess::new(Arc::clone(router)))
}
