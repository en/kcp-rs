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
        println!("send packet ts: {} len: {}", pkt.ts, buf.len());
        self.0.push_back(pkt);

        Ok(buf.len())
    }

    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}

impl Read for Route {
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        let pkt = self.0.pop_front().unwrap();
        println!("packet ts: {}", pkt.ts);
        let buf = &mut buf[..pkt.data.len()];
        buf.copy_from_slice(&pkt.data[..]);
        Ok(0)
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

    // TODO: rm
    let start = clock();

    let mut current = clock();
    let mut slap = current + 20;
    let mut buffer: [u8; 2000] = [0; 2000];
    let mut index: u32 = 0;

    loop {
        thread::sleep(time::Duration::from_millis(1));
        current = clock();

        alice.update(clock());
        bob.update(clock());

        if current >= slap {
            let mut p: usize = 0;
            encode32u(&mut buffer[..], &mut p, index);
            encode32u(&mut buffer[..], &mut p, current);
            index += 1;
            alice.send(&mut buffer[..8]);
            slap += 20;
        }




        // TODO: rm
        let now = clock();
        if now - start > 1000 {
            break;
        }
    }

    let r = pn.alice_to_bob.clone();
    let mut r = r.lock().unwrap();
    let mut output: [u8; 2048] = [0; 2048];
    r.read(&mut output[..]);
    println!("output: {:?}", &output[..512]);
    let mut p: usize = 0;
    let n = decode32u(&output, &mut p);
    let t = decode32u(&output, &mut p);
    println!("index: {}, current: {}", n, t);

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
