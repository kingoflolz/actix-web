extern crate actix;
extern crate actix_web;
extern crate bytes;
extern crate futures;
extern crate h2;
extern crate http;
extern crate tokio_core;
#[macro_use]
extern crate serde_derive;

use actix_web::*;
use bytes::Bytes;
use http::StatusCode;

#[derive(Deserialize)]
struct PParam {
    username: String,
}

#[test]
fn test_path_extractor() {
    let mut srv = test::TestServer::new(|app| {
        app.resource("/{username}/index.html", |r| {
            r.with(|p: Path<PParam>| format!("Welcome {}!", p.username))
        });
    });

    // client request
    let request = srv.get()
        .uri(srv.url("/test/index.html"))
        .finish()
        .unwrap();
    let response = srv.execute(request.send()).unwrap();
    assert!(response.status().is_success());

    // read response
    let bytes = srv.execute(response.body()).unwrap();
    assert_eq!(bytes, Bytes::from_static(b"Welcome test!"));
}

#[test]
fn test_query_extractor() {
    let mut srv = test::TestServer::new(|app| {
        app.resource("/index.html", |r| {
            r.with(|p: Query<PParam>| format!("Welcome {}!", p.username))
        });
    });

    // client request
    let request = srv.get()
        .uri(srv.url("/index.html?username=test"))
        .finish()
        .unwrap();
    let response = srv.execute(request.send()).unwrap();
    assert!(response.status().is_success());

    // read response
    let bytes = srv.execute(response.body()).unwrap();
    assert_eq!(bytes, Bytes::from_static(b"Welcome test!"));

    // client request
    let request = srv.get()
        .uri(srv.url("/index.html"))
        .finish()
        .unwrap();
    let response = srv.execute(request.send()).unwrap();
    assert_eq!(response.status(), StatusCode::BAD_REQUEST);
}

#[test]
fn test_path_and_query_extractor() {
    let mut srv = test::TestServer::new(|app| {
        app.resource("/{username}/index.html", |r| {
            r.route().with2(|p: Path<PParam>, q: Query<PParam>| {
                format!("Welcome {} - {}!", p.username, q.username)
            })
        });
    });

    // client request
    let request = srv.get()
        .uri(srv.url("/test1/index.html?username=test2"))
        .finish()
        .unwrap();
    let response = srv.execute(request.send()).unwrap();
    assert!(response.status().is_success());

    // read response
    let bytes = srv.execute(response.body()).unwrap();
    assert_eq!(bytes, Bytes::from_static(b"Welcome test1 - test2!"));

    // client request
    let request = srv.get()
        .uri(srv.url("/test1/index.html"))
        .finish()
        .unwrap();
    let response = srv.execute(request.send()).unwrap();
    assert_eq!(response.status(), StatusCode::BAD_REQUEST);
}

#[test]
fn test_path_and_query_extractor2() {
    let mut srv = test::TestServer::new(|app| {
        app.resource("/{username}/index.html", |r| {
            r.route()
                .with3(|_: HttpRequest, p: Path<PParam>, q: Query<PParam>| {
                    format!("Welcome {} - {}!", p.username, q.username)
                })
        });
    });

    // client request
    let request = srv.get()
        .uri(srv.url("/test1/index.html?username=test2"))
        .finish()
        .unwrap();
    let response = srv.execute(request.send()).unwrap();
    assert!(response.status().is_success());

    // read response
    let bytes = srv.execute(response.body()).unwrap();
    assert_eq!(bytes, Bytes::from_static(b"Welcome test1 - test2!"));

    // client request
    let request = srv.get()
        .uri(srv.url("/test1/index.html"))
        .finish()
        .unwrap();
    let response = srv.execute(request.send()).unwrap();
    assert_eq!(response.status(), StatusCode::BAD_REQUEST);
}

#[test]
fn test_non_ascii_route() {
    let mut srv = test::TestServer::new(|app| {
        app.resource("/中文/index.html", |r| r.f(|_| "success"));
    });

    // client request
    let request = srv.get()
        .uri(srv.url("/中文/index.html"))
        .finish()
        .unwrap();
    let response = srv.execute(request.send()).unwrap();
    assert!(response.status().is_success());

    // read response
    let bytes = srv.execute(response.body()).unwrap();
    assert_eq!(bytes, Bytes::from_static(b"success"));
}
