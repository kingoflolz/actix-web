# Testing

Every application should be well tested. Actix provides tools to perform unit and
integration tests.

## Unit tests

For unit testing actix provides a request builder type and simple handler runner.
[*TestRequest*](../actix_web/test/struct.TestRequest.html) implements a builder-like pattern.
You can generate a `HttpRequest` instance with `finish()` or you can
run your handler with `run()` or `run_async()`.

```rust
# extern crate http;
# extern crate actix_web;
use http::{header, StatusCode};
use actix_web::*;
use actix_web::test::TestRequest;

fn index(req: HttpRequest) -> HttpResponse {
     if let Some(hdr) = req.headers().get(header::CONTENT_TYPE) {
        if let Ok(s) = hdr.to_str() {
            return httpcodes::HttpOk.into()
        }
     }
     httpcodes::HttpBadRequest.into()
}

fn main() {
    let resp = TestRequest::with_header("content-type", "text/plain")
        .run(index)
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);

    let resp = TestRequest::default()
        .run(index)
        .unwrap();
    assert_eq!(resp.status(), StatusCode::BAD_REQUEST);
}
```


## Integration tests

There are several methods how you can test your application. Actix provides
[*TestServer*](../actix_web/test/struct.TestServer.html)
server that can be used to run the whole application of just specific handlers
in real http server. *TestServer::get()*, *TestServer::post()* or *TestServer::client()*
methods can be used to send requests to the test server.

In simple form *TestServer* can be configured to use handler. *TestServer::new* method
accepts configuration function, only argument for this function is *test application*
instance. You can check the [api documentation](../actix_web/test/struct.TestApp.html)
for more information.

```rust
# extern crate actix_web;
use actix_web::*;
use actix_web::test::TestServer;

fn index(req: HttpRequest) -> HttpResponse {
     httpcodes::HttpOk.into()
}

fn main() {
    let mut srv = TestServer::new(|app| app.handler(index));  // <- Start new test server

    let request = srv.get().finish().unwrap();                // <- create client request
    let response = srv.execute(request.send()).unwrap();      // <- send request to the server
    assert!(response.status().is_success());                  // <- check response

    let bytes = srv.execute(response.body()).unwrap();        // <- read response body
}
```

The other option is to use an application factory. In this case you need to pass the factory
function same way as you would for real http server configuration.

```rust
# extern crate http;
# extern crate actix_web;
use http::Method;
use actix_web::*;
use actix_web::test::TestServer;

fn index(req: HttpRequest) -> HttpResponse {
     httpcodes::HttpOk.into()
}

/// This function get called by http server.
fn create_app() -> Application {
    Application::new()
        .resource("/test", |r| r.h(index))
}

fn main() {
    let mut srv = TestServer::with_factory(create_app);         // <- Start new test server

    let request = srv.client(Method::GET, "/test").finish().unwrap(); // <- create client request
    let response = srv.execute(request.send()).unwrap();        // <- send request to the server

    assert!(response.status().is_success());                    // <- check response
}
```

## WebSocket server tests

It is possible to register a *handler* with `TestApp::handler()` that
initiates a web socket connection. *TestServer* provides `ws()` which connects to
the websocket server and returns ws reader and writer objects. *TestServer* also
provides an `execute()` method which runs future objects to completion and returns
result of the future computation.

Here is a simple example that shows how to test server websocket handler.

```rust
# extern crate actix;
# extern crate actix_web;
# extern crate futures;
# extern crate http;
# extern crate bytes;

use actix_web::*;
use futures::Stream;
# use actix::prelude::*;

struct Ws;   // <- WebSocket actor

impl Actor for Ws {
    type Context = ws::WebsocketContext<Self>;
}

impl StreamHandler<ws::Message, ws::ProtocolError> for Ws {

    fn handle(&mut self, msg: ws::Message, ctx: &mut Self::Context) {
        match msg {
            ws::Message::Text(text) => ctx.text(text),
            _ => (),
        }
    }
}

fn main() {
    let mut srv = test::TestServer::new(             // <- start our server with ws handler
        |app| app.handler(|req| ws::start(req, Ws)));

    let (reader, mut writer) = srv.ws().unwrap();    // <- connect to ws server

    writer.text("text");                             // <- send message to server

    let (item, reader) = srv.execute(reader.into_future()).unwrap();  // <- wait for one message
    assert_eq!(item, Some(ws::Message::Text("text".to_owned())));
}
```
