use std::cmp;
use std::collections::VecDeque;
use std::io::{self, Error, ErrorKind, Write};

const KCP_RTO_NDL: u32 = 30; // no delay min rto
const KCP_RTO_MIN: u32 = 100; // normal min rto
const KCP_RTO_DEF: u32 = 200;
const KCP_RTO_MAX: u32 = 60000;
const KCP_CMD_PUSH: u32 = 81; // cmd: push data
const KCP_CMD_ACK: u32 = 82; // cmd: ack
const KCP_CMD_WASK: u32 = 83; // cmd: window probe (ask)
const KCP_CMD_WINS: u32 = 84; // cmd: window size (tell)
const KCP_ASK_SEND: u32 = 1; // need to send KCP_CMD_WASK
const KCP_ASK_TELL: u32 = 2; // need to send KCP_CMD_WINS
const KCP_WND_SND: u32 = 32;
const KCP_WND_RCV: u32 = 32;
const KCP_MTU_DEF: u32 = 1400;
// const KCP_ACK_FAST: u32 = 3; // never used
const KCP_INTERVAL: u32 = 100;
const KCP_OVERHEAD: u32 = 24;
// const KCP_DEADLINK: u32 = 20; // never used
const KCP_THRESH_INIT: u32 = 2;
const KCP_THRESH_MIN: u32 = 2;
const KCP_PROBE_INIT: u32 = 7000; // 7 secs to probe window size
const KCP_PROBE_LIMIT: u32 = 120000; // up to 120 secs to probe window

#[derive(Default)]
struct Segment {
    conv: u32,
    cmd: u32,
    frg: u32,
    wnd: u32,
    ts: u32,
    sn: u32,
    una: u32,
    resendts: u32,
    rto: u32,
    fastack: u32,
    xmit: u32,
    data: Vec<u8>,
}

impl Segment {
    fn new(size: usize) -> Segment {
        Segment {
            data: Vec::with_capacity(size),
            ..Default::default()
        }
    }
}

pub struct KCP<W: Write> {
    conv: u32,
    mtu: u32,
    mss: u32,
    // state: u32, // never used
    snd_una: u32,
    snd_nxt: u32,
    rcv_nxt: u32,

    // ts_recent: u32, // never used
    // ts_lastack: u32, // never used
    ssthresh: u32,

    rx_rttval: u32,
    rx_srtt: u32,
    rx_rto: u32,
    rx_minrto: u32,

    snd_wnd: u32,
    rcv_wnd: u32,
    rmt_wnd: u32,
    cwnd: u32,
    probe: u32,

    current: u32,
    interval: u32,
    ts_flush: u32,
    xmit: u32,

    nodelay: u32,
    updated: bool,

    ts_probe: u32,
    probe_wait: u32,

    // dead_link: u32, // never used
    incr: u32,

    snd_queue: VecDeque<Segment>,
    rcv_queue: VecDeque<Segment>,
    snd_buf: VecDeque<Segment>,
    rcv_buf: VecDeque<Segment>,

    acklist: Vec<(u32, u32)>,
    buffer: Vec<u8>,

    fastresend: u32,
    nocwnd: bool,
    stream: bool,

    output: W,
}

impl<W: Write> KCP<W> {
    pub fn new(conv: u32, w: W) -> KCP<W> {
        let mut kcp = KCP {
            // state: 0,
            snd_una: 0,
            snd_nxt: 0,
            rcv_nxt: 0,
            // ts_recent: 0,
            // ts_lastack: 0,
            rx_rttval: 0,
            rx_srtt: 0,
            cwnd: 0,
            probe: 0,
            current: 0,
            xmit: 0,
            nodelay: 0,
            updated: false,
            ts_probe: 0,
            probe_wait: 0,
            incr: 0,
            fastresend: 0,
            nocwnd: false,
            stream: false,

            conv: conv,
            snd_wnd: KCP_WND_SND,
            rcv_wnd: KCP_WND_RCV,
            rmt_wnd: KCP_WND_RCV,
            mtu: KCP_MTU_DEF,
            mss: KCP_MTU_DEF - KCP_OVERHEAD,
            buffer: Vec::new(),
            snd_queue: VecDeque::new(),
            rcv_queue: VecDeque::new(),
            snd_buf: VecDeque::new(),
            rcv_buf: VecDeque::new(),
            acklist: Vec::new(),
            rx_rto: KCP_RTO_DEF,
            rx_minrto: KCP_RTO_MIN,
            interval: KCP_INTERVAL,
            ts_flush: KCP_INTERVAL,
            ssthresh: KCP_THRESH_INIT, // dead_link: KCP_DEADLINK,
            output: w,
        };
        kcp.buffer.resize(
            ((KCP_MTU_DEF + KCP_OVERHEAD) * 3) as usize,
            0,
        );
        kcp
    }

    // user/upper level recv: returns size, returns below zero for EAGAIN
    pub fn recv(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        if self.rcv_queue.is_empty() {
            return Err(Error::new(ErrorKind::Other, "EOF"));
        }
        let peeksize = match self.peeksize() {
            Ok(x) => x,
            Err(_) => return Err(Error::new(ErrorKind::UnexpectedEof, "unexpected EOF")),
        };

        if peeksize > buf.len() {
            return Err(Error::new(ErrorKind::InvalidInput, "short buffer"));
        }

        let recover = self.rcv_queue.len() >= self.rcv_wnd as usize;

        // merge fragment
        let mut p: usize = 0;
        let mut index: usize = 0;
        for seg in &self.rcv_queue {
            let l = seg.data.len();
            buf[p..p + l].copy_from_slice(&seg.data[..]);
            p += l;
            index += 1;
            if seg.frg == 0 {
                break;
            }
        }
        if index > 0 {
            let new_rcv_queue = self.rcv_queue.split_off(index);
            self.rcv_queue = new_rcv_queue;
        }
        assert!(p == peeksize);

        // move available data from rcv_buf -> rcv_queue
        index = 0;
        let mut nrcv_que = self.rcv_queue.len();
        for seg in &self.rcv_buf {
            if seg.sn == self.rcv_nxt && nrcv_que < self.rcv_wnd as usize {
                nrcv_que += 1;
                self.rcv_nxt += 1;
                index += 1;
            } else {
                break;
            }
        }

        if index > 0 {
            let new_rcv_buf = self.rcv_buf.split_off(index);
            self.rcv_queue.append(&mut self.rcv_buf);
            self.rcv_buf = new_rcv_buf;
        }

        // fast recover
        if self.rcv_queue.len() < self.rcv_wnd as usize && recover {
            // ready to send back KCP_CMD_WINS in `flush`
            // tell remote my window size
            self.probe |= KCP_ASK_TELL;
        }
        Ok(p)
    }

    // peek data size
    fn peeksize(&self) -> Result<usize, i32> {
        let seg = match self.rcv_queue.front() {
            Some(x) => x,
            None => return Err(-1),
        };
        if seg.frg == 0 {
            return Ok(seg.data.len());
        }
        if self.rcv_queue.len() < (seg.frg + 1) as usize {
            return Err(-1);
        }
        let mut length: usize = 0;
        for seg in &self.rcv_queue {
            length += seg.data.len();
            if seg.frg == 0 {
                break;
            }
        }
        Ok(length)
    }

    // user/upper level send, returns the number of fragments
    pub fn send(&mut self, buf: &[u8]) -> io::Result<usize> {
        let mut n = buf.len();
        if n == 0 {
            return Err(Error::new(ErrorKind::InvalidInput, "no data available"));
        }
        let mut p: usize = 0;

        // append to previous segment in streaming mode (if possible)
        if self.stream {
            if let Some(seg) = self.snd_queue.back_mut() {
                let l = seg.data.len();
                if l < self.mss as usize {
                    let capacity = self.mss as usize - l;
                    let extend = cmp::min(n, capacity);
                    seg.data.extend_from_slice(&buf[p..p + extend]);
                    seg.frg = 0;
                    p += extend;
                    n -= extend;
                    if n == 0 {
                        return Ok(1);
                    }
                }
            };
        }

        let count = if n <= self.mss as usize {
            1
        } else {
            (n + self.mss as usize - 1) / self.mss as usize
        };

        if count > 255 {
            return Err(Error::new(ErrorKind::InvalidInput, "data too long"));
        }
        assert!(count > 0);

        // fragment
        for i in 0..count {
            let size = cmp::min(self.mss as usize, n);
            let mut seg = Segment::new(size);
            seg.data.extend_from_slice(&buf[p..p + size]);
            seg.frg = if !self.stream {
                (count - i - 1) as u32
            } else {
                0
            };
            self.snd_queue.push_back(seg);

            p += size;
            n -= size;
        }
        Ok(p)
    }

    fn update_ack(&mut self, rtt: u32) {
        if self.rx_srtt == 0 {
            self.rx_srtt = rtt;
            self.rx_rttval = rtt / 2;
        } else {
            let delta = if rtt > self.rx_srtt {
                rtt - self.rx_srtt
            } else {
                self.rx_srtt - rtt
            };
            self.rx_rttval = (3 * self.rx_rttval + delta) / 4;
            self.rx_srtt = (7 * self.rx_srtt + rtt) / 8;
            if self.rx_srtt < 1 {
                self.rx_srtt = 1;
            }
        }
        let rto = self.rx_srtt + cmp::max(self.interval, 4 * self.rx_rttval);
        self.rx_rto = bound(self.rx_minrto, rto, KCP_RTO_MAX);
    }

    #[inline]
    fn shrink_buf(&mut self) {
        self.snd_una = match self.snd_buf.front() {
            Some(seg) => seg.sn,
            None => self.snd_nxt,
        };
    }

    fn parse_ack(&mut self, sn: u32) {
        if sn < self.snd_una || sn >= self.snd_nxt {
            return;
        }
        for i in 0..self.snd_buf.len() {
            if sn == self.snd_buf[i].sn {
                self.snd_buf.remove(i);
                break;
            } else if sn < self.snd_buf[i].sn {
                break;
            }
        }
    }

    fn parse_una(&mut self, una: u32) {
        let mut index: usize = 0;
        for seg in &self.snd_buf {
            if una > seg.sn {
                index += 1;
            } else {
                break;
            }
        }
        if index > 0 {
            let new_snd_buf = self.snd_buf.split_off(index);
            self.snd_buf = new_snd_buf;
        }
    }

    fn parse_fastack(&mut self, sn: u32) {
        if sn < self.snd_una || sn >= self.snd_nxt {
            return;
        }
        for seg in &mut self.snd_buf {
            if sn < seg.sn {
                break;
            } else if sn != seg.sn {
                seg.fastack += 1;
            }
        }
    }

    fn parse_data(&mut self, newseg: Segment) {
        let sn = newseg.sn;
        if sn >= self.rcv_nxt + self.rcv_wnd || sn < self.rcv_nxt {
            // ikcp_segment_delete(kcp, newseg);
            return;
        }

        let mut repeat = false;
        let mut index: usize = self.rcv_buf.len();
        for seg in self.rcv_buf.iter().rev() {
            if sn == seg.sn {
                repeat = true;
                break;
            } else if sn > seg.sn {
                break;
            }
            index -= 1;
        }

        if !repeat {
            self.rcv_buf.insert(index, newseg);
        } else {
            // ikcp_segment_delete(kcp, newseg);
        }

        // move available data from rcv_buf -> rcv_queue
        index = 0;
        let mut nrcv_que = self.rcv_queue.len();
        for seg in &self.rcv_buf {
            if seg.sn == self.rcv_nxt && nrcv_que < self.rcv_wnd as usize {
                nrcv_que += 1;
                self.rcv_nxt += 1;
                index += 1;
            } else {
                break;
            }
        }
        if index > 0 {
            let new_rcv_buf = self.rcv_buf.split_off(index);
            self.rcv_queue.append(&mut self.rcv_buf);
            self.rcv_buf = new_rcv_buf;
        }
    }

    pub fn input(&mut self, buf: &[u8]) -> io::Result<usize> {
        let mut n = buf.len();
        if n < KCP_OVERHEAD as usize {
            return Err(Error::new(ErrorKind::InvalidData, "invalid data"));
        }
        let old_una = self.snd_una;
        let mut p: usize = 0;
        let mut flag = false;
        let mut maxack: u32 = 0;
        loop {
            if n < KCP_OVERHEAD as usize {
                break;
            }

            let conv = decode32u(buf, &mut p);
            if conv != self.conv {
                return Err(Error::new(ErrorKind::InvalidData, "invalid data"));
            }

            let cmd = decode8u(buf, &mut p);
            let frg = decode8u(buf, &mut p);
            let wnd = decode16u(buf, &mut p);
            let ts = decode32u(buf, &mut p);
            let sn = decode32u(buf, &mut p);
            let una = decode32u(buf, &mut p);
            let len = decode32u(buf, &mut p);

            n -= KCP_OVERHEAD as usize;

            let len = len as usize;
            if n < len {
                return Err(Error::new(ErrorKind::UnexpectedEof, "unexpected EOF"));
            }

            let cmd = cmd as u32;
            if cmd != KCP_CMD_PUSH && cmd != KCP_CMD_ACK && cmd != KCP_CMD_WASK &&
                cmd != KCP_CMD_WINS
            {
                return Err(Error::new(ErrorKind::InvalidData, "invalid data"));
            }

            self.rmt_wnd = wnd as u32;
            self.parse_una(una);
            self.shrink_buf();
            if cmd == KCP_CMD_ACK {
                let rtt = timediff(self.current, ts);
                if rtt >= 0 {
                    self.update_ack(rtt as u32);
                }
                self.parse_ack(sn);
                self.shrink_buf();
                if !flag {
                    flag = true;
                    maxack = sn;
                } else {
                    if sn > maxack {
                        maxack = sn;
                    }
                }
            } else if cmd == KCP_CMD_PUSH {
                if sn < self.rcv_nxt + self.rcv_wnd {
                    self.acklist.push((sn, ts));
                    if sn >= self.rcv_nxt {
                        let mut seg = Segment::new(len);
                        seg.conv = conv;
                        seg.cmd = cmd;
                        seg.frg = frg as u32;
                        seg.wnd = wnd as u32;
                        seg.ts = ts;
                        seg.sn = sn;
                        seg.una = una;
                        seg.data.extend_from_slice(&buf[p..p + len]);
                        self.parse_data(seg);
                    }
                }
            } else if cmd == KCP_CMD_WASK {
                // ready to send back KCP_CMD_WINS in `flush`
                // tell remote my window size
                self.probe |= KCP_ASK_TELL;
            } else if cmd == KCP_CMD_WINS {
                // do nothing
            } else {
                return Err(Error::new(ErrorKind::InvalidData, "invalid data"));
            }

            p += len;
            n -= len;
        }
        if flag {
            self.parse_fastack(maxack);
        }

        if self.snd_una > old_una {
            if self.cwnd < self.rmt_wnd {
                let mss = self.mss;
                if self.cwnd < self.ssthresh {
                    self.cwnd += 1;
                    self.incr += mss;
                } else {
                    if self.incr < mss {
                        self.incr = mss;
                    }
                    self.incr += (mss * mss) / self.incr + (mss / 16);
                    if (self.cwnd + 1) * mss <= self.incr {
                        self.cwnd += 1;
                    }
                }
                if self.cwnd > self.rmt_wnd {
                    self.cwnd = self.rmt_wnd;
                    self.incr = self.rmt_wnd * mss;
                }
            }
        }
        Ok(p)
    }

    fn wnd_unused(&self) -> u32 {
        let nrcv_que = self.rcv_queue.len() as u32;
        if nrcv_que < self.rcv_wnd {
            return self.rcv_wnd - nrcv_que;
        }
        0
    }

    pub fn flush(&mut self) {
        // `update` haven't been called.
        if !self.updated {
            return;
        }
        let current = self.current;
        let mut lost = false;
        let mut change = false;
        let mut p: usize = 0;
        let mut seg = Segment::new(0);

        seg.conv = self.conv;
        seg.cmd = KCP_CMD_ACK;
        seg.wnd = self.wnd_unused();
        seg.una = self.rcv_nxt;

        // flush acknowledges
        for ack in &self.acklist {
            if p as u32 + KCP_OVERHEAD > self.mtu {
                self.output.write_all(&self.buffer[..p]);
                p = 0;
            }
            seg.sn = ack.0;
            seg.ts = ack.1;
            encode_seg(&mut self.buffer, &mut p, &seg);
        }
        self.acklist.clear();

        // probe window size (if remote window size equals zero)
        if self.rmt_wnd == 0 {
            if self.probe_wait == 0 {
                self.probe_wait = KCP_PROBE_INIT;
                self.ts_probe = self.current + self.probe_wait;
            } else {
                if timediff(self.current, self.ts_probe) >= 0 {
                    if self.probe_wait < KCP_PROBE_INIT {
                        self.probe_wait = KCP_PROBE_INIT;
                    }
                    self.probe_wait += self.probe_wait / 2;
                    if self.probe_wait > KCP_PROBE_LIMIT {
                        self.probe_wait = KCP_PROBE_LIMIT;
                    }
                    self.ts_probe = self.current + self.probe_wait;
                    self.probe |= KCP_ASK_SEND;
                }
            }
        } else {
            self.ts_probe = 0;
            self.probe_wait = 0;
        }

        // flush window probing commands
        if (self.probe & KCP_ASK_SEND) != 0 {
            seg.cmd = KCP_CMD_WASK;
            if p as u32 + KCP_OVERHEAD > self.mtu {
                self.output.write_all(&self.buffer[..p]);
                p = 0;
            }
            encode_seg(&mut self.buffer, &mut p, &seg);
        }

        // flush window probing commands
        if (self.probe & KCP_ASK_TELL) != 0 {
            seg.cmd = KCP_CMD_WINS;
            if p as u32 + KCP_OVERHEAD > self.mtu {
                self.output.write_all(&self.buffer[..p]);
                p = 0;
            }
            encode_seg(&mut self.buffer, &mut p, &seg);
        }
        self.probe = 0;

        // calculate window size
        let mut cwnd = cmp::min(self.snd_wnd, self.rmt_wnd);
        if !self.nocwnd {
            cwnd = cmp::min(self.cwnd, cwnd);
        }

        // move data from snd_queue to snd_buf
        while self.snd_nxt < self.snd_una + cwnd {
            if let Some(mut newseg) = self.snd_queue.pop_front() {
                newseg.conv = self.conv;
                newseg.cmd = KCP_CMD_PUSH;
                newseg.wnd = seg.wnd;
                newseg.ts = current;
                newseg.sn = self.snd_nxt;
                self.snd_nxt += 1;
                newseg.una = self.rcv_nxt;
                newseg.resendts = current;
                newseg.rto = self.rx_rto;
                newseg.fastack = 0;
                newseg.xmit = 0;
                self.snd_buf.push_back(newseg);
            } else {
                break;
            }
        }
        // calculate resent
        let resent = if self.fastresend > 0 {
            self.fastresend
        } else {
            u32::max_value()
        };
        let rtomin = if self.nodelay == 0 {
            self.rx_rto >> 3
        } else {
            0
        };

        // flush data segments
        for segment in &mut self.snd_buf {
            let mut needsend = false;
            if segment.xmit == 0 {
                needsend = true;
                segment.xmit += 1;
                segment.rto = self.rx_rto;
                segment.resendts = current + segment.rto + rtomin;
            } else if timediff(current, segment.resendts) >= 0 {
                needsend = true;
                segment.xmit += 1;
                self.xmit += 1;
                if self.nodelay == 0 {
                    segment.rto += self.rx_rto;
                } else {
                    segment.rto += self.rx_rto / 2;
                }
                segment.resendts = current + segment.rto;
                lost = true;
            } else if segment.fastack >= resent {
                needsend = true;
                segment.xmit += 1;
                segment.fastack = 0;
                segment.resendts = current + segment.rto;
                change = true;
            }

            if needsend {
                segment.ts = current;
                segment.wnd = seg.wnd;
                segment.una = self.rcv_nxt;

                let len = segment.data.len();
                let need = KCP_OVERHEAD as usize + len;

                if p + need > self.mtu as usize {
                    self.output.write_all(&self.buffer[..p]);
                    p = 0;
                }
                encode_seg(&mut self.buffer, &mut p, &segment);

                if len > 0 {
                    &self.buffer[p..p + len].copy_from_slice(&segment.data[..]);
                    p += len;
                }

                // never used
                // if segment.xmit >= self.dead_link {
                //     self.state = -1;
                // }
            }
        }

        // flash remain segments
        if p > 0 {
            self.output.write_all(&self.buffer[..p]);
        }

        // update ssthresh
        if change {
            let inflight = self.snd_nxt - self.snd_una;
            self.ssthresh = inflight / 2;
            if self.ssthresh < KCP_THRESH_MIN {
                self.ssthresh = KCP_THRESH_MIN;
            }
            self.cwnd = self.ssthresh + resent;
            self.incr = self.cwnd * self.mss;
        }

        if lost {
            self.ssthresh = cwnd / 2;
            if self.ssthresh < KCP_THRESH_MIN {
                self.ssthresh = KCP_THRESH_MIN;
            }
            self.cwnd = 1;
            self.incr = self.mss;
        }

        if self.cwnd < 1 {
            self.cwnd = 1;
            self.incr = self.mss;
        }
    }

    // update state (call it repeatedly, every 10ms-100ms), or you can ask
    // `check` when to call it again (without `input`/`send` calling).
    // `current` - current timestamp in millisec.
    pub fn update(&mut self, current: u32) {
        self.current = current;
        if !self.updated {
            self.updated = true;
            self.ts_flush = self.current;
        }
        let mut slap = timediff(self.current, self.ts_flush);

        if slap >= 10000 || slap < -10000 {
            self.ts_flush = self.current;
            slap = 0;
        }

        if slap >= 0 {
            self.ts_flush += self.interval;
            if timediff(self.current, self.ts_flush) >= 0 {
                self.ts_flush = self.current + self.interval;
            }
            self.flush();
        }
    }

    // Determine when should you invoke `update`:
    // returns when you should invoke `update` in millisec, if there
    // is no `input`/`send` calling. you can call `update` in that
    // time, instead of call update repeatly.
    // Important to reduce unnacessary `update` invoking. use it to
    // schedule `update` (eg. implementing an epoll-like mechanism,
    // or optimize `update` when handling massive kcp connections)
    pub fn check(&self, current: u32) -> u32 {
        if !self.updated {
            return 0;
        }

        let mut ts_flush = self.ts_flush;
        let mut tm_packet = u32::max_value();

        if timediff(current, ts_flush) >= 10000 || timediff(current, ts_flush) < -10000 {
            ts_flush = current;
        }

        if timediff(current, ts_flush) >= 0 {
            return 0;
        }

        let tm_flush = timediff(ts_flush, current) as u32;
        for seg in &self.snd_buf {
            let diff = timediff(seg.resendts, current);
            if diff <= 0 {
                return 0;
            }
            if (diff as u32) < tm_packet {
                tm_packet = diff as u32;
            }
        }

        let minimal = cmp::min(cmp::min(tm_packet, tm_flush), self.interval);

        minimal
    }

    pub fn setmtu(&mut self, mtu: u32) -> bool {
        if mtu < 50 || mtu < KCP_OVERHEAD {
            return false;
        }
        self.mtu = mtu;
        self.mss = self.mtu - KCP_OVERHEAD;
        self.buffer.resize(((mtu + KCP_OVERHEAD) * 3) as usize, 0);
        true
    }

    pub fn nodelay(&mut self, nodelay: i32, interval: i32, resend: i32, nc: bool) {
        if nodelay >= 0 {
            let nodelay = nodelay as u32;
            self.nodelay = nodelay;
            if nodelay > 0 {
                self.rx_minrto = KCP_RTO_NDL;
            } else {
                self.rx_minrto = KCP_RTO_MIN;
            }
        }
        if interval >= 0 {
            let mut interval = interval as u32;
            if interval > 5000 {
                interval = 5000;
            } else if interval < 10 {
                interval = 10;
            }
            self.interval = interval;
        }
        if resend >= 0 {
            self.fastresend = resend as u32;
        }
        self.nocwnd = nc;
    }

    pub fn wndsize(&mut self, sndwnd: i32, rcvwnd: i32) {
        if sndwnd > 0 {
            self.snd_wnd = sndwnd as u32;
        }
        if rcvwnd > 0 {
            self.rcv_wnd = rcvwnd as u32;
        }
    }

    pub fn waitsnd(&self) -> usize {
        self.snd_buf.len() + self.snd_queue.len()
    }
}

#[inline]
fn timediff(later: u32, earlier: u32) -> i32 {
    later as i32 - earlier as i32
}

#[inline]
fn bound(lower: u32, v: u32, upper: u32) -> u32 {
    cmp::min(cmp::max(lower, v), upper)
}

#[inline]
fn encode8u(buf: &mut [u8], p: &mut usize, n: u8) {
    buf[*p] = n;
    *p += 1;
}

#[inline]
fn decode8u(buf: &[u8], p: &mut usize) -> u8 {
    let n = buf[*p];
    *p += 1;
    n
}

#[inline]
fn encode16u(buf: &mut [u8], p: &mut usize, n: u16) {
    let n = n.to_le();
    buf[*p] = n as u8;
    buf[*p + 1] = (n >> 8) as u8;
    *p += 2;
}

#[inline]
fn decode16u(buf: &[u8], p: &mut usize) -> u16 {
    let n = (buf[*p] as u16) | (buf[*p + 1] as u16) << 8;
    *p += 2;
    u16::from_le(n)
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

#[inline]
fn decode32u(buf: &[u8], p: &mut usize) -> u32 {
    let n = (buf[*p] as u32) | (buf[*p + 1] as u32) << 8 | (buf[*p + 2] as u32) << 16 |
        (buf[*p + 3] as u32) << 24;
    *p += 4;
    u32::from_le(n)
}

fn encode_seg(buf: &mut [u8], p: &mut usize, seg: &Segment) {
    encode32u(buf, p, seg.conv);
    encode8u(buf, p, seg.cmd as u8);
    encode8u(buf, p, seg.frg as u8);
    encode16u(buf, p, seg.wnd as u16);
    encode32u(buf, p, seg.ts);
    encode32u(buf, p, seg.sn);
    encode32u(buf, p, seg.una);
    encode32u(buf, p, seg.data.len() as u32);
}
