extern crate futures;
extern crate tokio_core;

pub use kcp::KCP;

mod kcp;

use std::io;
use std::net::{SocketAddr, Shutdown};

use futures::Async;
use tokio_core::io::IoFuture;
use tokio_core::reactor::{Handle, PollEvented};

pub struct KcpStream {
    io: PollEvented<u8>,
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
        unimplemented!()
    }

    fn new(// connected_stream: mio::tcp::TcpStream,
           handle: &Handle)
           -> IoFuture<KcpStream> {
        unimplemented!()
    }

    pub fn connect_stream(// stream: net::TcpStream,
                          addr: &SocketAddr,
                          handle: &Handle)
                          -> IoFuture<KcpStream> {
        unimplemented!()
    }

    pub fn poll_read(&self) -> Async<()> {
        unimplemented!()
    }

    pub fn poll_write(&self) -> Async<()> {
        unimplemented!()
    }

    pub fn local_addr(&self) -> io::Result<SocketAddr> {
        unimplemented!()
    }

    pub fn peer_addr(&self) -> io::Result<SocketAddr> {
        unimplemented!()
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
        unimplemented!()
    }

    pub fn ttl(&self) -> io::Result<u32> {
        unimplemented!()
    }
}
