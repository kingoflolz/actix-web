extern crate actix;
extern crate actix_web;
extern crate futures;
extern crate http;
extern crate bytes;
extern crate rand;

use bytes::Bytes;
use futures::Stream;
use rand::Rng;

#[cfg(feature="alpn")]
extern crate openssl;

use actix_web::*;
use actix::prelude::*;

struct Ws;

impl Actor for Ws {
    type Context = ws::WebsocketContext<Self>;
}

impl StreamHandler<ws::Message, ws::ProtocolError> for Ws {

    fn handle(&mut self, msg: ws::Message, ctx: &mut Self::Context) {
        match msg {
            ws::Message::Ping(msg) => ctx.pong(&msg),
            ws::Message::Text(text) => ctx.text(text),
            ws::Message::Binary(bin) => ctx.binary(bin),
            ws::Message::Close(reason) => ctx.close(reason, ""),
            _ => (),
        }
    }
}

#[test]
fn test_simple() {
    let mut srv = test::TestServer::new(
        |app| app.handler(|req| ws::start(req, Ws)));
    let (reader, mut writer) = srv.ws().unwrap();

    writer.text("text");
    let (item, reader) = srv.execute(reader.into_future()).unwrap();
    assert_eq!(item, Some(ws::Message::Text("text".to_owned())));

    writer.binary(b"text".as_ref());
    let (item, reader) = srv.execute(reader.into_future()).unwrap();
    assert_eq!(item, Some(ws::Message::Binary(Bytes::from_static(b"text").into())));

    writer.ping("ping");
    let (item, reader) = srv.execute(reader.into_future()).unwrap();
    assert_eq!(item, Some(ws::Message::Pong("ping".to_owned())));

    writer.close(ws::CloseCode::Normal, "");
    let (item, _) = srv.execute(reader.into_future()).unwrap();
    assert_eq!(item, Some(ws::Message::Close(ws::CloseCode::Normal)));
}

#[test]
fn test_large_text() {
    let data = rand::thread_rng()
        .gen_ascii_chars()
        .take(65_536)
        .collect::<String>();

    let mut srv = test::TestServer::new(
        |app| app.handler(|req| ws::start(req, Ws)));
    let (mut reader, mut writer) = srv.ws().unwrap();

    for _ in 0..100 {
        writer.text(data.clone());
        let (item, r) = srv.execute(reader.into_future()).unwrap();
        reader = r;
        assert_eq!(item, Some(ws::Message::Text(data.clone())));
    }
}

#[test]
fn test_large_bin() {
    let data = rand::thread_rng()
        .gen_ascii_chars()
        .take(65_536)
        .collect::<String>();

    let mut srv = test::TestServer::new(
        |app| app.handler(|req| ws::start(req, Ws)));
    let (mut reader, mut writer) = srv.ws().unwrap();

    for _ in 0..100 {
        writer.binary(data.clone());
        let (item, r) = srv.execute(reader.into_future()).unwrap();
        reader = r;
        assert_eq!(item, Some(ws::Message::Binary(Binary::from(data.clone()))));
    }
}

struct Ws2 {
    count: usize,
    bin: bool,
}

impl Actor for Ws2 {
    type Context = ws::WebsocketContext<Self>;

    fn started(&mut self, ctx: &mut Self::Context) {
        self.send(ctx);
    }
}

impl Ws2 {
    fn send(&mut self, ctx: &mut ws::WebsocketContext<Self>) {
        if self.bin {
            ctx.binary(Vec::from("0".repeat(65_536)));
        } else {
            ctx.text("0".repeat(65_536));
        }
        ctx.drain().and_then(|_, act, ctx| {
            act.count += 1;
            if act.count != 10_000 {
                act.send(ctx);
            }
            actix::fut::ok(())
        }).wait(ctx);
    }
}

impl StreamHandler<ws::Message, ws::ProtocolError> for Ws2 {

    fn handle(&mut self, msg: ws::Message, ctx: &mut Self::Context) {
        match msg {
            ws::Message::Ping(msg) => ctx.pong(&msg),
            ws::Message::Text(text) => ctx.text(text),
            ws::Message::Binary(bin) => ctx.binary(bin),
            ws::Message::Close(reason) => ctx.close(reason, ""),
            _ => (),
        }
    }
}

#[test]
fn test_server_send_text() {
    let data = Some(ws::Message::Text("0".repeat(65_536)));

    let mut srv = test::TestServer::new(
        |app| app.handler(|req| ws::start(req, Ws2{count:0, bin: false})));
    let (mut reader, _writer) = srv.ws().unwrap();

    for _ in 0..10_000 {
        let (item, r) = srv.execute(reader.into_future()).unwrap();
        reader = r;
        assert_eq!(item, data);
    }
}

#[test]
fn test_server_send_bin() {
    let data = Some(ws::Message::Binary(Binary::from("0".repeat(65_536))));

    let mut srv = test::TestServer::new(
        |app| app.handler(|req| ws::start(req, Ws2{count:0, bin: true})));
    let (mut reader, _writer) = srv.ws().unwrap();

    for _ in 0..10_000 {
        let (item, r) = srv.execute(reader.into_future()).unwrap();
        reader = r;
        assert_eq!(item, data);
    }
}

#[test]
#[cfg(feature="alpn")]
fn test_ws_server_ssl() {
    extern crate openssl;
    use openssl::ssl::{SslMethod, SslAcceptor, SslFiletype};

    // load ssl keys
    let mut builder = SslAcceptor::mozilla_intermediate(SslMethod::tls()).unwrap();
    builder.set_private_key_file("tests/key.pem", SslFiletype::PEM).unwrap();
    builder.set_certificate_chain_file("tests/cert.pem").unwrap();

    let mut srv = test::TestServer::build()
        .ssl(builder.build())
        .start(|app| app.handler(|req| ws::start(req, Ws2{count:0, bin: false})));
    let (mut reader, _writer) = srv.ws().unwrap();

    let data = Some(ws::Message::Text("0".repeat(65_536)));
    for _ in 0..10_000 {
        let (item, r) = srv.execute(reader.into_future()).unwrap();
        reader = r;
        assert_eq!(item, data);
    }
}
