//! Middlewares
use futures::Future;

use error::{Error, Result};
use httprequest::HttpRequest;
use httpresponse::HttpResponse;

mod logger;

#[cfg(feature = "session")]
mod session;
mod defaultheaders;
mod errhandlers;
pub mod cors;
pub mod csrf;
pub use self::logger::Logger;
pub use self::errhandlers::ErrorHandlers;
pub use self::defaultheaders::DefaultHeaders;

#[cfg(feature = "session")]
pub use self::session::{RequestSession, Session, SessionImpl, SessionBackend, SessionStorage,
                        CookieSessionError, CookieSessionBackend};

/// Middleware start result
pub enum Started {
    /// Execution completed
    Done,
    /// New http response got generated. If middleware generates response
    /// handler execution halts.
    Response(HttpResponse),
    /// Execution completed, runs future to completion.
    Future(Box<Future<Item=Option<HttpResponse>, Error=Error>>),
}

/// Middleware execution result
pub enum Response {
    /// New http response got generated
    Done(HttpResponse),
    /// Result is a future that resolves to a new http response
    Future(Box<Future<Item=HttpResponse, Error=Error>>),
}

/// Middleware finish result
pub enum Finished {
    /// Execution completed
    Done,
    /// Execution completed, but run future to completion
    Future(Box<Future<Item=(), Error=Error>>),
}

/// Middleware definition
#[allow(unused_variables)]
pub trait Middleware<S>: 'static {

    /// Method is called when request is ready. It may return
    /// future, which should resolve before next middleware get called.
    fn start(&self, req: &mut HttpRequest<S>) -> Result<Started> {
        Ok(Started::Done)
    }

    /// Method is called when handler returns response,
    /// but before sending http message to peer.
    fn response(&self, req: &mut HttpRequest<S>, resp: HttpResponse) -> Result<Response> {
        Ok(Response::Done(resp))
    }

    /// Method is called after body stream get sent to peer.
    fn finish(&self, req: &mut HttpRequest<S>, resp: &HttpResponse) -> Finished {
        Finished::Done
    }
}
