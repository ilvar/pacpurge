//! Tests for the `--web` server against a real socket.
//!
//! The routing decisions are unit-tested in `web.rs` without a port. What
//! these add is the part no pure test can reach: that a listener binds, that a
//! browser's bytes arrive and parse as a request, and that what goes back is a
//! response a client can read to the end. The stand-in for the scan is a fixed
//! document — what the analysis produces is `end_to_end`'s job.

use std::io::{Read, Write};
use std::net::TcpStream;
use std::thread;

use pacpurge::capability;
use pacpurge::web;

/// The document the stub server answers `/api/inventory` with.
const STUB: &str = r#"{"summary":{"packages":1},"inventory":{"entries":[]}}"#;

/// Serve `count` requests on a free port, and return the port.
fn serve(count: usize) -> u16 {
    let listener = capability::listen(0).expect("the listener should bind on loopback");
    let port = listener.port().expect("the bound port should be readable");

    thread::spawn(move || {
        let respond = |head: &str| -> Vec<u8> {
            let route = web::route(head, port);
            match web::canned(route) {
                Some(canned) => {
                    web::response(canned.status, canned.content_type, canned.body.as_bytes())
                }
                None => web::response("200 OK", "application/json; charset=utf-8", STUB.as_bytes()),
            }
        };

        for _request in 0..count {
            capability::serve_next(&listener, &respond, web::MAX_HEAD)
                .expect("the server should answer the request");
        }
    });

    port
}

/// Send one raw request and read the whole reply.
fn request(port: u16, raw: &str) -> String {
    let mut stream =
        TcpStream::connect(("127.0.0.1", port)).expect("the server should accept a connection");
    stream
        .write_all(raw.as_bytes())
        .expect("the request should send");
    stream.flush().expect("the request should flush");

    let mut reply = String::new();
    stream
        .read_to_string(&mut reply)
        .expect("the reply should read to the end");
    reply
}

/// A well-formed GET for `path`, addressed to this server.
fn get(port: u16, path: &str) -> String {
    format!("GET {path} HTTP/1.1\r\nHost: 127.0.0.1:{port}\r\nAccept: */*\r\n\r\n")
}

#[test]
fn the_page_and_its_assets_are_served() {
    let port = serve(3);

    let page = request(port, &get(port, "/"));
    assert!(page.starts_with("HTTP/1.1 200 OK\r\n"), "got: {page}");
    assert!(page.contains("text/html"), "got: {page}");
    assert!(page.contains("<title>pacpurge</title>"), "got: {page}");

    let script = request(port, &get(port, "/app.js"));
    assert!(script.contains("text/javascript"), "got: {script}");
    assert!(script.contains("/api/inventory"), "got: {script}");

    let style = request(port, &get(port, "/style.css"));
    assert!(style.contains("text/css"), "got: {style}");
}

#[test]
fn the_inventory_endpoint_answers_with_the_document() {
    let port = serve(1);

    let reply = request(port, &get(port, "/api/inventory"));
    assert!(reply.contains("application/json"), "got: {reply}");
    assert!(reply.ends_with(STUB), "got: {reply}");
}

#[test]
fn a_request_from_another_name_is_refused() {
    let port = serve(1);

    // A page on the internet pointing a script at a hostname that resolves to
    // 127.0.0.1 reaches this socket. The Host header is what gives it away.
    let rebound = format!("GET /api/inventory HTTP/1.1\r\nHost: evil.example.com:{port}\r\n\r\n");
    let reply = request(port, &rebound);
    assert!(
        reply.starts_with("HTTP/1.1 403 Forbidden\r\n"),
        "got: {reply}"
    );
    assert!(!reply.contains("packages"), "the body leaked: {reply}");
}

#[test]
fn nothing_that_could_change_the_system_is_answered() {
    let port = serve(2);

    for verb in ["POST", "DELETE"] {
        let reply = request(
            port,
            &format!("{verb} /api/inventory HTTP/1.1\r\nHost: 127.0.0.1:{port}\r\n\r\n"),
        );
        assert!(
            reply.starts_with("HTTP/1.1 400 Bad Request\r\n"),
            "{verb} got: {reply}"
        );
    }
}

#[test]
fn an_unknown_path_is_a_clean_404() {
    let port = serve(1);

    let reply = request(port, &get(port, "/../../../etc/shadow"));
    assert!(
        reply.starts_with("HTTP/1.1 404 Not Found\r\n"),
        "got: {reply}"
    );
}
