use std::rc::Rc;
use std::cell::UnsafeCell;
use std::marker::PhantomData;
use futures::{Async, Future, Poll};

use error::Error;
use handler::{Handler, Reply, ReplyItem, Responder};
use httprequest::HttpRequest;
use httpresponse::HttpResponse;
use extractor::HttpRequestExtractor;


/// Trait defines object that could be registered as route handler
#[allow(unused_variables)]
pub trait WithHandler<T, S>: 'static
    where T: HttpRequestExtractor<S>, S: 'static
{
    /// The type of value that handler will return.
    type Result: Responder;

    /// Handle request
    fn handle(&mut self, data: T) -> Self::Result;
}

/// WithHandler<D, T, S> for Fn()
impl<T, S, F, R> WithHandler<T, S> for F
    where F: Fn(T) -> R + 'static,
          R: Responder + 'static,
          T: HttpRequestExtractor<S>,
          S: 'static,
{
    type Result = R;

    fn handle(&mut self, item: T) -> R {
        (self)(item)
    }
}

pub(crate)
fn with<T, S, H>(h: H) -> With<T, S, H>
    where H: WithHandler<T, S>,
          T: HttpRequestExtractor<S>,
{
    With{hnd: Rc::new(UnsafeCell::new(h)), _t: PhantomData, _s: PhantomData}
}

pub struct With<T, S, H>
    where H: WithHandler<T, S>,
          T: HttpRequestExtractor<S>,
          S: 'static,
{
    hnd: Rc<UnsafeCell<H>>,
    _t: PhantomData<T>,
    _s: PhantomData<S>,
}

impl<T, S, H> Handler<S> for With<T, S, H>
    where H: WithHandler<T, S>,
          T: HttpRequestExtractor<S> + 'static,
          S: 'static, H: 'static
{
    type Result = Reply;

    fn handle(&mut self, req: HttpRequest<S>) -> Self::Result {
        let fut = Box::new(T::extract(&req));

        Reply::async(
            WithHandlerFut{
                req,
                hnd: Rc::clone(&self.hnd),
                fut1: Some(fut),
                fut2: None,
            })
    }
}

struct WithHandlerFut<T, S, H>
    where H: WithHandler<T, S>,
          T: HttpRequestExtractor<S>,
          T: 'static, S: 'static
{
    hnd: Rc<UnsafeCell<H>>,
    req: HttpRequest<S>,
    fut1: Option<Box<Future<Item=T, Error=Error>>>,
    fut2: Option<Box<Future<Item=HttpResponse, Error=Error>>>,
}

impl<T, S, H> Future for WithHandlerFut<T, S, H>
    where H: WithHandler<T, S>,
          T: HttpRequestExtractor<S> + 'static,
          S: 'static
{
    type Item = HttpResponse;
    type Error = Error;

    fn poll(&mut self) -> Poll<Self::Item, Self::Error> {
        if let Some(ref mut fut) = self.fut2 {
            return fut.poll()
        }

        let item = match self.fut1.as_mut().unwrap().poll()? {
            Async::Ready(item) => item,
            Async::NotReady => return Ok(Async::NotReady),
        };

        let hnd: &mut H = unsafe{&mut *self.hnd.get()};
        let item = match hnd.handle(item)
            .respond_to(self.req.without_state())
        {
            Ok(item) => item.into(),
            Err(err) => return Err(err.into()),
        };

        match item.into() {
            ReplyItem::Message(resp) => return Ok(Async::Ready(resp)),
            ReplyItem::Future(fut) => self.fut2 = Some(fut),
        }

        self.poll()
    }
}

pub(crate)
fn with2<T1, T2, S, F, R>(h: F) -> With2<T1, T2, S, F, R>
    where F: Fn(T1, T2) -> R,
          R: Responder,
          T1: HttpRequestExtractor<S>,
          T2: HttpRequestExtractor<S>,
{
    With2{hnd: Rc::new(UnsafeCell::new(h)),
          _t1: PhantomData, _t2: PhantomData, _s: PhantomData}
}

pub struct With2<T1, T2, S, F, R>
    where F: Fn(T1, T2) -> R,
          R: Responder,
          T1: HttpRequestExtractor<S>,
          T2: HttpRequestExtractor<S>,
          S: 'static,
{
    hnd: Rc<UnsafeCell<F>>,
    _t1: PhantomData<T1>,
    _t2: PhantomData<T2>,
    _s: PhantomData<S>,
}

impl<T1, T2, S, F, R> Handler<S> for With2<T1, T2, S, F, R>
    where F: Fn(T1, T2) -> R + 'static,
          R: Responder + 'static,
          T1: HttpRequestExtractor<S> + 'static,
          T2: HttpRequestExtractor<S> + 'static,
          S: 'static
{
    type Result = Reply;

    fn handle(&mut self, req: HttpRequest<S>) -> Self::Result {
        let fut = Box::new(T1::extract(&req));

        Reply::async(
            WithHandlerFut2{
                req,
                hnd: Rc::clone(&self.hnd),
                item: None,
                fut1: Some(fut),
                fut2: None,
                fut3: None,
            })
    }
}

struct WithHandlerFut2<T1, T2, S, F, R>
    where F: Fn(T1, T2) -> R + 'static,
          R: Responder + 'static,
          T1: HttpRequestExtractor<S> + 'static,
          T2: HttpRequestExtractor<S> + 'static,
          S: 'static
{
    hnd: Rc<UnsafeCell<F>>,
    req: HttpRequest<S>,
    item: Option<T1>,
    fut1: Option<Box<Future<Item=T1, Error=Error>>>,
    fut2: Option<Box<Future<Item=T2, Error=Error>>>,
    fut3: Option<Box<Future<Item=HttpResponse, Error=Error>>>,
}

impl<T1, T2, S, F, R> Future for WithHandlerFut2<T1, T2, S, F, R>
    where F: Fn(T1, T2) -> R + 'static,
          R: Responder + 'static,
          T1: HttpRequestExtractor<S> + 'static,
          T2: HttpRequestExtractor<S> + 'static,
          S: 'static
{
    type Item = HttpResponse;
    type Error = Error;

    fn poll(&mut self) -> Poll<Self::Item, Self::Error> {
        if let Some(ref mut fut) = self.fut3 {
            return fut.poll()
        }

        if self.fut1.is_some() {
            match self.fut1.as_mut().unwrap().poll()? {
                Async::Ready(item) => {
                    self.item = Some(item);
                    self.fut1.take();
                    self.fut2 = Some(Box::new(T2::extract(&self.req)));
                },
                Async::NotReady => return Ok(Async::NotReady),
            }
        }

        let item = match self.fut2.as_mut().unwrap().poll()? {
            Async::Ready(item) => item,
            Async::NotReady => return Ok(Async::NotReady),
        };

        let hnd: &mut F = unsafe{&mut *self.hnd.get()};
        let item = match (*hnd)(self.item.take().unwrap(), item)
            .respond_to(self.req.without_state())
        {
            Ok(item) => item.into(),
            Err(err) => return Err(err.into()),
        };

        match item.into() {
            ReplyItem::Message(resp) => return Ok(Async::Ready(resp)),
            ReplyItem::Future(fut) => self.fut3 = Some(fut),
        }

        self.poll()
    }
}

pub(crate)
fn with3<T1, T2, T3, S, F, R>(h: F) -> With3<T1, T2, T3, S, F, R>
    where F: Fn(T1, T2, T3) -> R + 'static,
          R: Responder,
          T1: HttpRequestExtractor<S>,
          T2: HttpRequestExtractor<S>,
          T3: HttpRequestExtractor<S>,
{
    With3{hnd: Rc::new(UnsafeCell::new(h)),
          _s: PhantomData, _t1: PhantomData, _t2: PhantomData, _t3: PhantomData}
}

pub struct With3<T1, T2, T3, S, F, R>
    where F: Fn(T1, T2, T3) -> R + 'static,
          R: Responder + 'static,
          T1: HttpRequestExtractor<S>,
          T2: HttpRequestExtractor<S>,
          T3: HttpRequestExtractor<S>,
          S: 'static,
{
    hnd: Rc<UnsafeCell<F>>,
    _t1: PhantomData<T1>,
    _t2: PhantomData<T2>,
    _t3: PhantomData<T3>,
    _s: PhantomData<S>,
}

impl<T1, T2, T3, S, F, R> Handler<S> for With3<T1, T2, T3, S, F, R>
    where F: Fn(T1, T2, T3) -> R + 'static,
          R: Responder + 'static,
          T1: HttpRequestExtractor<S>,
          T2: HttpRequestExtractor<S>,
          T3: HttpRequestExtractor<S>,
          T1: 'static, T2: 'static, T3: 'static, S: 'static
{
    type Result = Reply;

    fn handle(&mut self, req: HttpRequest<S>) -> Self::Result {
        let fut = Box::new(T1::extract(&req));

        Reply::async(
            WithHandlerFut3{
                req,
                hnd: Rc::clone(&self.hnd),
                item1: None,
                item2: None,
                fut1: Some(fut),
                fut2: None,
                fut3: None,
                fut4: None,
            })
    }
}

struct WithHandlerFut3<T1, T2, T3, S, F, R>
    where F: Fn(T1, T2, T3) -> R + 'static,
          R: Responder + 'static,
          T1: HttpRequestExtractor<S> + 'static,
          T2: HttpRequestExtractor<S> + 'static,
          T3: HttpRequestExtractor<S> + 'static,
          S: 'static
{
    hnd: Rc<UnsafeCell<F>>,
    req: HttpRequest<S>,
    item1: Option<T1>,
    item2: Option<T2>,
    fut1: Option<Box<Future<Item=T1, Error=Error>>>,
    fut2: Option<Box<Future<Item=T2, Error=Error>>>,
    fut3: Option<Box<Future<Item=T3, Error=Error>>>,
    fut4: Option<Box<Future<Item=HttpResponse, Error=Error>>>,
}

impl<T1, T2, T3, S, F, R> Future for WithHandlerFut3<T1, T2, T3, S, F, R>
    where F: Fn(T1, T2, T3) -> R + 'static,
          R: Responder + 'static,
          T1: HttpRequestExtractor<S> + 'static,
          T2: HttpRequestExtractor<S> + 'static,
          T3: HttpRequestExtractor<S> + 'static,
          S: 'static
{
    type Item = HttpResponse;
    type Error = Error;

    fn poll(&mut self) -> Poll<Self::Item, Self::Error> {
        if let Some(ref mut fut) = self.fut4 {
            return fut.poll()
        }

        if self.fut1.is_some() {
            match self.fut1.as_mut().unwrap().poll()? {
                Async::Ready(item) => {
                    self.item1 = Some(item);
                    self.fut1.take();
                    self.fut2 = Some(Box::new(T2::extract(&self.req)));
                },
                Async::NotReady => return Ok(Async::NotReady),
            }
        }

        if self.fut2.is_some() {
            match self.fut2.as_mut().unwrap().poll()? {
                Async::Ready(item) => {
                    self.item2 = Some(item);
                    self.fut2.take();
                    self.fut3 = Some(Box::new(T3::extract(&self.req)));
                },
                Async::NotReady => return Ok(Async::NotReady),
            }
        }

        let item = match self.fut3.as_mut().unwrap().poll()? {
            Async::Ready(item) => item,
            Async::NotReady => return Ok(Async::NotReady),
        };

        let hnd: &mut F = unsafe{&mut *self.hnd.get()};
        let item = match (*hnd)(self.item1.take().unwrap(),
                                self.item2.take().unwrap(),
                                item)
            .respond_to(self.req.without_state())
        {
            Ok(item) => item.into(),
            Err(err) => return Err(err.into()),
        };

        match item.into() {
            ReplyItem::Message(resp) => return Ok(Async::Ready(resp)),
            ReplyItem::Future(fut) => self.fut4 = Some(fut),
        }

        self.poll()
    }
}
