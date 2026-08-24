//! The `--web` interface: routing and response building, with no socket in
//! sight.
//!
//! The server exists because the analysis is already a JSON document and every
//! Arch desktop already has a browser. Rendering it in one costs three static
//! files and no dependency; the alternative — linking a GUI toolkit — costs a
//! webview or a widget set, which on a tool whose entire purpose is reclaiming
//! disk space would be an odd thing to make people install.
//!
//! Everything here is pure. `capability` owns the listener, `main` owns the
//! scan, and this module only decides what a request means and what bytes go
//! back, which is what makes it testable without binding a port.

/// The page itself.
const INDEX: &str = include_str!("../web/index.html");
/// Its stylesheet.
const STYLE: &str = include_str!("../web/style.css");
/// Its script.
const SCRIPT: &str = include_str!("../web/app.js");

/// The largest request head worth reading, in bytes.
///
/// A browser sends well under this. Anything larger is not a browser, and the
/// server would rather answer it than keep reading it.
pub const MAX_HEAD: usize = 8_192;

/// What a request was asking for.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Route {
    /// The page.
    Index,
    /// The stylesheet.
    Style,
    /// The script.
    Script,
    /// The analysis, as the same JSON document `--json` prints.
    Inventory,
    /// A path this server does not have.
    NotFound,
    /// Something that did not parse as a request this server answers.
    BadRequest,
    /// A request that parsed but was addressed to somebody else.
    ///
    /// The server binds loopback only, but a page on the public internet can
    /// still point a script at `http://<name-that-resolves-to-127.0.0.1>:8080`
    /// and read the reply. Checking the `Host` header is what stops it: a
    /// browser sends the name it dialled, and this server answers only to its
    /// own two.
    Forbidden,
}

/// A response that needs no scan: everything except the inventory.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Canned {
    /// HTTP status line text, e.g. `200 OK`.
    pub status: &'static str,
    /// Value for the `Content-Type` header.
    pub content_type: &'static str,
    /// The body.
    pub body: &'static str,
}

/// Decide what a request head is asking for.
///
/// `port` is the port the listener actually bound, which is not always the one
/// that was asked for: `--port 0` takes whatever is free.
pub fn route(head: &str, port: u16) -> Route {
    let Some(request_line) = head.lines().next() else {
        return Route::BadRequest;
    };

    let mut fields = request_line.split_whitespace();
    let (Some(method), Some(target)) = (fields.next(), fields.next()) else {
        return Route::BadRequest;
    };

    // Read-only by construction. A verb that implies a change is refused
    // rather than quietly treated as a GET.
    if method != "GET" {
        return Route::BadRequest;
    }

    if !host_is_local(head, port) {
        return Route::Forbidden;
    }

    // The query string is not used by anything here, but a browser is free to
    // append one, so it is trimmed rather than treated as part of the path.
    let path = target.split('?').next().unwrap_or(target);

    match path {
        "/" | "/index.html" => Route::Index,
        "/style.css" => Route::Style,
        "/app.js" => Route::Script,
        "/api/inventory" => Route::Inventory,
        _other => Route::NotFound,
    }
}

/// Whether the `Host` header names this server rather than one that merely
/// resolves to it.
fn host_is_local(head: &str, port: u16) -> bool {
    let Some(host) = header(head, "host") else {
        // HTTP/1.1 requires the header. Something that omits it is not the
        // browser this server is for.
        return false;
    };

    let expected = [
        format!("127.0.0.1:{port}"),
        format!("localhost:{port}"),
        format!("[::1]:{port}"),
    ];
    expected.iter().any(|candidate| candidate == &host)
}

/// The value of a header, matched case-insensitively.
fn header(head: &str, name: &str) -> Option<String> {
    head.lines()
        .skip(1)
        .filter_map(|line| line.split_once(':'))
        .find(|(key, _value)| key.trim().eq_ignore_ascii_case(name))
        .map(|(_key, value)| value.trim().to_owned())
}

/// The response for every route that does not need a scan.
///
/// [`Route::Inventory`] is the exception: it is answered by the caller, which
/// is the only part of the program allowed to touch a filesystem.
pub fn canned(route: Route) -> Option<Canned> {
    match route {
        Route::Index => Some(Canned {
            status: "200 OK",
            content_type: "text/html; charset=utf-8",
            body: INDEX,
        }),
        Route::Style => Some(Canned {
            status: "200 OK",
            content_type: "text/css; charset=utf-8",
            body: STYLE,
        }),
        Route::Script => Some(Canned {
            status: "200 OK",
            content_type: "text/javascript; charset=utf-8",
            body: SCRIPT,
        }),
        Route::NotFound => Some(Canned {
            status: "404 Not Found",
            content_type: "text/plain; charset=utf-8",
            body: "not found\n",
        }),
        Route::BadRequest => Some(Canned {
            status: "400 Bad Request",
            content_type: "text/plain; charset=utf-8",
            body: "this server answers GET requests only\n",
        }),
        Route::Forbidden => Some(Canned {
            status: "403 Forbidden",
            content_type: "text/plain; charset=utf-8",
            body: "this server answers to localhost only\n",
        }),
        Route::Inventory => None,
    }
}

/// Build a complete HTTP/1.1 response.
///
/// Every response closes the connection. Keeping it open would mean tracking
/// state per client to save a socket on a server with exactly one user.
pub fn response(status: &str, content_type: &str, body: &[u8]) -> Vec<u8> {
    let mut head = format!(
        "HTTP/1.1 {status}\r\n\
         Content-Type: {content_type}\r\n\
         Content-Length: {length}\r\n\
         Cache-Control: no-store\r\n\
         X-Content-Type-Options: nosniff\r\n\
         Content-Security-Policy: default-src 'self'\r\n\
         Connection: close\r\n\r\n",
        length = body.len()
    )
    .into_bytes();

    head.extend_from_slice(body);
    head
}

/// The address to print once the listener is up.
pub fn address(port: u16) -> String {
    format!("http://127.0.0.1:{port}/")
}

#[cfg(test)]
mod tests {
    use super::{address, canned, response, route, Route};

    fn get(path: &str, host: &str) -> String {
        format!("GET {path} HTTP/1.1\r\nHost: {host}\r\nAccept: */*\r\n\r\n")
    }

    #[test]
    fn the_page_and_its_assets_are_routed() {
        assert_eq!(route(&get("/", "127.0.0.1:8080"), 8080), Route::Index);
        assert_eq!(
            route(&get("/index.html", "localhost:8080"), 8080),
            Route::Index
        );
        assert_eq!(
            route(&get("/style.css", "localhost:8080"), 8080),
            Route::Style
        );
        assert_eq!(
            route(&get("/app.js", "localhost:8080"), 8080),
            Route::Script
        );
        assert_eq!(
            route(&get("/api/inventory", "localhost:8080"), 8080),
            Route::Inventory
        );
    }

    #[test]
    fn a_query_string_does_not_change_the_route() {
        assert_eq!(
            route(&get("/api/inventory?t=1712", "localhost:8080"), 8080),
            Route::Inventory
        );
    }

    #[test]
    fn an_unknown_path_is_not_found() {
        assert_eq!(
            route(&get("/etc/passwd", "localhost:8080"), 8080),
            Route::NotFound
        );
        assert_eq!(
            route(&get("/../../etc/shadow", "localhost:8080"), 8080),
            Route::NotFound
        );
    }

    #[test]
    fn nothing_but_get_is_answered() {
        let head = "POST /api/inventory HTTP/1.1\r\nHost: localhost:8080\r\n\r\n";
        assert_eq!(route(head, 8080), Route::BadRequest);
        assert_eq!(route("", 8080), Route::BadRequest);
        assert_eq!(route("nonsense\r\n\r\n", 8080), Route::BadRequest);
    }

    #[test]
    fn a_host_this_server_does_not_answer_to_is_refused() {
        // The shape of a DNS rebinding attempt: a name that resolves to
        // 127.0.0.1, dialled from a page the user did not open.
        assert_eq!(
            route(&get("/api/inventory", "evil.example.com:8080"), 8080),
            Route::Forbidden
        );
        // The right name on the wrong port is somebody else's server.
        assert_eq!(
            route(&get("/api/inventory", "localhost:9999"), 8080),
            Route::Forbidden
        );
        // HTTP/1.1 requires a Host header; a request without one is not a
        // browser this server is for.
        assert_eq!(
            route("GET / HTTP/1.1\r\nAccept: */*\r\n\r\n", 8080),
            Route::Forbidden
        );
    }

    #[test]
    fn the_host_header_is_matched_case_insensitively() {
        let head = "GET / HTTP/1.1\r\nHOST: localhost:8080\r\n\r\n";
        assert_eq!(route(head, 8080), Route::Index);
    }

    #[test]
    fn every_route_but_the_inventory_answers_itself() {
        for route in [
            Route::Index,
            Route::Style,
            Route::Script,
            Route::NotFound,
            Route::BadRequest,
            Route::Forbidden,
        ] {
            assert!(canned(route).is_some(), "{route:?} has no canned response");
        }
        assert!(canned(Route::Inventory).is_none());
    }

    #[test]
    fn a_response_states_its_length_and_closes() {
        let bytes = response("200 OK", "text/plain; charset=utf-8", b"hello");
        let text = String::from_utf8_lossy(&bytes).into_owned();
        assert!(text.starts_with("HTTP/1.1 200 OK\r\n"));
        assert!(text.contains("Content-Length: 5\r\n"));
        assert!(text.contains("Connection: close\r\n"));
        assert!(text.ends_with("\r\n\r\nhello"));
    }

    #[test]
    fn the_printed_address_is_loopback() {
        assert_eq!(address(8080), "http://127.0.0.1:8080/");
    }
}
