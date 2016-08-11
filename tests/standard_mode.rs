extern crate kcp;
extern crate time as ctime;

use std::collections::VecDeque;
use std::io;
use std::io::{Read, Write};
use std::sync::{Arc, Mutex};
use std::thread;
use std::time;

use kcp::KCP;

#[inline]
fn clock() -> u32 {
    let timespec = ctime::get_time();
    let mills = timespec.sec * 1000 + timespec.nsec as i64 / 1000 / 1000;
    (mills & 0xffffffff) as u32 // ??
}

#[derive(Default)]
struct DelayPacket {
    data: Vec<u8>,
    ts: u32,
}

impl DelayPacket {
    fn new() -> DelayPacket {
        Default::default()
    }
}

#[derive(Default)]
struct Route(VecDeque<DelayPacket>);

impl Write for Route {
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        let mut pkt = DelayPacket::new();
        pkt.ts = clock(); // TODO
        pkt.data.extend_from_slice(buf);
        self.0.push_back(pkt);

        Ok(buf.len())
    }

    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}

impl Read for Route {
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        if let Some(pkt) = self.0.pop_front() {
            let len = pkt.data.len();
            let buf = &mut buf[..len];
            buf.copy_from_slice(&pkt.data[..]);
            return Ok(len);
        } else {
            Err(io::Error::new(io::ErrorKind::UnexpectedEof, "failed to fill whole buffer"))
        }
    }
}

struct PoorNetwork {
    alice_to_bob: Arc<Mutex<Route>>,
    bob_to_alice: Arc<Mutex<Route>>,
}

impl PoorNetwork {
    fn new() -> PoorNetwork {
        PoorNetwork {
            alice_to_bob: Arc::new(Mutex::new(Route(VecDeque::new()))),
            bob_to_alice: Arc::new(Mutex::new(Route(VecDeque::new()))),
        }
    }
}

#[test]
fn standard_mode() {
    let pn = PoorNetwork::new();

    let alice_sends = pn.alice_to_bob.clone();
    let bob_sends = pn.bob_to_alice.clone();
    let mut alice = KCP::new(0x11223344, alice_sends);
    let mut bob = KCP::new(0x11223344, bob_sends);

    let mut current = clock();
    let mut slap = current + 20;
    let mut index: u32 = 0;
    let mut next: u32 = 0;
    let mut sumrtt: i64 = 0;
    let mut count = 0;
    let mut maxrtt = 0;

    alice.wndsize(128, 128);
    bob.wndsize(128, 128);

    alice.nodelay(0, 10, 0, 0);
    bob.nodelay(0, 10, 0, 0);

    let mut buffer: [u8; 2000] = [0; 2000];
    let mut ts1 = clock();

    loop {
        thread::sleep(time::Duration::from_millis(1));
        current = clock();

        alice.update(clock());
        bob.update(clock());

        while current >= slap {
            let mut p: usize = 0;
            encode32u(&mut buffer[..], &mut p, index);
            index += 1;
            encode32u(&mut buffer[..], &mut p, current);
            alice.send(&buffer[..8]);
            slap += 20;
        }

        let a2b = pn.alice_to_bob.clone();
        let mut a2b = a2b.lock().unwrap();

        let b2a = pn.bob_to_alice.clone();
        let mut b2a = b2a.lock().unwrap();

        loop {
            match a2b.read(&mut buffer[..]) {
                Ok(hr) => {
                    bob.input(&buffer[..hr]);
                }
                Err(_) => break,
            };
        }

        loop {
            match b2a.read(&mut buffer[..]) {
                Ok(hr) => {
                    alice.input(&buffer[..hr]);
                }
                Err(_) => break,
            }
        }

        loop {
            match bob.recv(&mut buffer[..10]) {
                Ok(hr) => {
                    bob.send(&buffer[..hr]);
                }
                Err(_) => break,
            }
        }

        loop {
            match alice.recv(&mut buffer[..10]) {
                Ok(hr) => {
                    let mut p: usize = 0;
                    let sn = decode32u(&buffer, &mut p);
                    let ts = decode32u(&buffer, &mut p);
                    let rtt = current - ts;
                    if sn != next {
                        println!("ERROR sn {}<->{}", count, next);
                        return;
                    }
                    next += 1;
                    sumrtt += rtt as i64;
                    count += 1;
                    if rtt > maxrtt {
                        maxrtt = rtt;
                    }
                    println!("[RECV] sn={} rtt={}", sn, rtt);
                }
                Err(_) => break,
            }
        }
        if next > 1000 {
            break;
        }
    }

    ts1 = clock() - ts1;
    println!("result ({}ms):", ts1);
    println!("avgrtt={} maxrtt={}", sumrtt / count, maxrtt);
}

#[inline]
fn decode32u(buf: &[u8], p: &mut usize) -> u32 {
    let n = (buf[*p] as u32) | (buf[*p + 1] as u32) << 8 | (buf[*p + 2] as u32) << 16 |
            (buf[*p + 3] as u32) << 24;
    *p += 4;
    u32::from_le(n)
}

#[inline]
fn encode32u(buf: &mut [u8], p: &mut usize, n: u32) {
    let n = n.to_le();
    buf[*p] = n as u8;
    buf[*p + 1] = (n >> 8) as u8;
    buf[*p + 2] = (n >> 16) as u8;
    buf[*p + 3] = (n >> 24) as u8;
    *p += 4;
}

fn to_hex_string(bytes: &[u8]) {
    let mut i = 0;
    for b in bytes {
        print!("0x{:02X} ", b);
        i = i + 1;
        if i % 4 == 0 {
            println!("");
        }
    }
}
