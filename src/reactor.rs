use crate::futures::MyTcpListener;
use dashmap::{DashMap, Map};
use log::{info, trace, warn};
use std::io::prelude::*;
use std::net::{TcpListener, TcpStream};
use std::os::unix::io::{AsRawFd, RawFd};
use std::sync::atomic::{AtomicU64, Ordering};
use std::task::Waker;
use std::thread;
use std::thread::JoinHandle;
use std::{collections::HashMap, io};
const HTTP_RESP: &[u8] = b"HTTP/1.1 200 OK
content-type: text/html
content-length: 5

Hello";

#[allow(unused_macros)]
macro_rules! syscall {
    ($fn: ident ( $($arg: expr),* $(,)* ) ) => {{
        let res = unsafe { libc::$fn($($arg, )*) };
        if res == -1 {
            Err(std::io::Error::last_os_error())
        } else {
            Ok(res)
        }
    }};
}
pub const EPOLL_KEY: u64 = 100;
const READ_FLAGS: i32 = libc::EPOLLET
    | libc::EPOLLEXCLUSIVE
    | libc::EPOLLIN
    | libc::EPOLLOUT
    | libc::EPOLLERR
    | libc::EPOLLHUP
    | (1 << 28);

fn epoll_create() -> io::Result<RawFd> {
    let fd = syscall!(epoll_create1(0))?;

    // Get the flags associated with `fd`
    if let Ok(flags) = syscall!(fcntl(fd, libc::F_GETFD)) {
        // set the flag CLOSE ON EXEC for the fd
        let _ = syscall!(fcntl(fd, libc::F_SETFD, flags | libc::FD_CLOEXEC));
    }

    Ok(fd)
}

fn add_interest(epoll_fd: RawFd, fd: RawFd, mut event: libc::epoll_event) -> io::Result<()> {
    syscall!(epoll_ctl(epoll_fd, libc::EPOLL_CTL_ADD, fd, &mut event))?;
    Ok(())
}

fn modify_interest(epoll_fd: RawFd, fd: RawFd, mut event: libc::epoll_event) -> io::Result<()> {
    syscall!(epoll_ctl(epoll_fd, libc::EPOLL_CTL_MOD, fd, &mut event))?;
    Ok(())
}

fn listener_read_event(key: u64) -> libc::epoll_event {
    libc::epoll_event {
        events: READ_FLAGS as u32,
        u64: key,
    }
}

// fn listener_write_event(key: u64) -> libc::epoll_event {
//     libc::epoll_event {
//         events: WRITE_FLAGS as u32,
//         u64: key,
//     }
// }

#[derive(Debug)]
struct RequestContext {
    pub stream: TcpStream,
    pub content_length: usize,
    pub buf: [u8; 1024],
}

impl RequestContext {
    fn new(stream: TcpStream) -> Self {
        Self {
            stream,
            buf: [0u8; 1024],
            content_length: 0,
        }
    }
    fn read_cb(&mut self, key: u64, epoll_fd: RawFd) -> io::Result<()> {
        let mut buf: Vec<u8> = Vec::new();
        loop {
            match self.stream.read(&mut self.buf) {
                Ok(len) => {
                    if len > 0 {
                        buf.extend_from_slice(&self.buf[0..len]);
                    } else {
                        break;
                    }
                }
                Err(e) if e.kind() == io::ErrorKind::WouldBlock => {
                    // println!("brk");
                    break;
                }
                Err(e) => {
                    return Err(e);
                }
            }
        }
        // self.parse_and_set_content_length(buf);
        // println!(
        // "Read data as: {} {}",
        // std::str::from_utf8(&self.buf).unwrap(),
        // self.buf.len()
        // );
        // modify_interest(epoll_fd, self.stream.as_raw_fd(), listener_write_event(key))?;
        Ok(())
    }

    fn parse_and_set_content_length(&mut self, data: &str) {
        if data.contains("HTTP") {
            if let Some(content_length) = data
                .lines()
                .find(|l| l.to_lowercase().starts_with("content-length: "))
            {
                if let Some(len) = content_length
                    .to_lowercase()
                    .strip_prefix("content-length: ")
                {
                    self.content_length = len.parse::<usize>().expect("content-length is valid");
                    // !("set content length: {} bytes", self.content_length);
                }
            }
        }
    }
    fn write_cb(&mut self, key: u64, epoll_fd: RawFd) -> io::Result<()> {
        self.stream.write_all(HTTP_RESP)?;
        self.stream.flush().unwrap();
        let fd = self.stream.as_raw_fd();
        Ok(())
    }
    fn close(&mut self, epoll_fd: RawFd) {
        let fd = self.stream.as_raw_fd();
        self.stream.shutdown(std::net::Shutdown::Both);
        remove_interest(epoll_fd, fd);
        close(fd);
    }
}

fn close(fd: RawFd) {
    let _ = syscall!(close(fd));
}

fn remove_interest(epoll_fd: RawFd, fd: RawFd) -> io::Result<()> {
    syscall!(epoll_ctl(
        epoll_fd,
        libc::EPOLL_CTL_DEL,
        fd,
        std::ptr::null_mut()
    ))?;
    Ok(())
}

pub struct Reactor {
    worker_threads: usize,
    epoll_buffer_size: usize,
    epoll_wait_time: i32,
    reader_waker: DashMap<u64, std::task::Waker>,
    writer_waker: DashMap<u64, std::task::Waker>,
    streams: DashMap<u64, TcpStream>,
    epoll_fd: i32,
    key: AtomicU64,
}

impl Reactor {
    pub fn new(epoll_buffer_size: usize, worker_threads: usize, epoll_wait_time: i32) -> Self {
        Self {
            worker_threads,
            epoll_buffer_size,
            epoll_wait_time,
            reader_waker: DashMap::new(),
            writer_waker: DashMap::new(),
            streams: DashMap::new(),
            epoll_fd: epoll_create().expect("can create epoll queue"),
            key: AtomicU64::new(EPOLL_KEY + 1),
        }
    }

    pub fn run(&'static self) -> Result<(), io::Error> {
        // let listener = TcpListener::bind("127.0.0.1:8000")?;
        let handles: Vec<JoinHandle<io::Result<()>>> = (0..self.worker_threads)
            .into_iter()
            .map(|tid| {
                // let listener = listener.try_clone().unwrap();
                let epoll_buffer_size = self.epoll_buffer_size;
                let epoll_wait_time = self.epoll_wait_time;
                let readers = &self.reader_waker;
                let writers = &self.writer_waker;
                // let listener = listener.try_clone().unwrap();
                thread::spawn(move || -> io::Result<()> {
                    // listener.set_nonblocking(true);
                    // let listener_fd = listener.as_raw_fd();
                    // let mut request_contexts: HashMap<u64, RequestContext> = HashMap::new();
                    let epoll_fd = self.epoll_fd;
                    // add_interest(epoll_fd, listener_fd, listener_read_event(EPOLL_KEY));

                    let mut events: Vec<libc::epoll_event> = Vec::with_capacity(epoll_buffer_size);

                    loop {
                        events.clear();
                        let res = match syscall!(epoll_wait(
                            epoll_fd,
                            events.as_mut_ptr() as *mut libc::epoll_event,
                            epoll_buffer_size as i32,
                            epoll_wait_time,
                        )) {
                            Ok(v) => v,
                            Err(e) => panic!("error during epoll wait: {}", e),
                        };

                        // safe  as long as the kernel does nothing wrong - copied from mio
                        unsafe { events.set_len(res as usize) };

                        // println!("requests in flight: {}", request_contexts.len());
                        for ev in &events {
                            // println!("event {:?} {:?}", ev.u64, ev.events);
                            let key = ev.u64;
                            // match ev.u64 {
                            //     EPOLL_KEY => {
                            //         match listener.accept() {
                            //             Ok((stream, addr)) => {
                            //                 stream.set_nonblocking(true);
                            //                 key += 1;
                            //                 add_interest(
                            //                     epoll_fd,
                            //                     stream.as_raw_fd(),
                            //                     listener_read_event(key),
                            //                 );
                            //                 println!("new client: {}->{}", addr, key);
                            //                 request_contexts
                            //                     .insert(key, RequestContext::new(stream));
                            //             }
                            //             Err(e) => {
                            //                 // self.stream.shutdown(std::net::Shutdown::Both);
                            //                 // let fd = self.stream.as_raw_fd();
                            //                 // remove_interest(epoll_fd, fd);
                            //                 // close(fd);
                            //                 eprintln!("couldn't accept: {}", e)
                            //             }
                            //         };
                            //     }
                            // let mut to_delete = None;
                            let v: i32 = ev.events as i32;
                            // println!("event {:?}", events);
                            if (v as i32 & libc::EPOLLHUP == libc::EPOLLHUP)
                                || (v as i32 & libc::EPOLLERR == libc::EPOLLERR)
                            {
                                info!("closing key: {} {}", epoll_fd, key);
                                // context.close(epoll_fd);
                                self.close(key);
                            } else {
                                readers.remove(&key).map(|(_, w)| {
                                    w.wake();
                                    Some(())
                                });
                                writers.remove(&key).map(|(_, w)| {
                                    info!("writing to key: {} {}", epoll_fd, key);
                                    w.wake();
                                    Some(())
                                });
                                // // println!("read key: {} {}", v, key);
                                // context.read_cb(key, epoll_fd);
                                // context.write_cb(key, epoll_fd);
                            }
                            // if let Some(key) = to_delete {
                            //     request_contexts.remove(&key);
                            // }
                            // }
                        }
                    }
                })
            })
            .collect();

        // for jh in handles {
        //     jh.join();
        // }
        Ok(())
    }

    pub fn close(&self, key: u64) {
        self.streams.remove(&key).and_then(|(_, stream)| {
            let fd = stream.as_raw_fd();
            stream.shutdown(std::net::Shutdown::Both);
            remove_interest(self.epoll_fd, fd);
            self.reader_waker.remove(&key);
            self.writer_waker.remove(&key);
            close(fd);
            Some(())
        });
    }
    pub fn register_stream_read(&self, key: u64, strm: &TcpStream) {
        self.streams.insert(key, strm.try_clone().unwrap());
        // println!("registered stream {}", key);
        add_interest(self.epoll_fd, strm.as_raw_fd(), listener_read_event(key));
    }
    pub fn register_interest(&self, key: u64, fd: RawFd) {
        // println!("registered interest {}", key);
        add_interest(self.epoll_fd, fd, listener_read_event(key));
    }
    pub fn register_waker_read(&self, key: u64, w: Waker) {
        self.reader_waker.insert(key, w);
    }

    pub fn register_waker_write(&self, key: u64, w: Waker) {
        self.writer_waker.insert(key, w);
    }
    pub fn get_next_key(&self) -> u64 {
        self.key.fetch_add(1, Ordering::SeqCst)
    }
}
