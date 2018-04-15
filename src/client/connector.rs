use std::{fmt, mem, io, time};
use std::cell::{Cell, RefCell};
use std::rc::Rc;
use std::net::Shutdown;
use std::time::{Duration, Instant};
use std::collections::{HashMap, VecDeque};

use actix::{fut, Actor, ActorFuture, Arbiter, Context, AsyncContext,
            Handler, Message, ActorResponse, Supervised, ContextFutureSpawner};
use actix::registry::ArbiterService;
use actix::fut::WrapFuture;
use actix::actors::{Connector, ConnectorError, Connect as ResolveConnect};

use http::{Uri, HttpTryFrom, Error as HttpError};
use futures::{Async, Future, Poll};
use futures::task::{Task, current as current_task};
use futures::unsync::oneshot;
use tokio_io::{AsyncRead, AsyncWrite};
use tokio_core::reactor::Timeout;

#[cfg(feature="alpn")]
use openssl::ssl::{SslMethod, SslConnector, Error as OpensslError};
#[cfg(feature="alpn")]
use tokio_openssl::SslConnectorExt;

#[cfg(all(feature="tls", not(feature="alpn")))]
use native_tls::{TlsConnector, Error as TlsError};
#[cfg(all(feature="tls", not(feature="alpn")))]
use tokio_tls::TlsConnectorExt;

use {HAS_OPENSSL, HAS_TLS};
use server::IoStream;


#[derive(Debug)]
/// `Connect` type represents message that can be send to `ClientConnector`
/// with connection request.
pub struct Connect {
    pub(crate) uri: Uri,
    pub(crate) wait_time: Duration,
    pub(crate) conn_timeout: Duration,
}

impl Connect {
    /// Create `Connect` message for specified `Uri`
    pub fn new<U>(uri: U) -> Result<Connect, HttpError> where Uri: HttpTryFrom<U> {
        Ok(Connect {
            uri: Uri::try_from(uri).map_err(|e| e.into())?,
            wait_time: Duration::from_secs(5),
            conn_timeout: Duration::from_secs(1),
        })
    }

    /// Connection timeout, max time to connect to remote host.
    /// By default connect timeout is 1 seccond.
    pub fn conn_timeout(mut self, timeout: Duration) -> Self {
        self.conn_timeout = timeout;
        self
    }

    /// If connection pool limits are enabled, wait time indicates
    /// max time to wait for available connection.
    /// By default connect timeout is 5 secconds.
    pub fn wait_time(mut self, timeout: Duration) -> Self {
        self.wait_time = timeout;
        self
    }
}

impl Message for Connect {
    type Result = Result<Connection, ClientConnectorError>;
}

/// A set of errors that can occur during connecting to a http host
#[derive(Fail, Debug)]
pub enum ClientConnectorError {
    /// Invalid url
    #[fail(display="Invalid url")]
    InvalidUrl,

    /// SSL feature is not enabled
    #[fail(display="SSL is not supported")]
    SslIsNotSupported,

    /// SSL error
    #[cfg(feature="alpn")]
    #[fail(display="{}", _0)]
    SslError(#[cause] OpensslError),

    /// SSL error
    #[cfg(all(feature="tls", not(feature="alpn")))]
    #[fail(display="{}", _0)]
    SslError(#[cause] TlsError),

    /// Connection error
    #[fail(display = "{}", _0)]
    Connector(#[cause] ConnectorError),

    /// Connection took too long
    #[fail(display = "Timeout out while establishing connection")]
    Timeout,

    /// Connector has been disconnected
    #[fail(display = "Internal error: connector has been disconnected")]
    Disconnected,

    /// Connection io error
    #[fail(display = "{}", _0)]
    IoError(#[cause] io::Error),
}

impl From<ConnectorError> for ClientConnectorError {
    fn from(err: ConnectorError) -> ClientConnectorError {
        match err {
            ConnectorError::Timeout => ClientConnectorError::Timeout,
            _ => ClientConnectorError::Connector(err)
        }
    }
}

struct Waiter {
    tx: oneshot::Sender<Result<Connection, ClientConnectorError>>,
    wait: Instant,
    conn_timeout: Duration,
}

/// `ClientConnector` type is responsible for transport layer of a client connection
/// of a client connection.
pub struct ClientConnector {
    #[cfg(all(feature="alpn"))]
    connector: SslConnector,
    #[cfg(all(feature="tls", not(feature="alpn")))]
    connector: TlsConnector,

    pool: Rc<Pool>,
    pool_modified: Rc<Cell<bool>>,

    conn_lifetime: Duration,
    conn_keep_alive: Duration,
    limit: usize,
    limit_per_host: usize,
    acquired: usize,
    acquired_per_host: HashMap<Key, usize>,
    available: HashMap<Key, VecDeque<Conn>>,
    to_close: Vec<Connection>,
    waiters: HashMap<Key, VecDeque<Waiter>>,
    wait_timeout: Option<(Instant, Timeout)>,
}

impl Actor for ClientConnector {
    type Context = Context<ClientConnector>;

    fn started(&mut self, ctx: &mut Self::Context) {
        self.collect_periodic(ctx);
        ctx.spawn(Maintenance);
    }
}

impl Supervised for ClientConnector {}

impl ArbiterService for ClientConnector {}

impl Default for ClientConnector {
    fn default() -> ClientConnector {
        let modified = Rc::new(Cell::new(false));

        #[cfg(all(feature="alpn"))]
        {
            let builder = SslConnector::builder(SslMethod::tls()).unwrap();
            ClientConnector::with_connector(builder.build())
        }
        #[cfg(all(feature="tls", not(feature="alpn")))]
        {
            let builder = TlsConnector::builder().unwrap();
            ClientConnector {
                pool: Rc::new(Pool::new(Rc::clone(&modified))),
                pool_modified: modified,
                connector: builder.build().unwrap(),
                conn_lifetime: Duration::from_secs(15),
                conn_keep_alive: Duration::from_secs(75),
                limit: 100,
                limit_per_host: 0,
                acquired: 0,
                acquired_per_host: HashMap::new(),
                available: HashMap::new(),
                to_close: Vec::new(),
                waiters: HashMap::new(),
                wait_timeout: None,
            }
        }

        #[cfg(not(any(feature="alpn", feature="tls")))]
        ClientConnector {pool: Rc::new(Pool::new(Rc::clone(&modified))),
                         pool_modified: modified,
                         conn_lifetime: Duration::from_secs(15),
                         conn_keep_alive: Duration::from_secs(75),
                         limit: 100,
                         limit_per_host: 0,
                         acquired: 0,
                         acquired_per_host: HashMap::new(),
                         available: HashMap::new(),
                         to_close: Vec::new(),
                         waiters: HashMap::new(),
                         wait_timeout: None,
        }
    }
}

impl ClientConnector {

    #[cfg(feature="alpn")]
    /// Create `ClientConnector` actor with custom `SslConnector` instance.
    ///
    /// By default `ClientConnector` uses very simple ssl configuration.
    /// With `with_connector` method it is possible to use custom `SslConnector`
    /// object.
    ///
    /// ```rust
    /// # #![cfg(feature="alpn")]
    /// # extern crate actix;
    /// # extern crate actix_web;
    /// # extern crate futures;
    /// # use futures::Future;
    /// # use std::io::Write;
    /// extern crate openssl;
    /// use actix::prelude::*;
    /// use actix_web::client::{Connect, ClientConnector};
    ///
    /// use openssl::ssl::{SslMethod, SslConnector};
    ///
    /// fn main() {
    ///     let sys = System::new("test");
    ///
    ///     // Start `ClientConnector` with custom `SslConnector`
    ///     let ssl_conn = SslConnector::builder(SslMethod::tls()).unwrap().build();
    ///     let conn: Address<_> = ClientConnector::with_connector(ssl_conn).start();
    ///
    ///     Arbiter::handle().spawn({
    ///         conn.send(
    ///             Connect::new("https://www.rust-lang.org").unwrap()) // <- connect to host
    ///                 .map_err(|_| ())
    ///                 .and_then(|res| {
    ///                     if let Ok(mut stream) = res {
    ///                         stream.write_all(b"GET / HTTP/1.0\r\n\r\n").unwrap();
    ///                     }
    /// #                   Arbiter::system().do_send(actix::msgs::SystemExit(0));
    ///                     Ok(())
    ///                 })
    ///     });
    ///
    ///     sys.run();
    /// }
    /// ```
    pub fn with_connector(connector: SslConnector) -> ClientConnector {
        let modified = Rc::new(Cell::new(false));
        ClientConnector {
            connector,
            pool: Rc::new(Pool::new(Rc::clone(&modified))),
            pool_modified: modified,
            conn_lifetime: Duration::from_secs(15),
            conn_keep_alive: Duration::from_secs(75),
            limit: 100,
            limit_per_host: 0,
            acquired: 0,
            acquired_per_host: HashMap::new(),
            available: HashMap::new(),
            to_close: Vec::new(),
            waiters: HashMap::new(),
            wait_timeout: None,
        }
    }

    /// Set total number of simultaneous connections.
    ///
    /// If limit is 0, the connector has no limit.
    /// The default limit size is 100.
    pub fn limit(mut self, limit: usize) -> Self {
        self.limit = limit;
        self
    }

    /// Set total number of simultaneous connections to the same endpoint.
    ///
    /// Endpoints are the same if they are have equal (host, port, ssl) triplet.
    /// If limit is 0, the connector has no limit. The default limit size is 0.
    pub fn limit_per_host(mut self, limit: usize) -> Self {
        self.limit_per_host = limit;
        self
    }

    /// Set keep-alive period for opened connection.
    ///
    /// Keep-alive period is period between connection usage.
    /// if deley between connection usage exceeds this period
    /// connection closes.
    pub fn conn_keep_alive(mut self, dur: Duration) -> Self {
        self.conn_keep_alive = dur;
        self
    }

    /// Set max lifetime period for connection.
    ///
    /// Connection lifetime is max lifetime of any opened connection
    /// until it get closed regardless of keep-alive period.
    pub fn conn_lifetime(mut self, dur: Duration) -> Self {
        self.conn_lifetime = dur;
        self
    }

    fn acquire(&mut self, key: &Key) -> Acquire {
        // check limits
        if self.limit > 0 {
            if self.acquired >= self.limit {
                return Acquire::NotAvailable
            }
            if self.limit_per_host > 0 {
                if let Some(per_host) = self.acquired_per_host.get(key) {
                    if self.limit_per_host >= *per_host {
                        return Acquire::NotAvailable
                    }
                }
            }
        }
        else if self.limit_per_host > 0 {
            if let Some(per_host) = self.acquired_per_host.get(key) {
                if self.limit_per_host >= *per_host {
                    return Acquire::NotAvailable
                }
            }
        }

        self.reserve(key);

        // check if open connection is available
        // cleanup stale connections at the same time
        if let Some(ref mut connections) = self.available.get_mut(key) {
            let now = Instant::now();
            while let Some(conn) = connections.pop_back() {
                // check if it still usable
                if (now - conn.0) > self.conn_keep_alive
                    || (now - conn.1.ts) > self.conn_lifetime
                {
                    self.to_close.push(conn.1);
                } else {
                    let mut conn = conn.1;
                    let mut buf = [0; 2];
                    match conn.stream().read(&mut buf) {
                        Err(ref e) if e.kind() == io::ErrorKind::WouldBlock => (),
                        Ok(n) if n > 0 => {
                            self.to_close.push(conn);
                            continue
                        },
                        Ok(_) | Err(_) => continue,
                    }
                    return Acquire::Acquired(conn)
                }
            }
        }
        Acquire::Available
    }

    fn reserve(&mut self, key: &Key) {
        self.acquired += 1;
        let per_host =
            if let Some(per_host) = self.acquired_per_host.get(key) {
                *per_host
            } else {
                0
            };
        self.acquired_per_host.insert(key.clone(), per_host + 1);
    }

    fn release_key(&mut self, key: &Key) {
        self.acquired -= 1;
        let per_host =
            if let Some(per_host) = self.acquired_per_host.get(key) {
                *per_host
            } else {
                return
            };
        if per_host > 1 {
            self.acquired_per_host.insert(key.clone(), per_host - 1);
        } else {
            self.acquired_per_host.remove(key);
        }
    }

    fn collect(&mut self, periodic: bool) {
        let now = Instant::now();

        if self.pool_modified.get() {
            // collect half acquire keys
            if let Some(keys) = self.pool.collect_keys() {
                for key in keys {
                    self.release_key(&key);
                }
            }

            // collect connections for close
            if let Some(to_close) = self.pool.collect_close() {
                for conn in to_close {
                    self.release_key(&conn.key);
                    self.to_close.push(conn);
                }
            }

            // connection connections
            if let Some(to_release) = self.pool.collect_release() {
                for conn in to_release {
                    self.release_key(&conn.key);

                    // check connection lifetime and the return to available pool
                    if (now - conn.ts) < self.conn_lifetime {
                        self.available.entry(conn.key.clone())
                            .or_insert_with(VecDeque::new)
                            .push_back(Conn(Instant::now(), conn));
                    }
                }
            }
        }

        // check keep-alive
        for conns in self.available.values_mut() {
            while !conns.is_empty() {
                if (now > conns[0].0) && (now - conns[0].0) > self.conn_keep_alive
                    || (now - conns[0].1.ts) > self.conn_lifetime
                {
                    let conn = conns.pop_front().unwrap().1;
                    self.to_close.push(conn);
                } else {
                    break
                }
            }
        }

        // check connections for shutdown
        if periodic {
            let mut idx = 0;
            while idx < self.to_close.len() {
                match AsyncWrite::shutdown(&mut self.to_close[idx]) {
                    Ok(Async::NotReady) => idx += 1,
                    _ => {
                        self.to_close.swap_remove(idx);
                    },
                }
            }
        }

        self.pool_modified.set(false);
    }

    fn collect_periodic(&mut self, ctx: &mut Context<Self>) {
        self.collect(true);
        // re-schedule next collect period
        ctx.run_later(Duration::from_secs(1), |act, ctx| act.collect_periodic(ctx));
    }

    fn collect_waiters(&mut self) {
        let now = Instant::now();
        let mut next = None;

        for (_, waiters) in &mut self.waiters {
            let mut idx = 0;
            while idx < waiters.len() {
                if waiters[idx].wait <= now {
                    let waiter = waiters.swap_remove_back(idx).unwrap();
                    let _ = waiter.tx.send(Err(ClientConnectorError::Timeout));
                } else {
                    if let Some(n) = next {
                        if waiters[idx].wait < n {
                            next = Some(waiters[idx].wait);
                        }
                    } else {
                        next = Some(waiters[idx].wait);
                    }
                    idx += 1;
                }
            }
        }

        if next.is_some() {
            self.install_wait_timeout(next.unwrap());
        }
    }
    
    fn install_wait_timeout(&mut self, time: Instant) {
        if let Some(ref mut wait) = self.wait_timeout {
            if wait.0 < time {
                return
            }
        }

        let mut timeout = Timeout::new(time-Instant::now(), Arbiter::handle()).unwrap();
        let _ = timeout.poll();
        self.wait_timeout = Some((time, timeout));
    }
}

impl Handler<Connect> for ClientConnector {
    type Result = ActorResponse<ClientConnector, Connection, ClientConnectorError>;

    fn handle(&mut self, msg: Connect, _: &mut Self::Context) -> Self::Result {
        if self.pool_modified.get() {
            self.collect(false);
        }

        let uri = &msg.uri;
        let wait_time = msg.wait_time;
        let conn_timeout = msg.conn_timeout;

        // host name is required
        if uri.host().is_none() {
            return ActorResponse::reply(Err(ClientConnectorError::InvalidUrl))
        }

        // supported protocols
        let proto = match uri.scheme_part() {
            Some(scheme) => match Protocol::from(scheme.as_str()) {
                Some(proto) => proto,
                None => return ActorResponse::reply(Err(ClientConnectorError::InvalidUrl)),
            },
            None => return ActorResponse::reply(Err(ClientConnectorError::InvalidUrl)),
        };

        // check ssl availability
        if proto.is_secure() && !HAS_OPENSSL && !HAS_TLS {
            return ActorResponse::reply(Err(ClientConnectorError::SslIsNotSupported))
        }

        // check if pool has task reference
        if self.pool.task.borrow().is_none() {
            *self.pool.task.borrow_mut() = Some(current_task());
        }
        
        let host = uri.host().unwrap().to_owned();
        let port = uri.port().unwrap_or_else(|| proto.port());
        let key = Key {host, port, ssl: proto.is_secure()};

        // acquire connection
        let pool = if proto.is_http() {
            match self.acquire(&key) {
                Acquire::Acquired(mut conn) => {
                    // use existing connection
                    conn.pool = Some(AcquiredConn(key, Some(Rc::clone(&self.pool))));
                    return ActorResponse::async(fut::ok(conn))
                },
                Acquire::NotAvailable => {
                    // connection is not available, wait
                    let (tx, rx) = oneshot::channel();

                    let wait = Instant::now() + wait_time;
                    self.install_wait_timeout(wait);

                    let waiter = Waiter{ tx, wait, conn_timeout };
                    self.waiters.entry(key.clone()).or_insert_with(VecDeque::new)
                        .push_back(waiter);
                    return ActorResponse::async(
                        rx.map_err(|_| ClientConnectorError::Disconnected)
                            .into_actor(self)
                            .and_then(|res, _, _| match res {
                                Ok(conn) => fut::ok(conn),
                                Err(err) => fut::err(err),
                            }));
                }
                Acquire::Available => {
                    Some(Rc::clone(&self.pool))
                },
            }
        } else {
            None
        };
        let conn = AcquiredConn(key, pool);

{
    ActorResponse::async(
        Connector::from_registry()
            .send(ResolveConnect::host_and_port(&conn.0.host, port)
                  .timeout(conn_timeout))
            .into_actor(self)
            .map_err(|_, _, _| ClientConnectorError::Disconnected)
            .and_then(move |res, _act, _| {
                #[cfg(feature="alpn")]
                match res {
                    Err(err) => fut::Either::B(fut::err(err.into())),
                    Ok(stream) => {
                        if proto.is_secure() {
                            fut::Either::A(
                                _act.connector.connect_async(&conn.0.host, stream)
                                    .map_err(ClientConnectorError::SslError)
                                    .map(|stream| Connection::new(
                                        conn.0.clone(), Some(conn), Box::new(stream)))
                                    .into_actor(_act))
                        } else {
                            fut::Either::B(fut::ok(
                                Connection::new(
                                    conn.0.clone(), Some(conn), Box::new(stream))))
                        }
                    }
                }

                #[cfg(all(feature="tls", not(feature="alpn")))]
                match res {
                    Err(err) => fut::Either::B(fut::err(err.into())),
                    Ok(stream) => {
                        if proto.is_secure() {
                            fut::Either::A(
                                _act.connector.connect_async(&conn.0.host, stream)
                                    .map_err(ClientConnectorError::SslError)
                                    .map(|stream| Connection::new(
                                        conn.0.clone(), Some(conn), Box::new(stream)))
                                    .into_actor(_act))
                        } else {
                            fut::Either::B(fut::ok(
                                Connection::new(
                                    conn.0.clone(), Some(conn), Box::new(stream))))
                        }
                    }
                }

                #[cfg(not(any(feature="alpn", feature="tls")))]
                match res {
                    Err(err) => fut::err(err.into()),
                    Ok(stream) => {
                        if proto.is_secure() {
                            fut::err(ClientConnectorError::SslIsNotSupported)
                        } else {
                            fut::ok(Connection::new(
                                conn.0.clone(), Some(conn), Box::new(stream)))
                        }
                    }
                }
            }))
}
    }
}

struct Maintenance;

impl fut::ActorFuture for Maintenance
{
    type Item = ();
    type Error = ();
    type Actor = ClientConnector;

    fn poll(&mut self, act: &mut ClientConnector, ctx: &mut Context<ClientConnector>)
            -> Poll<Self::Item, Self::Error>
    {
        // collect connections
        if act.pool_modified.get() {
            act.collect(false);
        }

        // collect wait timers
        act.collect_waiters();

        // check waiters
        let tmp: &mut ClientConnector = unsafe{mem::transmute(act as &mut _)};

        for (key, waiters) in &mut tmp.waiters {
            while let Some(waiter) = waiters.pop_front() {
                if waiter.tx.is_canceled() { continue }

                match act.acquire(key) {
                    Acquire::Acquired(mut conn) => {
                        // use existing connection
                        conn.pool = Some(
                            AcquiredConn(key.clone(), Some(Rc::clone(&act.pool))));
                        let _ = waiter.tx.send(Ok(conn));
                    },
                    Acquire::NotAvailable => {
                        waiters.push_front(waiter);
                        break
                    }
                    Acquire::Available =>
       {
           let conn = AcquiredConn(key.clone(), Some(Rc::clone(&act.pool)));

           fut::WrapFuture::<ClientConnector>::actfuture(
               Connector::from_registry()
                   .send(ResolveConnect::host_and_port(&conn.0.host, conn.0.port)
                         .timeout(waiter.conn_timeout)))
               .map_err(|_, _, _| ())
               .and_then(move |res, _act, _| {
                   #[cfg(feature="alpn")]
                   match res {
                       Err(err) => {
                           let _ = waiter.tx.send(Err(err.into()));
                           fut::Either::B(fut::err(()))
                       },
                       Ok(stream) => {
                           if conn.0.ssl {
                               fut::Either::A(
                                   _act.connector.connect_async(&key.host, stream)
                                       .then(move |res| {
                                           match res {
                                               Err(e) => {
                                                   let _ = waiter.tx.send(Err(
                                                       ClientConnectorError::SslError(e)));
                                               },
                                               Ok(stream) => {
                                                   let _ = waiter.tx.send(Ok(
                                                       Connection::new(
                                                           conn.0.clone(),
                                                           Some(conn), Box::new(stream))));
                                               }
                                           }
                                           Ok(())
                                       })
                                       .actfuture())
                           } else {
                               let _ = waiter.tx.send(Ok(Connection::new(
                                   conn.0.clone(), Some(conn), Box::new(stream))));
                               fut::Either::B(fut::ok(()))
                           }
                       }
                   }

                   #[cfg(all(feature="tls", not(feature="alpn")))]
                   match res {
                       Err(err) => {
                           let _ = waiter.tx.send(Err(err.into()));
                           fut::Either::B(fut::err(()))
                       },
                       Ok(stream) => {
                           if conn.0.ssl {
                               fut::Either::A(
                                   _act.connector.connect_async(&conn.0.host, stream)
                                       .then(|res| {
                                           match res {
                                               Err(e) => {
                                                   let _ = waiter.tx.send(Err(
                                                       ClientConnectorError::SslError(e)));
                                               },
                                               Ok(stream) => {
                                                   let _ = waiter.tx.send(Ok(
                                                       Connection::new(
                                                           conn.0.clone(), Some(conn),
                                                           Box::new(stream))));
                                               }
                                           }
                                           Ok(())
                                       })
                                       .into_actor(_act))
                           } else {
                               let _ = waiter.tx.send(Ok(Connection::new(
                                   conn.0.clone(), Some(conn), Box::new(stream))));
                               fut::Either::B(fut::ok(()))
                           }
                       }
                   }

                   #[cfg(not(any(feature="alpn", feature="tls")))]
                   match res {
                       Err(err) => {
                           let _ = waiter.tx.send(Err(err.into()));
                           fut::err(())
                       },
                       Ok(stream) => {
                           if conn.0.ssl {
                               let _ = waiter.tx.send(
                                   Err(ClientConnectorError::SslIsNotSupported));
                           } else {
                               let _ = waiter.tx.send(Ok(Connection::new(
                                   conn.0.clone(), Some(conn), Box::new(stream))));
                           };
                           fut::ok(())
                       },
                   }
               })
               .spawn(ctx);
       }
                }
            }
        }

        Ok(Async::NotReady)
    }
}

#[derive(PartialEq, Hash, Debug, Clone, Copy)]
enum Protocol {
    Http,
    Https,
    Ws,
    Wss,
}

impl Protocol {
    fn from(s: &str) -> Option<Protocol> {
        match s {
            "http" => Some(Protocol::Http),
            "https" => Some(Protocol::Https),
            "ws" => Some(Protocol::Ws),
            "wss" => Some(Protocol::Wss),
            _ => None,
        }
    }

    fn is_http(&self) -> bool {
        match *self {
            Protocol::Https | Protocol::Http => true,
            _ => false,
        }
    }

    fn is_secure(&self) -> bool {
        match *self {
            Protocol::Https | Protocol::Wss => true,
            _ => false,
        }
    }

    fn port(&self) -> u16 {
        match *self {
            Protocol::Http | Protocol::Ws => 80,
            Protocol::Https | Protocol::Wss => 443
        }
    }
}

#[derive(Hash, Eq, PartialEq, Clone, Debug)]
struct Key {
    host: String,
    port: u16,
    ssl: bool,
}

impl Key {
    fn empty() -> Key {
        Key{host: String::new(), port: 0, ssl: false}
    }
}

#[derive(Debug)]
struct Conn(Instant, Connection);

enum Acquire {
    Acquired(Connection),
    Available,
    NotAvailable,
}

struct AcquiredConn(Key, Option<Rc<Pool>>);

impl AcquiredConn {
    fn close(&mut self, conn: Connection) {
        if let Some(pool) = self.1.take() {
            pool.close(conn);
        }
    }
    fn release(&mut self, conn: Connection) {
        if let Some(pool) = self.1.take() {
            pool.release(conn);
        }
    }
}

impl Drop for AcquiredConn {
    fn drop(&mut self) {
        if let Some(pool) = self.1.take() {
            pool.release_key(self.0.clone());
        }
    }
}

pub struct Pool {
    keys: RefCell<Vec<Key>>,
    to_close: RefCell<Vec<Connection>>,
    to_release: RefCell<Vec<Connection>>,
    task: RefCell<Option<Task>>,
    modified: Rc<Cell<bool>>,
}

impl Pool {
    fn new(modified: Rc<Cell<bool>>) -> Pool {
        Pool {
            modified,
            keys: RefCell::new(Vec::new()),
            to_close: RefCell::new(Vec::new()),
            to_release: RefCell::new(Vec::new()),
            task: RefCell::new(None),
        }
    }

    fn collect_keys(&self) -> Option<Vec<Key>> {
        if self.keys.borrow().is_empty() {
            None
        } else {
            Some(mem::replace(&mut *self.keys.borrow_mut(), Vec::new()))
        }
    }

    fn collect_close(&self) -> Option<Vec<Connection>> {
        if self.to_close.borrow().is_empty() {
            None
        } else {
            Some(mem::replace(&mut *self.to_close.borrow_mut(), Vec::new()))
        }
    }

    fn collect_release(&self) -> Option<Vec<Connection>> {
        if self.to_release.borrow().is_empty() {
            None
        } else {
            Some(mem::replace(&mut *self.to_release.borrow_mut(), Vec::new()))
        }
    }

    fn close(&self, conn: Connection) {
        self.modified.set(true);
        self.to_close.borrow_mut().push(conn);
        if let Some(ref task) = *self.task.borrow() {
            task.notify()
        }
    }

    fn release(&self, conn: Connection) {
        self.modified.set(true);
        self.to_release.borrow_mut().push(conn);
        if let Some(ref task) = *self.task.borrow() {
            task.notify()
        }
    }

    fn release_key(&self, key: Key) {
        self.modified.set(true);
        self.keys.borrow_mut().push(key);
        if let Some(ref task) = *self.task.borrow() {
            task.notify()
        }
    }
}


pub struct Connection {
    key: Key,
    stream: Box<IoStream>,
    pool: Option<AcquiredConn>,
    ts: Instant,
}

impl fmt::Debug for Connection {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        write!(f, "Connection {}:{}", self.key.host, self.key.port)
    }
}

impl Connection {
    fn new(key: Key, pool: Option<AcquiredConn>, stream: Box<IoStream>) -> Self {
        Connection {key, stream, pool, ts: Instant::now()}
    }

    pub fn stream(&mut self) -> &mut IoStream {
        &mut *self.stream
    }

    pub fn from_stream<T: IoStream>(io: T) -> Connection {
        Connection::new(Key::empty(), None, Box::new(io))
    }

    pub fn close(mut self) {
        if let Some(mut pool) = self.pool.take() {
            pool.close(self)
        }
    }

    pub fn release(mut self) {
        if let Some(mut pool) = self.pool.take() {
            pool.release(self)
        }
    }
}

impl IoStream for Connection {
    fn shutdown(&mut self, how: Shutdown) -> io::Result<()> {
        IoStream::shutdown(&mut *self.stream, how)
    }

    #[inline]
    fn set_nodelay(&mut self, nodelay: bool) -> io::Result<()> {
        IoStream::set_nodelay(&mut *self.stream, nodelay)
    }

    #[inline]
    fn set_linger(&mut self, dur: Option<time::Duration>) -> io::Result<()> {
        IoStream::set_linger(&mut *self.stream, dur)
    }
}

impl io::Read for Connection {
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        self.stream.read(buf)
    }
}

impl AsyncRead for Connection {}

impl io::Write for Connection {
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        self.stream.write(buf)
    }

    fn flush(&mut self) -> io::Result<()> {
        self.stream.flush()
    }
}

impl AsyncWrite for Connection {
    fn shutdown(&mut self) -> Poll<(), io::Error> {
        self.stream.shutdown()
    }
}
