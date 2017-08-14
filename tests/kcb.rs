extern crate bytes;
extern crate kcp;
extern crate rand;
extern crate time as ctime;

use std::cell::RefCell;
use std::collections::VecDeque;
use std::io::{self, Read, Write};
use std::iter::Iterator;
use std::rc::Rc;
use std::thread;
use std::time;

use bytes::{ByteOrder, LittleEndian};
use kcp::Kcb;

#[inline]
fn clock() -> u32 {
    let timespec = ctime::get_time();
    let mills = timespec.sec * 1000 + timespec.nsec as i64 / 1000 / 1000;
    mills as u32
}

#[derive(Default)]
struct DelayPacket {
    data: Vec<u8>,
    ts: u32,
}

struct LatencySimulator {
    tx: u32,
    current: u32,
    lost_rate: u32,
    rtt_min: u32,
    rtt_max: u32,
    nmax: u32,
    delay_tunnel: VecDeque<DelayPacket>,
    rng: Random,
}

impl LatencySimulator {
    fn new(lost_rate: u32, rtt_min: u32, rtt_max: u32, nmax: u32) -> LatencySimulator {
        LatencySimulator {
            tx: 0,
            current: clock(),
            lost_rate: lost_rate / 2,
            rtt_min: rtt_min / 2,
            rtt_max: rtt_max / 2,
            nmax: nmax,
            delay_tunnel: VecDeque::new(),
            rng: Random::new(100),
        }
    }
}

impl Write for LatencySimulator {
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        self.tx += 1;
        if self.rng.uniform() < self.lost_rate {
            return Err(io::Error::new(io::ErrorKind::Other, "lost"));
        }
        if self.delay_tunnel.len() >= self.nmax as usize {
            return Err(io::Error::new(
                io::ErrorKind::Other,
                format!("exceeded nmax: {}", self.delay_tunnel.len()),
            ));
        }

        self.current = clock();
        let mut delay = self.rtt_min;
        if self.rtt_max > self.rtt_min {
            delay += rand::random::<u32>() % (self.rtt_max - self.rtt_min);
        }
        let pkt = DelayPacket {
            ts: self.current + delay,
            data: buf.to_vec(),
        };
        self.delay_tunnel.push_back(pkt);

        Ok(buf.len())
    }

    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}

impl Read for LatencySimulator {
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        let len: usize;
        if let Some(pkt) = self.delay_tunnel.front() {
            self.current = clock();
            if self.current < pkt.ts {
                return Err(io::Error::new(
                    io::ErrorKind::Other,
                    format!("current({}) < ts({})", self.current, pkt.ts),
                ));
            }
            len = pkt.data.len();
            if len > buf.len() {
                return Err(io::Error::new(
                    io::ErrorKind::Other,
                    format!("buf_size({}) < pkt_size({})", buf.len(), len),
                ));
            }
            let buf = &mut buf[..len];
            buf.copy_from_slice(&pkt.data[..]);
        } else {
            return Err(io::Error::new(io::ErrorKind::Other, "empty"));
        }

        self.delay_tunnel.pop_front();
        Ok(len)
    }
}

struct Output {
    ls: Rc<RefCell<LatencySimulator>>,
}

impl Write for Output {
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        let mut ls = self.ls.borrow_mut();
        ls.write(buf)
    }

    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}

#[test]
fn kcb_tests() {
    let tests = vec!["default", "normal", "fast"];
    let results = tests.into_iter().map(|t| test(t)).collect::<Vec<_>>();
    for result in results {
        println!("{}", result);
    }
}

fn test(mode: &str) -> String {
    let alice_to_bob = Rc::new(RefCell::new(LatencySimulator::new(10, 60, 125, 1000)));
    let bob_to_alice = Rc::new(RefCell::new(LatencySimulator::new(10, 60, 125, 1000)));

    let mut alice = Kcb::new(0x11223344, Output { ls: alice_to_bob.clone() });
    let mut bob = Kcb::new(0x11223344, Output { ls: bob_to_alice.clone() });

    let mut current = clock();
    let mut slap = current + 20;
    let mut index: u32 = 0;
    let mut next: u32 = 0;
    let mut sumrtt: i64 = 0;
    let mut count = 0;
    let mut maxrtt = 0;

    alice.wndsize(128, 128);
    bob.wndsize(128, 128);

    match mode {
        "default" => {
            alice.nodelay(0, 10, 0, false);
            bob.nodelay(0, 10, 0, false);
        }
        "normal" => {
            alice.nodelay(0, 10, 0, true);
            bob.nodelay(0, 10, 0, true);
        }
        "fast" => {
            alice.nodelay(1, 10, 2, true);
            bob.nodelay(1, 10, 2, true);
        }
        _ => {}
    };

    let mut buffer: [u8; 2000] = [0; 2000];
    let mut ts1 = clock();

    'outer: loop {
        thread::sleep(time::Duration::from_millis(1));
        current = clock();

        alice.update(clock());
        bob.update(clock());

        while current >= slap {
            let mut p: usize = 0;
            LittleEndian::write_u32(&mut buffer[p..p + 4], index);
            p += 4;
            index += 1;
            LittleEndian::write_u32(&mut buffer[p..p + 4], current);
            p += 4;
            alice.send(&buffer[..p]).unwrap();
            slap += 20;
        }

        let mut a2b = alice_to_bob.borrow_mut();
        loop {
            match a2b.read(&mut buffer[..]) {
                Ok(hr) => {
                    bob.input(&buffer[..hr]).ok();
                }
                Err(_) => break,
            };
        }

        let mut b2a = bob_to_alice.borrow_mut();
        loop {
            match b2a.read(&mut buffer[..]) {
                Ok(hr) => {
                    alice.input(&buffer[..hr]).ok();
                }
                Err(_) => break,
            }
        }

        loop {
            match bob.recv(&mut buffer[..10]) {
                Ok(hr) => {
                    bob.send(&buffer[..hr]).unwrap();
                }
                Err(_) => break,
            }
        }

        loop {
            match alice.recv(&mut buffer[..10]) {
                Ok(_) => {
                    let mut p: usize = 0;
                    let sn = LittleEndian::read_u32(&buffer[p..p + 4]);
                    p += 4;
                    let ts = LittleEndian::read_u32(&buffer[p..p + 4]);
                    let rtt = current - ts;
                    if sn != next {
                        println!("ERROR sn {}<->{}", count, next);
                        return "err".to_string();
                    }

                    next += 1;
                    sumrtt += rtt as i64;
                    count += 1;
                    if rtt > maxrtt {
                        maxrtt = rtt;
                    }
                    println!("[RECV] mode={} sn={} rtt={}", mode, sn, rtt);

                    if next > 1000 {
                        break 'outer;
                    }
                }
                Err(_) => break,
            }
        }
    }

    ts1 = clock() - ts1;
    format!("{} mode result ({}ms):\n", mode, ts1) +
        &format!("avgrtt={} maxrtt={}", sumrtt / count, maxrtt)
}

struct Random {
    size: usize,
    seeds: Vec<u32>,
}

impl Random {
    fn new(n: usize) -> Random {
        Random {
            size: 0,
            seeds: vec![0; n],
        }
    }

    fn uniform(&mut self) -> u32 {
        if self.seeds.len() == 0 {
            return 0;
        }
        if self.size == 0 {
            for (i, e) in self.seeds.iter_mut().enumerate() {
                *e = i as u32;
            }
            self.size = self.seeds.len();
        }
        let i = rand::random::<usize>() % self.size;
        let x = self.seeds[i];
        self.size -= 1;
        self.seeds[i] = self.seeds[self.size];
        x
    }
}
