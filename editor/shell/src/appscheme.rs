//! The `saffron-app://` scheme serves the bundled React UI from disk in a packaged build. The dev
//! loop loads the UI from Vite (`SAFFRON_DEV_URL`); a distributable ships no dev server, so the shell
//! navigates to `saffron-app://localhost/index.html` and this factory serves every request from the
//! UI directory (`SAFFRON_UI_DIR`, set by the AppImage's AppRun). Registered as a
//! **standard + secure + CORS + fetch-enabled** scheme (`on_register_custom_schemes`), which gives it
//! a real origin so Chromium loads the Vite `crossorigin` ES-module bundle (absolute `/assets/…`
//! refs resolve against the `saffron-app://localhost` origin) and its `fetch` requests.

use crate::scheme::percent_decode;
use cef::rc::*;
use cef::*;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

/// The document a packaged build navigates to.
pub const INDEX_URL: &str = "saffron-app://localhost/index.html";

/// One served file, or an error status with an empty body; drained by `read`.
#[derive(Default)]
struct FileState {
    status: u16,
    bytes: Vec<u8>,
    mime: String,
    cursor: usize,
}

wrap_scheme_handler_factory! {
    pub struct AppSchemeFactory {
        root: Arc<PathBuf>,
    }

    impl SchemeHandlerFactory {
        fn create(
            &self,
            _browser: Option<&mut Browser>,
            _frame: Option<&mut Frame>,
            _scheme_name: Option<&CefString>,
            request: Option<&mut Request>,
        ) -> Option<ResourceHandler> {
            let url = request
                .map(|r| CefStringUtf16::from(&r.url()).to_string())
                .unwrap_or_default();
            Some(AppResourceHandler::new(Arc::new(Mutex::new(load(&self.root, &url)))))
        }
    }
}

wrap_resource_handler! {
    struct AppResourceHandler {
        state: Arc<Mutex<FileState>>,
    }

    impl ResourceHandler {
        /// The file is read up-front in `create`, so completion is immediate.
        fn open(
            &self,
            _request: Option<&mut Request>,
            handle_request: Option<&mut ::std::os::raw::c_int>,
            callback: Option<&mut Callback>,
        ) -> ::std::os::raw::c_int {
            if let Some(h) = handle_request {
                *h = 0;
            }
            if let Some(cb) = callback {
                cb.cont();
            }
            1
        }

        fn response_headers(
            &self,
            response: Option<&mut Response>,
            response_length: Option<&mut i64>,
            _redirect_url: Option<&mut CefString>,
        ) {
            let (status, mime, len) = match self.state.lock() {
                Ok(s) => (s.status, s.mime.clone(), s.bytes.len()),
                Err(_) => (500, String::new(), 0),
            };
            if let Some(resp) = response {
                resp.set_status(i32::from(status));
                if !mime.is_empty() {
                    resp.set_mime_type(Some(&CefString::from(mime.as_str())));
                }
            }
            if let Some(rl) = response_length {
                *rl = len as i64;
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

/// Resolve a request URL to a file under `root` and read it. `/` maps to `index.html`; a path that
/// escapes the root is a 403; a missing file is a 404.
fn load(root: &Path, url: &str) -> FileState {
    let full = root.join(request_path(url));
    if !full.starts_with(root) {
        return FileState {
            status: 403,
            ..Default::default()
        };
    }
    match std::fs::read(&full) {
        Ok(bytes) => FileState {
            status: 200,
            mime: mime_for(&full).to_owned(),
            bytes,
            cursor: 0,
        },
        Err(_) => FileState {
            status: 404,
            ..Default::default()
        },
    }
}

/// The sanitized, root-relative path for a `saffron-app://host/<path>` URL: strip the scheme + host +
/// query/fragment, percent-decode, drop `.`/`..`/empty segments so a request can't climb out of the
/// root, and default an empty path to `index.html`.
fn request_path(url: &str) -> PathBuf {
    let after_scheme = url.split_once("://").map_or(url, |(_, rest)| rest);
    let path = after_scheme.split_once('/').map_or("", |(_, rest)| rest);
    let path = path.split(['?', '#']).next().unwrap_or("");
    let mut out = PathBuf::new();
    for segment in percent_decode(path).split('/') {
        match segment {
            "" | "." | ".." => continue,
            other => out.push(other),
        }
    }
    if out.as_os_str().is_empty() {
        out.push("index.html");
    }
    out
}

/// The MIME type for a built asset, by extension.
fn mime_for(path: &Path) -> &'static str {
    match path.extension().and_then(|ext| ext.to_str()) {
        Some("html") => "text/html",
        Some("js" | "mjs") => "text/javascript",
        Some("css") => "text/css",
        Some("json" | "map") => "application/json",
        Some("svg") => "image/svg+xml",
        Some("png") => "image/png",
        Some("jpg" | "jpeg") => "image/jpeg",
        Some("gif") => "image/gif",
        Some("webp") => "image/webp",
        Some("ico") => "image/x-icon",
        Some("woff2") => "font/woff2",
        Some("woff") => "font/woff",
        Some("ttf") => "font/ttf",
        Some("wasm") => "application/wasm",
        Some("txt") => "text/plain",
        _ => "application/octet-stream",
    }
}

/// Copy the next chunk of the served file into CEF's buffer. `bytes_read > 0` + `1` means more may
/// follow; `bytes_read = 0` + `0` is EOF (buffer drained, or a non-200 response).
fn serve(
    state: &Mutex<FileState>,
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
    if s.status != 200 || bytes_to_read <= 0 {
        return done(bytes_read);
    }
    let remaining = s.bytes.len().saturating_sub(s.cursor);
    if remaining == 0 {
        return done(bytes_read);
    }
    let n = remaining.min(bytes_to_read as usize);
    // SAFETY: CEF guarantees `data_out` is writable for `bytes_to_read` bytes; `n <= bytes_to_read`
    // and the copied range lies within `s.bytes`.
    unsafe {
        std::ptr::copy_nonoverlapping(s.bytes.as_ptr().add(s.cursor), data_out, n);
    }
    s.cursor += n;
    if let Some(br) = bytes_read {
        *br = n as ::std::os::raw::c_int;
    }
    1
}

/// The factory over the bundled UI root, registered after `initialize`.
pub fn factory(ui_dir: PathBuf) -> SchemeHandlerFactory {
    AppSchemeFactory::new(Arc::new(ui_dir))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn request_path_defaults_root_to_index() {
        assert_eq!(
            request_path("saffron-app://localhost/"),
            PathBuf::from("index.html")
        );
        assert_eq!(
            request_path("saffron-app://localhost"),
            PathBuf::from("index.html")
        );
    }

    #[test]
    fn request_path_maps_assets_and_strips_query() {
        assert_eq!(
            request_path("saffron-app://localhost/assets/index-abc.js?v=1"),
            PathBuf::from("assets/index-abc.js"),
        );
    }

    #[test]
    fn request_path_drops_traversal_segments() {
        assert_eq!(
            request_path("saffron-app://localhost/../../etc/passwd"),
            PathBuf::from("etc/passwd")
        );
    }

    #[test]
    fn mime_for_known_extensions() {
        assert_eq!(mime_for(Path::new("a/index.html")), "text/html");
        assert_eq!(mime_for(Path::new("a/x.js")), "text/javascript");
        assert_eq!(mime_for(Path::new("a/x.css")), "text/css");
        assert_eq!(mime_for(Path::new("a/f.woff2")), "font/woff2");
    }

    #[test]
    fn load_serves_root_and_404s_missing() {
        let dir = std::env::temp_dir().join("saffron-appscheme-load-test");
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("index.html"), b"<!doctype html><title>x</title>").unwrap();

        let served = load(&dir, "saffron-app://localhost/");
        assert_eq!(served.status, 200);
        assert_eq!(served.mime, "text/html");
        assert_eq!(served.bytes, b"<!doctype html><title>x</title>");
        assert_eq!(load(&dir, "saffron-app://localhost/missing.js").status, 404);

        let _ = std::fs::remove_dir_all(&dir);
    }
}
