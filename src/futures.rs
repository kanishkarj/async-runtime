use std::{
    io::{self, Read, Write},
    net::{SocketAddr, TcpListener, TcpStream},
    ops::Deref,
    os::unix::prelude::AsRawFd,
    pin::Pin,
    sync::{Arc, Mutex},
    task::{Context, Poll, Waker},
};

use futures::Future;

use crate::executor::RUNTIME;

#[derive(Debug)]
pub struct MyTcpStream {
    inner: (TcpStream, SocketAddr),
    key: u64,
}

impl MyTcpStream {
    pub fn new(inner: (TcpStream, SocketAddr), key: u64) -> Self {
        println!("new client: {} -> {}", inner.1, key);
        inner.0.set_nonblocking(true);
        RUNTIME.register_stream_read(key, &inner.0);
        // println!("{} {}", key, inner.0.as_raw_fd());
        Self { inner, key }
    }
    pub fn read(&self) -> StreamFuture {
        // println!("{} {}", self.key, self.inner.0.as_raw_fd());
        StreamFuture::new(self.inner.0.try_clone().unwrap(), self.key)
    }
    pub fn write(&self, to_write: &'static [u8]) -> StreamFutureWrite {
        StreamFutureWrite::new(self.inner.0.try_clone().unwrap(), self.key, to_write)
    }
}

pub struct MyTcpListener {
    listener: TcpListener,
}

impl MyTcpListener {
    pub fn bind(addr: &str) -> Self {
        let listener = TcpListener::bind(addr).unwrap();
        listener.set_nonblocking(true).unwrap();
        RUNTIME.register_interest(crate::reactor::EPOLL_KEY, listener.as_raw_fd());
        println!("bound to {:?}", addr);
        Self { listener }
    }

    pub fn maccept(&self) -> AcceptFuture {
        AcceptFuture::new(self.listener.try_clone().unwrap())
    }
}

impl Clone for MyTcpListener {
    fn clone(&self) -> Self {
        MyTcpListener {
            listener: self.listener.try_clone().unwrap(),
        }
    }
}

impl Deref for MyTcpListener {
    type Target = TcpListener;
    fn deref(&self) -> &<Self as Deref>::Target {
        &self.listener
    }
}

pub struct StreamFutureWrite {
    shared_state: Arc<Mutex<SharedState2>>,
}

struct SharedState2 {
    stream: TcpStream,
    key: u64,
    to_write: &'static [u8],
    waker: Option<Waker>,
}

impl Future for StreamFutureWrite {
    type Output = Result<(), io::Error>;
    fn poll(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Self::Output> {
        // Look at the shared state to see if the timer has already completed.
        let mut shared_state = self.shared_state.lock().unwrap();
        // let mut buf = [0u8; 4096];
        // let mut buffer = vec![];
        let to_write = shared_state.to_write;
        match shared_state.stream.write(to_write) {
            Ok(n) => {
                shared_state.stream.flush().unwrap();
                // let fd = shared_state.stream.as_raw_fd();
                // RUNTIME.register_stream_read(shared_state.key, fd);
                // println!("done {}", n);
                return Poll::Ready(Ok(()));
            }
            Err(e) if e.kind() == io::ErrorKind::WouldBlock => {
                RUNTIME.register_waker_write(shared_state.key, cx.waker().clone());
                return Poll::Pending;
            }
            Err(e) => {
                return Poll::Ready(Err(e));
            }
        };
    }
}

impl StreamFutureWrite {
    pub fn new(stream: TcpStream, key: u64, to_write: &'static [u8]) -> Self {
        let shared_state = Arc::new(Mutex::new(SharedState2 {
            stream,
            key,
            to_write,
            waker: None,
        }));
        StreamFutureWrite { shared_state }
    }
}

pub struct StreamFuture {
    shared_state: Arc<Mutex<SharedState1>>,
}

struct SharedState1 {
    stream: TcpStream,
    key: u64,
    waker: Option<Waker>,
}

impl Future for StreamFuture {
    type Output = Result<Vec<u8>, io::Error>;
    fn poll(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Self::Output> {
        // Look at the shared state to see if the timer has already completed.
        let mut shared_state = self.shared_state.lock().unwrap();
        //TODO: remember accept would return wouldBlock or some error if it is not possible to accept. Play with this first.
        let mut buf = [0u8; 4096];
        let mut buffer = vec![];
        match shared_state.stream.read(&mut buf) {
            Ok(n) => {
                buffer.extend_from_slice(&buf[0..n]);
                // println!("done");
                return Poll::Ready(Ok(buffer));
            }
            Err(e) if e.kind() == io::ErrorKind::WouldBlock => {
                // println!("blk");
                RUNTIME.register_waker_read(shared_state.key, cx.waker().clone());
                return Poll::Pending;
            }
            Err(e) => {
                return Poll::Ready(Err(e));
            }
        };
    }
}

impl StreamFuture {
    pub fn new(stream: TcpStream, key: u64) -> Self {
        let shared_state = Arc::new(Mutex::new(SharedState1 {
            stream,
            key,
            waker: None,
        }));
        StreamFuture { shared_state }
    }
}

pub struct AcceptFuture {
    shared_state: Arc<Mutex<SharedState>>,
}

struct SharedState {
    listener: TcpListener,
    waker: Option<Waker>,
}

impl Future for AcceptFuture {
    type Output = Result<MyTcpStream, io::Error>;
    fn poll(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Self::Output> {
        // Look at the shared state to see if the timer has already completed.
        let mut shared_state = self.shared_state.lock().unwrap();
        //TODO: remember accept would return wouldBlock or some error if it is not possible to accept. Play with this first.
        match shared_state.listener.accept() {
            Ok(val) => Poll::Ready(Ok(MyTcpStream::new(val, RUNTIME.get_next_key()))),
            Err(err) => match err.kind() {
                io::ErrorKind::WouldBlock => {
                    // println!("blocked on accept");
                    RUNTIME.register_waker_read(crate::reactor::EPOLL_KEY, cx.waker().clone());
                    Poll::Pending
                }
                _ => Poll::Ready(Err(err)),
            },
        }
    }
}

impl AcceptFuture {
    pub fn new(listener: TcpListener) -> Self {
        let shared_state = Arc::new(Mutex::new(SharedState {
            listener,
            waker: None,
        }));
        AcceptFuture { shared_state }
    }
}
