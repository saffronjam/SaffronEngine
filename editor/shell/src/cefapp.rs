//! The CEF application handler: the Chromium switches the shell needs, the render-subprocess
//! handler, and custom-scheme registration.

use crate::{ipc_render, schemes};
use cef::*;

/// Sets the Chromium command-line switches CEF's GPU subprocess needs; `SAFFRON_CEF_SWITCHES` adds
/// more at runtime.
#[derive(Clone)]
pub(crate) struct ShellApp;

wrap_app! {
    pub(crate) struct AppBuilder {
        app: ShellApp,
    }

    impl App {
        fn on_before_command_line_processing(
            &self,
            _process_type: Option<&cef::CefStringUtf16>,
            command_line: Option<&mut cef::CommandLine>,
        ) {
            let Some(cl) = command_line else {
                return;
            };
            cl.append_switch(Some(&"no-sandbox".into()));
            cl.append_switch(Some(&"noerrdialogs".into()));
            // Overlay scrollbars: draw scrollbars on top of content (auto-hiding, no reserved layout
            // gutter) instead of the classic space-reserving bar. The theme tint comes from
            // `scrollbar-color` in the frontend `styles.css`; this feature makes them overlay.
            cl.append_switch_with_value(
                Some(&"enable-features".into()),
                Some(&"OverlayScrollbar".into()),
            );
            // Windowless OSR reports no hover-capable pointer, so Blink evaluates `(hover: none)` /
            // `(pointer: coarse)` and disables every hover media query — and Tailwind v4 gates `hover:`
            // / `group-hover:` behind `@media (hover: hover)`, so all hover styling silently dies. The
            // editor is always a desktop mouse app; declare a fine, hovering pointer to Blink.
            // (`HoverType::kHover = 2`, `PointerType::kFine = 4` — a single comma-joined switch value,
            // so it must not go through the comma-split `SAFFRON_CEF_SWITCHES` path below.)
            cl.append_switch_with_value(
                Some(&"blink-settings".into()),
                Some(
                    &"primaryHoverType=2,availableHoverTypes=2,primaryPointerType=4,availablePointerTypes=4"
                        .into(),
                ),
            );
            // The Ozone/GL set that brings the GPU subprocess up is host-specific, so it is passed
            // at runtime as comma-separated `k=v` / bare flags rather than baked in.
            if let Ok(extra) = std::env::var("SAFFRON_CEF_SWITCHES") {
                for sw in extra.split(',').filter(|s| !s.is_empty()) {
                    match sw.split_once('=') {
                        Some((k, v)) => cl.append_switch_with_value(Some(&k.into()), Some(&v.into())),
                        None => cl.append_switch(Some(&sw.into())),
                    }
                }
            }
        }

        fn render_process_handler(&self) -> Option<RenderProcessHandler> {
            // Invoked in the render subprocess: registers `cefQuery` via the renderer-side router.
            Some(ipc_render::render_process_handler())
        }

        fn on_register_custom_schemes(&self, registrar: Option<&mut SchemeRegistrar>) {
            // Custom schemes must be registered identically in every process; the handlers are
            // registered in the browser process after `initialize`.
            if let Some(registrar) = registrar {
                schemes::register(registrar);
            }
        }
    }
}

impl AppBuilder {
    pub(crate) fn build(app: ShellApp) -> cef::App {
        Self::new(app)
    }
}
