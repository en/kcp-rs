extern crate futures;
extern crate rand;
#[macro_use]
extern crate tokio_core;
extern crate tokio_io;
extern crate time;
extern crate byteorder;
extern crate bytes;
extern crate time as ctime;
extern crate mio;

pub use kcp::KCP;

mod kcp;

use std::io::{self, Read, Write};
use std::net::{SocketAddr, IpAddr, Shutdown, Ipv4Addr};
use std::str;
use std::cell::RefCell;
use std::collections::HashMap;
use std::time::Duration;
use std::sync::Arc;

use bytes::{Buf, BufMut};
use futures::{Poll, Async, Future};
use futures::stream::Stream;
use tokio_core::net::UdpSocket;
use tokio_core::reactor::{Handle, PollEvented, Interval, Remote};
use tokio_io::{AsyncRead, AsyncWrite};
use byteorder::{ByteOrder, LittleEndian};
use mio::{Ready, Registration, PollOpt, Token, SetReadiness};
use mio::event::Evented;

struct KcpPair {
    k: Arc<RefCell<KCP>>,
    set_readiness: SetReadiness,
}

pub struct KcpListener {
    udp: Arc<UdpSocket>,
    connections: HashMap<SocketAddr, KcpPair>,
    handle: Remote,
}

pub struct Incoming {
    inner: KcpListener,
}

impl KcpListener {
    pub fn bind(addr: &SocketAddr, handle: &Handle) -> io::Result<KcpListener> {
        let udp = UdpSocket::bind(addr, handle).unwrap();
        let listener = KcpListener {
            udp: Arc::new(udp),
            connections: HashMap::new(),
            handle: handle.remote().clone(),
        };
        Ok(listener)
    }

    pub fn accept(&mut self) -> io::Result<(KcpStream, SocketAddr)> {
        let mut buf = vec![0; 1024];
        loop {
            match self.udp.recv_from(&mut buf) {
                Err(e) => {
                    return Err(e);
                }
                Ok((n, addr)) => {
                    if self.connections.contains_key(&addr) {
                        if let Some(kp) = self.connections.get(&addr) {
                            let data = Vec::from(&buf[..n]);
                            let mut kcp = kp.k.borrow_mut();
                            kcp.input(&buf[..n]);
                            kp.set_readiness.set_readiness(mio::Ready::readable());
                        }
                    } else {
                        let conv = LittleEndian::read_u32(&buf[..4]);
                        let dur = Duration::from_millis(10);
                        let mut kcp = KCP::new(conv);
                        kcp.wndsize(128, 128);
                        kcp.nodelay(0, 10, 0, true);
                        let kcp = Arc::new(RefCell::new(kcp));
                        let (registration, set_readiness) = Registration::new2();
                        let core = KcpCore {
                            kcp: kcp.clone(),
                            udp: self.udp.clone(),
                            peer: addr.clone(),
                            registration: registration,
                            set_readiness: set_readiness.clone(),
                        };
                        let io = PollEvented::new(core, &self.handle.handle().unwrap()).unwrap();
                        let stream = KcpStream { io: io };
                        stream.foo(&self.handle, kcp.clone());
                        stream.io.get_ref().kcp.borrow_mut().input(&buf[..n]);
                        stream.io.get_ref().set_readiness.set_readiness(
                            mio::Ready::readable(),
                        );
                        let kp = KcpPair {
                            k: kcp.clone(),
                            set_readiness: set_readiness.clone(),
                        };
                        self.connections.insert(addr, kp);
                        return Ok((stream, addr));
                    }
                }
            }
        }
    }

    pub fn incoming(self) -> Incoming {
        Incoming { inner: self }
    }
}

impl Stream for Incoming {
    type Item = (KcpStream, SocketAddr);
    type Error = io::Error;

    fn poll(&mut self) -> Poll<Option<Self::Item>, io::Error> {
        Ok(Async::Ready(Some(try_nb!(self.inner.accept()))))
    }
}

struct Server {
    socket: Arc<UdpSocket>,
    buf: Vec<u8>,
    to_send: Option<(usize, SocketAddr)>,
    kcp: Arc<RefCell<KCP>>,
    set_readiness: SetReadiness,
}

impl Future for Server {
    type Item = ();
    type Error = io::Error;

    fn poll(&mut self) -> Poll<(), io::Error> {
        loop {
            if let Some((size, peer)) = self.to_send {
                self.kcp.borrow_mut().input(&self.buf[..size]);
                self.set_readiness.set_readiness(mio::Ready::readable());
                self.to_send = None;
            }

            self.to_send = Some(try_nb!(self.socket.recv_from(&mut self.buf)));
        }
    }
}

pub struct KcpStreamNew {
    inner: Option<KcpStream>,
}

impl Future for KcpStreamNew {
    type Item = KcpStream;
    type Error = io::Error;

    fn poll(&mut self) -> Poll<KcpStream, io::Error> {
        Ok(Async::Ready(self.inner.take().unwrap()))
    }
}

struct KcpCore {
    kcp: Arc<RefCell<KCP>>,
    udp: Arc<UdpSocket>,
    peer: SocketAddr,
    registration: Registration,
    set_readiness: SetReadiness,
}

impl Read for KcpCore {
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        let result = self.kcp.borrow_mut().recv(buf);
        match result {
            Err(e) => Err(io::Error::new(io::ErrorKind::WouldBlock, "would block")),
            Ok(n) => Ok(n),
        }
    }
}

impl Write for KcpCore {
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        self.kcp.borrow_mut().send(buf)
    }

    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}

impl Evented for KcpCore {
    fn register(
        &self,
        poll: &mio::Poll,
        token: Token,
        interest: Ready,
        opts: PollOpt,
    ) -> io::Result<()> {
        self.registration.register(poll, token, interest, opts)
    }

    fn reregister(
        &self,
        poll: &mio::Poll,
        token: Token,
        interest: Ready,
        opts: PollOpt,
    ) -> io::Result<()> {
        self.registration.reregister(poll, token, interest, opts)
    }

    fn deregister(&self, poll: &mio::Poll) -> io::Result<()> {
        self.registration.deregister(poll)
    }
}

pub struct KcpStream {
    io: PollEvented<KcpCore>,
}

impl KcpStream {
    pub fn connect(addr: &SocketAddr, handle: &Handle) -> KcpStreamNew {
        let dur = Duration::from_millis(10);
        let conv = rand::random::<u32>();
        let mut kcp = KCP::new(conv);
        kcp.wndsize(128, 128);
        kcp.nodelay(0, 10, 0, true);
        let kcp = Arc::new(RefCell::new(kcp));
        let r: SocketAddr = "127.0.0.1:0".parse().unwrap();
        let udp = UdpSocket::bind(&r, handle).unwrap();
        let udp = Arc::new(udp);
        let (registration, set_readiness) = Registration::new2();
        let core = KcpCore {
            kcp: kcp.clone(),
            udp: udp.clone(),
            peer: addr.clone(),
            registration: registration,
            set_readiness: set_readiness.clone(),
        };
        let io = PollEvented::new(core, handle).unwrap();
        let inner = KcpStream { io: io };
        inner.bar(handle, kcp.clone());
        handle.spawn(
            Server {
                socket: udp.clone(),
                buf: vec![0; 1024],
                to_send: None,
                kcp: kcp.clone(),
                set_readiness: set_readiness.clone(),
            }.then(|_| Ok(())),
        );
        KcpStreamNew { inner: Some(inner) }
    }

    pub fn bar(&self, handle: &Handle, kcp: Arc<RefCell<KCP>>) {
        let dur = Duration::from_millis(10);
        let interval = Interval::new(dur, handle).unwrap();
        let mut output = KcpOutput {
            udp: self.io.get_ref().udp.clone(),
            peer: self.io.get_ref().peer.clone(),
        };
        let updater = interval.for_each(move |()| {
                            kcp.borrow_mut().update(clock(), &mut output);
                            Ok(())
                        });
        handle.spawn(updater.then(|_| Ok(())));
    }

    pub fn foo(&self, handle: &Remote, kcp: Arc<RefCell<KCP>>) {
        let dur = Duration::from_millis(10);
        let h = handle.handle().unwrap();
        let interval = Interval::new(dur, &h).unwrap();
        let mut output = KcpOutput {
            udp: self.io.get_ref().udp.clone(),
            peer: self.io.get_ref().peer.clone(),
        };
        let updater = interval.for_each(move |()| {
                            kcp.borrow_mut().update(clock(), &mut output);
                            Ok(())
                        });
        handle.handle().unwrap().spawn(updater.then(|_| Ok(())));
    }

    pub fn poll_read(&self) -> Async<()> {
        self.io.poll_read()
    }

    pub fn poll_write(&self) -> Async<()> {
        self.io.poll_write()
    }

    pub fn shutdown(&self, how: Shutdown) -> io::Result<()> {
        unimplemented!()
    }
}

impl Read for KcpStream {
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        self.io.read(buf)
    }
}

impl Write for KcpStream {
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        self.io.get_ref().set_readiness.set_readiness(
            mio::Ready::writable(),
        );
        self.io.write(buf)
    }
    fn flush(&mut self) -> io::Result<()> {
        self.io.flush()
    }
}

impl AsyncRead for KcpStream {
    unsafe fn prepare_uninitialized_buffer(&self, _: &mut [u8]) -> bool {
        false
    }

    fn read_buf<B: BufMut>(&mut self, buf: &mut B) -> Poll<usize, io::Error> {
        <&KcpStream>::read_buf(&mut &*self, buf)
    }
}

impl AsyncWrite for KcpStream {
    fn shutdown(&mut self) -> Poll<(), io::Error> {
        <&KcpStream>::shutdown(&mut &*self)
    }

    fn write_buf<B: Buf>(&mut self, buf: &mut B) -> Poll<usize, io::Error> {
        <&KcpStream>::write_buf(&mut &*self, buf)
    }
}

impl<'a> Read for &'a KcpStream {
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        Ok(0)
    }
}

impl<'a> Write for &'a KcpStream {
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        Ok(0)
    }

    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}

impl<'a> AsyncRead for &'a KcpStream {
    unsafe fn prepare_uninitialized_buffer(&self, _: &mut [u8]) -> bool {
        false
    }

    fn read_buf<B: BufMut>(&mut self, buf: &mut B) -> Poll<usize, io::Error> {
        Ok(Async::NotReady)
    }
}

impl<'a> AsyncWrite for &'a KcpStream {
    fn shutdown(&mut self) -> Poll<(), io::Error> {
        Ok(().into())
    }

    fn write_buf<B: Buf>(&mut self, buf: &mut B) -> Poll<usize, io::Error> {
        Ok(Async::NotReady)
    }
}

#[inline]
fn clock() -> u32 {
    let timespec = ctime::get_time();
    let mills = timespec.sec * 1000 + timespec.nsec as i64 / 1000 / 1000;
    mills as u32
}

pub struct KcpOutput {
    udp: Arc<UdpSocket>,
    peer: SocketAddr,
}

impl Write for KcpOutput {
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        self.udp.send_to(buf, &self.peer)
    }

    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}
