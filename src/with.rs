use std::rc::Rc;
use std::cell::UnsafeCell;
use std::marker::PhantomData;
use std::ops::{Deref, DerefMut};
use futures::{Async, Future, Poll};

use error::Error;
use handler::{Handler, FromRequest, Reply, ReplyItem, Responder};
use httprequest::HttpRequest;
use httpresponse::HttpResponse;

pub struct ExtractorConfig<S: 'static, T: FromRequest<S>> {
    cfg: Rc<UnsafeCell<T::Config>>
}

impl<S: 'static, T: FromRequest<S>> Default for ExtractorConfig<S, T> {
    fn default() -> Self {
        ExtractorConfig { cfg: Rc::new(UnsafeCell::new(T::Config::default())) }
    }
}

impl<S: 'static, T: FromRequest<S>> Clone for ExtractorConfig<S, T> {
    fn clone(&self) -> Self {
        ExtractorConfig { cfg: Rc::clone(&self.cfg) }
    }
}

impl<S: 'static, T: FromRequest<S>> AsRef<T::Config> for ExtractorConfig<S, T> {
    fn as_ref(&self) -> &T::Config {
        unsafe{&*self.cfg.get()}
    }
}

impl<S: 'static, T: FromRequest<S>> Deref for ExtractorConfig<S, T> {
    type Target = T::Config;

    fn deref(&self) -> &T::Config {
        unsafe{&*self.cfg.get()}
    }
}

impl<S: 'static, T: FromRequest<S>> DerefMut for ExtractorConfig<S, T> {
    fn deref_mut(&mut self) -> &mut T::Config {
        unsafe{&mut *self.cfg.get()}
    }
}

pub struct With<T, S, F, R>
    where F: Fn(T) -> R, T: FromRequest<S>, S: 'static,
{
    hnd: Rc<UnsafeCell<F>>,
    cfg: ExtractorConfig<S, T>,
    _s: PhantomData<S>,
}

impl<T, S, F, R> With<T, S, F, R>
    where F: Fn(T) -> R, T: FromRequest<S>, S: 'static,
{
    pub fn new(f: F, cfg: ExtractorConfig<S, T>) -> Self {
        With{cfg, hnd: Rc::new(UnsafeCell::new(f)), _s: PhantomData}
    }
}

impl<T, S, F, R> Handler<S> for With<T, S, F, R>
    where F: Fn(T) -> R + 'static,
          R: Responder + 'static,
          T: FromRequest<S> + 'static,
          S: 'static
{
    type Result = Reply;

    fn handle(&mut self, req: HttpRequest<S>) -> Self::Result {
        let mut fut = WithHandlerFut{
            req,
            started: false,
            hnd: Rc::clone(&self.hnd),
            cfg: self.cfg.clone(),
            fut1: None,
            fut2: None,
        };

        match fut.poll() {
            Ok(Async::Ready(resp)) => Reply::response(resp),
            Ok(Async::NotReady) => Reply::async(fut),
            Err(e) => Reply::response(e),
        }
    }
}

struct WithHandlerFut<T, S, F, R>
    where F: Fn(T) -> R,
          R: Responder,
          T: FromRequest<S> + 'static,
          S: 'static
{
    started: bool,
    hnd: Rc<UnsafeCell<F>>,
    cfg: ExtractorConfig<S, T>,
    req: HttpRequest<S>,
    fut1: Option<Box<Future<Item=T, Error=Error>>>,
    fut2: Option<Box<Future<Item=HttpResponse, Error=Error>>>,
}

impl<T, S, F, R> Future for WithHandlerFut<T, S, F, R>
    where F: Fn(T) -> R,
          R: Responder + 'static,
          T: FromRequest<S> + 'static,
          S: 'static
{
    type Item = HttpResponse;
    type Error = Error;

    fn poll(&mut self) -> Poll<Self::Item, Self::Error> {
        if let Some(ref mut fut) = self.fut2 {
            return fut.poll()
        }

        let item = if !self.started {
            self.started = true;
            let mut fut = T::from_request(&self.req, self.cfg.as_ref());
            match fut.poll() {
                Ok(Async::Ready(item)) => item,
                Ok(Async::NotReady) => {
                    self.fut1 = Some(Box::new(fut));
                    return Ok(Async::NotReady)
                },
                Err(e) => return Err(e),
            }
        } else {
            match self.fut1.as_mut().unwrap().poll()? {
                Async::Ready(item) => item,
                Async::NotReady => return Ok(Async::NotReady),
            }
        };

        let hnd: &mut F = unsafe{&mut *self.hnd.get()};
        let item = match (*hnd)(item).respond_to(self.req.drop_state()) {
            Ok(item) => item.into(),
            Err(e) => return Err(e.into()),
        };

        match item.into() {
            ReplyItem::Message(resp) => Ok(Async::Ready(resp)),
            ReplyItem::Future(fut) => {
                self.fut2 = Some(fut);
                self.poll()
            }
        }
    }
}

pub struct With2<T1, T2, S, F, R>
    where F: Fn(T1, T2) -> R,
          T1: FromRequest<S> + 'static, T2: FromRequest<S> + 'static, S: 'static
{
    hnd: Rc<UnsafeCell<F>>,
    cfg1: ExtractorConfig<S, T1>,
    cfg2: ExtractorConfig<S, T2>,
    _s: PhantomData<S>,
}

impl<T1, T2, S, F, R> With2<T1, T2, S, F, R>
    where F: Fn(T1, T2) -> R,
          T1: FromRequest<S> + 'static, T2: FromRequest<S> + 'static, S: 'static
{
    pub fn new(f: F, cfg1: ExtractorConfig<S, T1>, cfg2: ExtractorConfig<S, T2>) -> Self {
        With2{hnd: Rc::new(UnsafeCell::new(f)),
              cfg1, cfg2, _s: PhantomData}
    }
}

impl<T1, T2, S, F, R> Handler<S> for With2<T1, T2, S, F, R>
    where F: Fn(T1, T2) -> R + 'static,
          R: Responder + 'static,
          T1: FromRequest<S> + 'static,
          T2: FromRequest<S> + 'static,
          S: 'static
{
    type Result = Reply;

    fn handle(&mut self, req: HttpRequest<S>) -> Self::Result {
        let mut fut = WithHandlerFut2{
            req,
            started: false,
            hnd: Rc::clone(&self.hnd),
            cfg1: self.cfg1.clone(),
            cfg2: self.cfg2.clone(),
            item: None,
            fut1: None,
            fut2: None,
            fut3: None,
        };
        match fut.poll() {
            Ok(Async::Ready(resp)) => Reply::response(resp),
            Ok(Async::NotReady) => Reply::async(fut),
            Err(e) => Reply::response(e),
        }
    }
}

struct WithHandlerFut2<T1, T2, S, F, R>
    where F: Fn(T1, T2) -> R + 'static,
          R: Responder + 'static,
          T1: FromRequest<S> + 'static,
          T2: FromRequest<S> + 'static,
          S: 'static
{
    started: bool,
    hnd: Rc<UnsafeCell<F>>,
    cfg1: ExtractorConfig<S, T1>,
    cfg2: ExtractorConfig<S, T2>,
    req: HttpRequest<S>,
    item: Option<T1>,
    fut1: Option<Box<Future<Item=T1, Error=Error>>>,
    fut2: Option<Box<Future<Item=T2, Error=Error>>>,
    fut3: Option<Box<Future<Item=HttpResponse, Error=Error>>>,
}

impl<T1, T2, S, F, R> Future for WithHandlerFut2<T1, T2, S, F, R>
    where F: Fn(T1, T2) -> R + 'static,
          R: Responder + 'static,
          T1: FromRequest<S> + 'static,
          T2: FromRequest<S> + 'static,
          S: 'static
{
    type Item = HttpResponse;
    type Error = Error;

    fn poll(&mut self) -> Poll<Self::Item, Self::Error> {
        if let Some(ref mut fut) = self.fut3 {
            return fut.poll()
        }

        if !self.started {
            self.started = true;
            let mut fut = T1::from_request(&self.req, self.cfg1.as_ref());
            match fut.poll() {
                Ok(Async::Ready(item1)) => {
                    let mut fut = T2::from_request(&self.req,self.cfg2.as_ref());
                    match fut.poll() {
                        Ok(Async::Ready(item2)) => {
                            let hnd: &mut F = unsafe{&mut *self.hnd.get()};
                            match (*hnd)(item1, item2)
                                .respond_to(self.req.drop_state())
                            {
                                Ok(item) => match item.into().into() {
                                    ReplyItem::Message(resp) =>
                                        return Ok(Async::Ready(resp)),
                                    ReplyItem::Future(fut) => {
                                        self.fut3 = Some(fut);
                                        return self.poll()
                                    }
                                },
                                Err(e) => return Err(e.into()),
                            }
                        },
                        Ok(Async::NotReady) => {
                            self.item = Some(item1);
                            self.fut2 = Some(Box::new(fut));
                            return Ok(Async::NotReady);
                        },
                        Err(e) => return Err(e),
                    }
                },
                Ok(Async::NotReady) => {
                    self.fut1 = Some(Box::new(fut));
                    return Ok(Async::NotReady);
                }
                Err(e) => return Err(e),
            }
        }

        if self.fut1.is_some() {
            match self.fut1.as_mut().unwrap().poll()? {
                Async::Ready(item) => {
                    self.item = Some(item);
                    self.fut1.take();
                    self.fut2 = Some(Box::new(
                        T2::from_request(&self.req, self.cfg2.as_ref())));
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
            .respond_to(self.req.drop_state())
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

pub struct With3<T1, T2, T3, S, F, R>
    where F: Fn(T1, T2, T3) -> R,
          T1: FromRequest<S> + 'static,
          T2: FromRequest<S> + 'static,
          T3: FromRequest<S> + 'static,
          S: 'static
{
    hnd: Rc<UnsafeCell<F>>,
    cfg1: ExtractorConfig<S, T1>,
    cfg2: ExtractorConfig<S, T2>,
    cfg3: ExtractorConfig<S, T3>,
    _s: PhantomData<S>,
}


impl<T1, T2, T3, S, F, R> With3<T1, T2, T3, S, F, R>
    where F: Fn(T1, T2, T3) -> R,
          T1: FromRequest<S> + 'static,
          T2: FromRequest<S> + 'static,
          T3: FromRequest<S> + 'static,
          S: 'static
{
    pub fn new(f: F, cfg1: ExtractorConfig<S, T1>,
               cfg2: ExtractorConfig<S, T2>, cfg3: ExtractorConfig<S, T3>) -> Self
    {
        With3{hnd: Rc::new(UnsafeCell::new(f)), cfg1, cfg2, cfg3,  _s: PhantomData}
    }
}

impl<T1, T2, T3, S, F, R> Handler<S> for With3<T1, T2, T3, S, F, R>
    where F: Fn(T1, T2, T3) -> R + 'static,
          R: Responder + 'static,
          T1: FromRequest<S>,
          T2: FromRequest<S>,
          T3: FromRequest<S>,
          T1: 'static, T2: 'static, T3: 'static, S: 'static
{
    type Result = Reply;

    fn handle(&mut self, req: HttpRequest<S>) -> Self::Result {
        let mut fut = WithHandlerFut3{
            req,
            hnd: Rc::clone(&self.hnd),
            cfg1: self.cfg1.clone(),
            cfg2: self.cfg2.clone(),
            cfg3: self.cfg3.clone(),
            started: false,
            item1: None,
            item2: None,
            fut1: None,
            fut2: None,
            fut3: None,
            fut4: None,
        };
        match fut.poll() {
            Ok(Async::Ready(resp)) => Reply::response(resp),
            Ok(Async::NotReady) => Reply::async(fut),
            Err(e) => Reply::response(e),
        }
    }
}

struct WithHandlerFut3<T1, T2, T3, S, F, R>
    where F: Fn(T1, T2, T3) -> R + 'static,
          R: Responder + 'static,
          T1: FromRequest<S> + 'static,
          T2: FromRequest<S> + 'static,
          T3: FromRequest<S> + 'static,
          S: 'static
{
    hnd: Rc<UnsafeCell<F>>,
    req: HttpRequest<S>,
    cfg1: ExtractorConfig<S, T1>,
    cfg2: ExtractorConfig<S, T2>,
    cfg3: ExtractorConfig<S, T3>,
    started: bool,
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
          T1: FromRequest<S> + 'static,
          T2: FromRequest<S> + 'static,
          T3: FromRequest<S> + 'static,
          S: 'static
{
    type Item = HttpResponse;
    type Error = Error;

    fn poll(&mut self) -> Poll<Self::Item, Self::Error> {
        if let Some(ref mut fut) = self.fut4 {
            return fut.poll()
        }

        if !self.started {
            self.started = true;
            let mut fut = T1::from_request(&self.req, self.cfg1.as_ref());
            match fut.poll() {
                Ok(Async::Ready(item1)) => {
                    let mut fut = T2::from_request(&self.req, self.cfg2.as_ref());
                    match fut.poll() {
                        Ok(Async::Ready(item2)) => {
                            let mut fut = T3::from_request(&self.req, self.cfg3.as_ref());
                            match fut.poll() {
                                Ok(Async::Ready(item3)) => {
                                    let hnd: &mut F = unsafe{&mut *self.hnd.get()};
                                    match (*hnd)(item1, item2, item3)
                                        .respond_to(self.req.drop_state())
                                    {
                                        Ok(item) => match item.into().into() {
                                            ReplyItem::Message(resp) =>
                                                return Ok(Async::Ready(resp)),
                                            ReplyItem::Future(fut) => {
                                                self.fut4 = Some(fut);
                                                return self.poll()
                                            }
                                        },
                                        Err(e) => return Err(e.into()),
                                    }
                                },
                                Ok(Async::NotReady) => {
                                    self.item1 = Some(item1);
                                    self.item2 = Some(item2);
                                    self.fut3 = Some(Box::new(fut));
                                    return Ok(Async::NotReady);
                                },
                                Err(e) => return Err(e),
                            }
                        },
                        Ok(Async::NotReady) => {
                            self.item1 = Some(item1);
                            self.fut2 = Some(Box::new(fut));
                            return Ok(Async::NotReady);
                        },
                        Err(e) => return Err(e),
                    }
                },
                Ok(Async::NotReady) => {
                    self.fut1 = Some(Box::new(fut));
                    return Ok(Async::NotReady);
                }
                Err(e) => return Err(e),
            }
        }

        if self.fut1.is_some() {
            match self.fut1.as_mut().unwrap().poll()? {
                Async::Ready(item) => {
                    self.item1 = Some(item);
                    self.fut1.take();
                    self.fut2 = Some(Box::new(
                        T2::from_request(&self.req, self.cfg2.as_ref())));
                },
                Async::NotReady => return Ok(Async::NotReady),
            }
        }

        if self.fut2.is_some() {
            match self.fut2.as_mut().unwrap().poll()? {
                Async::Ready(item) => {
                    self.item2 = Some(item);
                    self.fut2.take();
                    self.fut3 = Some(Box::new(
                        T3::from_request(&self.req, self.cfg3.as_ref())));
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
            .respond_to(self.req.drop_state())
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
