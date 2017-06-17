extern crate kcp;
extern crate time as ctime;
extern crate rand;

use std::collections::VecDeque;
use std::io::{self, Read, Write};
use std::iter::Iterator;
use std::mem;
use std::thread;
use std::time;

use kcp::KCP;

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

impl DelayPacket {
    fn new() -> DelayPacket {
        Default::default()
    }
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

#[test]
fn kcp_test() {
    let tests = vec!["default", "normal", "fast"];
    let results = tests.into_iter().map(|t| test(t)).collect::<Vec<_>>();
    for result in results {
        println!("{}", result);
    }
}

fn test(mode: &str) -> String {
    let mut alice_to_bob = LatencySimulator::new(10, 60, 125, 1000);
    let mut bob_to_alice = LatencySimulator::new(10, 60, 125, 1000);

    let mut alice = KCP::new(0x11223344);
    let mut bob = KCP::new(0x11223344);

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

        alice.update(clock(), &mut alice_to_bob);
        bob.update(clock(), &mut bob_to_alice);

        while current >= slap {
            let mut p: usize = 0;
            encode32u(&mut buffer[..], &mut p, index);
            index += 1;
            encode32u(&mut buffer[..], &mut p, current);
            alice.send(&buffer[..p]).unwrap();
            slap += 20;
        }

        loop {
            match alice_to_bob.read(&mut buffer[..]) {
                Ok(hr) => {
                    bob.input(&buffer[..hr]).ok();
                }
                Err(_) => break,
            };
        }

        loop {
            match bob_to_alice.read(&mut buffer[..]) {
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
                    let sn = decode32u(&buffer, &mut p);
                    let ts = decode32u(&buffer, &mut p);
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

#[test]
fn test_decode32u() {
    let mut buf = vec![0u8; 16];
    let mut p = 0;

    let n = decode32u(&buf, &mut p);
    assert_eq!(0, n);
    assert_eq!(4, p);

    buf[4] = 7;
    let n = decode32u(&buf, &mut p);
    assert_eq!(7, n);
    assert_eq!(8, p);

    buf[8..12].copy_from_slice(&[13u8, 1, 0, 0]);
    let n = decode32u(&buf, &mut p);
    assert_eq!(256 + 13, n);
    assert_eq!(12, p);
}

#[test]
fn test_encode32u() {
    let mut buf = vec![0u8; 4];
    let mut p = 0;
    encode32u(&mut buf, &mut p, 256 + 13);

    assert_eq!(vec![13, 1, 0, 0], buf);
    assert_eq!(4, p);
}

#[inline]
fn decode32u(buf: &[u8], p: &mut usize) -> u32 {
    let buf = &buf[*p..];
    let n: u32 = unsafe { mem::transmute([buf[0], buf[1], buf[2], buf[3]]) };
    *p += 4;
    u32::from_le(n)
}

#[inline]
fn encode32u(buf: &mut [u8], p: &mut usize, n: u32) {
    let n = n.to_le();
    let data: [u8; 4] = unsafe { mem::transmute(n) };
    let buf = &mut buf[*p..];
    buf[..data.len()].copy_from_slice(&data);
    *p += 4;
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
