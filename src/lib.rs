extern crate tokio_core;

pub use kcp::KCP;

mod kcp;

use std::io;
use tokio_core::io::IoFuture;
use tokio_core::reactor::PollEvented;

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
        KcpStreamNew { inner: future }
    }

    fn new(connected_stream: mio::tcp::TcpStream, handle: &Handle) -> IoFuture<TcpStream> {}

    pub fn connect_stream(stream: net::TcpStream,
                          addr: &SocketAddr,
                          handle: &Handle)
                          -> IoFuture<TcpStream> {
    }

    pub fn poll_read(&self) -> Async<()> {
        self.io.poll_read()
    }

    pub fn poll_write(&self) -> Async<()> {
        self.io.poll_write()
    }

    pub fn local_addr(&self) -> io::Result<SocketAddr> {}

    pub fn peer_addr(&self) -> io::Result<SocketAddr> {}

    pub fn shutdown(&self, how: Shutdown) -> io::Result<()> {}

    pub fn set_nodelay(&self, nodelay: bool) -> io::Result<()> {}

    pub fn nodelay(&self) -> io::Result<bool> {}

    pub fn set_keepalive_ms(&self, keepalive: Option<u32>) -> io::Result<()> {}

    pub fn keepalive_ms(&self) -> io::Result<Option<u32>> {}

    pub fn set_ttl(&self, ttl: u32) -> io::Result<()> {}

    pub fn ttl(&self) -> io::Result<u32> {}
}
