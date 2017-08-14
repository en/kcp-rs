extern crate bytes;
extern crate futures;
extern crate iovec;
extern crate mio;
extern crate rand;
extern crate time;
extern crate time as ctime;
#[macro_use]
extern crate tokio_core;
extern crate tokio_io;

mod kcb;
mod kcp;

pub use self::kcb::Kcb;
pub use self::kcp::{KcpStream, KcpStreamNew};
pub use self::kcp::{KcpListener, Incoming};
