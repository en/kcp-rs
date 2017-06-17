extern crate futures;
extern crate rand;
#[macro_use]
extern crate tokio_core;
extern crate time;

pub use kcp::KCP;

mod kcp;

use std::io::{self, Read, Write};
use std::net::{SocketAddr, Shutdown};
use std::str;

use futures::{Future, failed, Poll, Async};
use tokio_core::io::{Io, IoFuture};
use tokio_core::net::UdpSocket;
use tokio_core::reactor::{Handle, PollEvented};

fn clock() -> u32 {
    let timespec = time::get_time();
    let mills = timespec.sec * 1000 + timespec.nsec as i64 / 1000 / 1000;
    mills as u32
}

struct KcpTunnel {
    socket: UdpSocket,
    addr: SocketAddr,
}

impl Write for KcpTunnel {
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        let n = self.socket.send_to(buf, &self.addr).unwrap();
        if n == buf.len() {
            Ok(n)
        } else {
            Err(io::Error::new(io::ErrorKind::WouldBlock, "failed to send"))
        }
    }

    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}

pub struct KcpStream {
    io: UdpSocket,
    kcp: KCP,
    peer_addr: SocketAddr,
}

pub struct KcpStreamNew {
    inner: IoFuture<KcpStream>,
}

enum KcpStreamConnect {
    Waiting(KcpStream),
    Empty,
}

impl KcpStream {
    pub fn connect(addr: &SocketAddr, handle: &Handle) -> KcpStreamNew {
        let any = str::FromStr::from_str("0.0.0.0:0").unwrap();
        let future = match UdpSocket::bind(&any, handle) {
            Ok(udp) => KcpStream::new(udp, addr, handle),
            Err(e) => failed(e).boxed(),
        };
        KcpStreamNew { inner: future }
    }

    fn new(socket: UdpSocket, addr: &SocketAddr, handle: &Handle) -> IoFuture<KcpStream> {
        let conv = rand::random::<u32>();
        let kcp = KCP::new(conv);
        KcpStreamConnect::Waiting(KcpStream {
            io: socket,
            kcp: kcp,
            peer_addr: *addr,
        }).boxed()
    }

    pub fn connect_stream(
        // stream: net::TcpStream,
        addr: &SocketAddr,
        handle: &Handle,
    ) -> IoFuture<KcpStream> {
        unimplemented!()
    }

    pub fn poll_read(&self) -> Async<()> {
        self.io.poll_read()
    }

    pub fn poll_write(&self) -> Async<()> {
        self.io.poll_write()
    }

    pub fn local_addr(&self) -> io::Result<SocketAddr> {
        self.io.local_addr()
    }

    pub fn peer_addr(&self) -> io::Result<SocketAddr> {
        Ok(self.peer_addr)
    }

    pub fn shutdown(&self, how: Shutdown) -> io::Result<()> {
        unimplemented!()
    }

    pub fn set_nodelay(&self, nodelay: bool) -> io::Result<()> {
        unimplemented!()
    }

    pub fn nodelay(&self) -> io::Result<bool> {
        unimplemented!()
    }

    pub fn set_keepalive_ms(&self, keepalive: Option<u32>) -> io::Result<()> {
        unimplemented!()
    }

    pub fn keepalive_ms(&self) -> io::Result<Option<u32>> {
        unimplemented!()
    }

    pub fn set_ttl(&self, ttl: u32) -> io::Result<()> {
        self.io.set_ttl(ttl)
    }

    pub fn ttl(&self) -> io::Result<u32> {
        self.io.ttl()
    }
}

impl Read for KcpStream {
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        // self.io.read(buf)
        unimplemented!()
    }
}

impl Write for KcpStream {
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        // self.io.write(buf)
        unimplemented!()
    }
    fn flush(&mut self) -> io::Result<()> {
        // self.io.flush()
        unimplemented!()
    }
}

impl Io for KcpStream {
    fn poll_read(&mut self) -> Async<()> {
        <KcpStream>::poll_read(self)
    }

    fn poll_write(&mut self) -> Async<()> {
        <KcpStream>::poll_write(self)
    }
}

impl<'a> Read for &'a KcpStream {
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        // (&self.io).read(buf)
        unimplemented!()
    }
}

impl<'a> Write for &'a KcpStream {
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        // (&self.io).write(buf)
        unimplemented!()
    }

    fn flush(&mut self) -> io::Result<()> {
        // (&self.io).flush()
        unimplemented!()
    }
}

impl<'a> Io for &'a KcpStream {
    fn poll_read(&mut self) -> Async<()> {
        <KcpStream>::poll_read(self)
    }

    fn poll_write(&mut self) -> Async<()> {
        <KcpStream>::poll_write(self)
    }
}

impl Future for KcpStreamNew {
    type Item = KcpStream;
    type Error = io::Error;

    fn poll(&mut self) -> Poll<KcpStream, io::Error> {
        self.inner.poll()
    }
}

impl Future for KcpStreamConnect {
    type Item = KcpStream;
    type Error = io::Error;

    fn poll(&mut self) -> futures::Poll<KcpStream, io::Error> {
        unimplemented!()
    }
}
