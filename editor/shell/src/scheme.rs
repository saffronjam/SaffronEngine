//! The `saffron-img://` custom scheme. It serves the storefront's thumbnails and gallery images; the
//! storefront rewrites every remote thumbnail/gallery URL to `saffron-img://fetch/?u=<percent-encoded
//! url>` (`storefront/cachedImage.ts`), and this factory serves each request from the shared
//! `ResourceCache` (a hot in-RAM LRU over a disk blob store), so a screenful of images never
//! stampedes a provider CDN and repeat loads stay local.
//!
//! Each request is served by an **async** `ResourceHandler`: `open` returns immediately and drives the
//! cache read on the shared runtime, signalling readiness with `callback.cont()`. That keeps CEF's
//! single IO thread free, so image fetches actually run concurrently (through the cache's own
//! concurrency gate) instead of serializing one-at-a-time behind a blocking `create`.
//!
//! Registered as a **standard + secure + CORS + fetch-enabled** scheme (`on_register_custom_schemes`
//! in the `App`), which is what lets Chromium honour it for `<img src>` and `fetch()`. Served bytes
//! carry `Cache-Control: …, immutable` so Chromium's own cache absorbs repeat requests for the
//! content-addressed images.

use crate::async_rt;
use crate::connectors::ResourceCache;
use cef::rc::*;
use cef::*;
use std::sync::{Arc, Mutex};

/// Immutable content-addressed images: let Chromium's cache absorb repeats.
const CACHE_CONTROL: &str = "public, max-age=31536000, immutable";

/// The in-flight fetch for one request, filled by the `open` task and drained by `read`.
#[derive(Default)]
struct ImgState {
    ok: bool,
    bytes: Vec<u8>,
    mime: String,
    cursor: usize,
}

wrap_scheme_handler_factory! {
    pub struct ImgSchemeFactory {
        cache: Arc<ResourceCache>,
    }

    impl SchemeHandlerFactory {
        fn create(
            &self,
            _browser: Option<&mut Browser>,
            _frame: Option<&mut Frame>,
            _scheme_name: Option<&CefString>,
            request: Option<&mut Request>,
        ) -> Option<ResourceHandler> {
            // `saffron-img://fetch/?u=<percent-encoded remote url>` — split on the query param. An
            // empty target resolves to a 502 in `open` (never a blocking failure here).
            let url = request
                .map(|r| CefStringUtf16::from(&r.url()).to_string())
                .unwrap_or_default();
            let target = url
                .split_once("u=")
                .map(|(_, tail)| percent_decode(tail))
                .unwrap_or_default();
            Some(ImgResourceHandler::new(
                Arc::clone(&self.cache),
                target,
                Arc::new(Mutex::new(ImgState::default())),
            ))
        }
    }
}

wrap_resource_handler! {
    struct ImgResourceHandler {
        cache: Arc<ResourceCache>,
        target: String,
        state: Arc<Mutex<ImgState>>,
    }

    impl ResourceHandler {
        /// Kick the cache read off the CEF IO thread: report async handling (`handle_request = 0`),
        /// fetch on the shared runtime, and signal completion with `callback.cont()`. The old blocking
        /// `block_on` in `create` serialized every image on the one IO thread; this lets them overlap.
        fn open(
            &self,
            _request: Option<&mut Request>,
            handle_request: Option<&mut ::std::os::raw::c_int>,
            callback: Option<&mut Callback>,
        ) -> ::std::os::raw::c_int {
            if let Some(h) = handle_request {
                *h = 0;
            }
            let callback = callback.map(|c| c.clone());
            if self.target.is_empty() {
                if let Some(cb) = callback {
                    cb.cont();
                }
                return 1;
            }
            let cache = Arc::clone(&self.cache);
            let target = self.target.clone();
            let state = Arc::clone(&self.state);
            async_rt::rt().spawn(async move {
                let fetched = cache.bytes(&target).await;
                if let Ok(mut s) = state.lock()
                    && let Ok((bytes, mime)) = fetched
                {
                    s.bytes = bytes;
                    s.mime = mime;
                    s.ok = true;
                }
                if let Some(cb) = callback {
                    cb.cont();
                }
            });
            1
        }

        /// Reports the fetched status/length once the `open` task has finished (CEF calls this after
        /// `cont`). A failed fetch is a 502 with an empty body — the webview shows its broken glyph.
        fn response_headers(
            &self,
            response: Option<&mut Response>,
            response_length: Option<&mut i64>,
            _redirect_url: Option<&mut CefString>,
        ) {
            let (ok, mime, len) = match self.state.lock() {
                Ok(s) => (s.ok, s.mime.clone(), s.bytes.len()),
                Err(_) => (false, String::new(), 0),
            };
            if let Some(resp) = response {
                if ok {
                    resp.set_status(200);
                    resp.set_mime_type(Some(&CefString::from(mime.as_str())));
                    resp.set_header_by_name(
                        Some(&CefString::from("Cache-Control")),
                        Some(&CefString::from(CACHE_CONTROL)),
                        1,
                    );
                } else {
                    resp.set_status(502);
                }
            }
            if let Some(rl) = response_length {
                *rl = if ok { len as i64 } else { 0 };
            }
        }

        fn read(
            &self,
            data_out: *mut u8,
            bytes_to_read: ::std::os::raw::c_int,
            bytes_read: Option<&mut ::std::os::raw::c_int>,
            _callback: Option<&mut ResourceReadCallback>,
        ) -> ::std::os::raw::c_int {
            serve(&self.state, data_out, bytes_to_read, bytes_read)
        }

        /// The deprecated read path, kept identical so whichever CEF calls, the buffer drains the same.
        fn read_response(
            &self,
            data_out: *mut u8,
            bytes_to_read: ::std::os::raw::c_int,
            bytes_read: Option<&mut ::std::os::raw::c_int>,
            _callback: Option<&mut Callback>,
        ) -> ::std::os::raw::c_int {
            serve(&self.state, data_out, bytes_to_read, bytes_read)
        }
    }
}

/// Copy the next chunk of the fetched image into CEF's buffer. `bytes_read > 0` + `1` means more may
/// follow; `bytes_read = 0` + `0` is EOF (the whole buffer drained, or a failed/empty fetch).
fn serve(
    state: &Mutex<ImgState>,
    data_out: *mut u8,
    bytes_to_read: ::std::os::raw::c_int,
    bytes_read: Option<&mut ::std::os::raw::c_int>,
) -> ::std::os::raw::c_int {
    let done = |bytes_read: Option<&mut ::std::os::raw::c_int>| {
        if let Some(br) = bytes_read {
            *br = 0;
        }
        0
    };
    let Ok(mut s) = state.lock() else {
        return done(bytes_read);
    };
    if !s.ok || bytes_to_read <= 0 {
        return done(bytes_read);
    }
    let remaining = s.bytes.len().saturating_sub(s.cursor);
    if remaining == 0 {
        return done(bytes_read);
    }
    let n = remaining.min(bytes_to_read as usize);
    // SAFETY: CEF guarantees `data_out` is writable for `bytes_to_read` bytes; `n <= bytes_to_read`
    // and the source range is within `s.bytes`.
    unsafe {
        std::ptr::copy_nonoverlapping(s.bytes.as_ptr().add(s.cursor), data_out, n);
    }
    s.cursor += n;
    if let Some(br) = bytes_read {
        *br = n as ::std::os::raw::c_int;
    }
    1
}

/// The factory over the shell's shared `ResourceCache`, registered after `initialize`.
pub fn factory(cache: Arc<ResourceCache>) -> SchemeHandlerFactory {
    ImgSchemeFactory::new(cache)
}

/// Percent-decode a URL tail (`%XX` → byte).
pub(crate) fn percent_decode(s: &str) -> String {
    let bytes = s.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'%' && i + 2 < bytes.len() {
            let hi = (bytes[i + 1] as char).to_digit(16);
            let lo = (bytes[i + 2] as char).to_digit(16);
            if let (Some(h), Some(l)) = (hi, lo) {
                out.push((h * 16 + l) as u8);
                i += 3;
                continue;
            }
        }
        out.push(bytes[i]);
        i += 1;
    }
    String::from_utf8_lossy(&out).into_owned()
}
