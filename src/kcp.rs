use std::cmp;
use std::collections::VecDeque;
use std::io;

const KCP_RTO_NDL: u32 = 30;		// no delay min rto
const KCP_RTO_MIN: u32 = 100;		// normal min rto
const KCP_RTO_DEF: u32 = 200;
const KCP_RTO_MAX: u32 = 60000;
const KCP_CMD_PUSH: u32 = 81;		// cmd: push data
const KCP_CMD_ACK: u32 = 82;		// cmd: ack
const KCP_CMD_WASK: u32 = 83;		// cmd: window probe (ask)
const KCP_CMD_WINS: u32 = 84;		// cmd: window size (tell)
const KCP_ASK_SEND: u32 = 1;		// need to send KCP_CMD_WASK
const KCP_ASK_TELL: u32 = 2;		// need to send KCP_CMD_WINS
const KCP_WND_SND: u32 = 32;
const KCP_WND_RCV: u32 = 32;
const KCP_MTU_DEF: u32 = 1400;
const KCP_ACK_FAST: u32 = 3;
const KCP_INTERVAL: u32 = 100;
const KCP_OVERHEAD: u32 = 24;
const KCP_DEADLINK: u32 = 20;
const KCP_THRESH_INIT: u32 = 2;
const KCP_THRESH_MIN: u32 = 2;
const KCP_PROBE_INIT: u32 = 7000;		// 7 secs to probe window size
const KCP_PROBE_LIMIT: u32 = 120000;	// up to 120 secs to probe window

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
        Segment { data: Vec::with_capacity(size), ..Default::default() }
    }
}

pub type OutputFn = fn(&[u8]) -> io::Result<usize>;

#[derive(Default)]
pub struct KCP {
    conv: u32,
    mtu: u32,
    mss: u32,
    state: u32,

    snd_una: u32,
    snd_nxt: u32,
    rcv_nxt: u32,

    ts_recent: u32,
    ts_lastack: u32,
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
    updated: u32,

    ts_probe: u32,
    probe_wait: u32,

    dead_link: u32,
    incr: u32,

    snd_queue: VecDeque<Segment>,
    rcv_queue: VecDeque<Segment>,
    snd_buf: VecDeque<Segment>,
    rcv_buf: VecDeque<Segment>,

    acklist: Vec<(u32, u32)>,
    buffer: Vec<u8>,

    fastresend: i32,
    nocwnd: i32,
    stream: i32,
    output: Option<OutputFn>,
}

impl KCP {
    pub fn new(conv: u32, output: Option<OutputFn>) -> KCP {
        KCP {
            conv: conv,
            snd_wnd: KCP_WND_SND,
            rcv_wnd: KCP_WND_RCV,
            rmt_wnd: KCP_WND_RCV,
            mtu: KCP_MTU_DEF,
            mss: KCP_MTU_DEF - KCP_OVERHEAD,
            buffer: Vec::with_capacity(((KCP_MTU_DEF + KCP_OVERHEAD) * 3) as usize),
            snd_queue: VecDeque::new(),
            rcv_queue: VecDeque::new(),
            snd_buf: VecDeque::new(),
            rcv_buf: VecDeque::new(),
            acklist: Vec::new(),
            rx_rto: KCP_RTO_DEF,
            rx_minrto: KCP_RTO_MIN,
            interval: KCP_INTERVAL,
            ts_flush: KCP_INTERVAL,
            ssthresh: KCP_THRESH_INIT,
            dead_link: KCP_DEADLINK,
            output: output,
            ..Default::default()
        }
    }

    fn update(&mut self, current: u32) {
        self.current = current;
        if self.updated == 0 {
            self.updated = 1;
            self.ts_flush = self.current;
        }
        let mut slap: i32 = timediff(self.current, self.ts_flush);

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

    fn recv(&mut self, buffer: &mut [u8]) -> Result<usize, i32> {
        let mut len = buffer.len();

        if self.rcv_queue.is_empty() {
            return Err(-1);
        }

        let peeksize = match self.peeksize() {
            Ok(x) => x,
            Err(_) => return Err(-2),
        };

        if peeksize > len {
            return Err(-3);
        }

        let recover = self.rcv_queue.len() >= self.rcv_wnd as usize;
        len = 0;
        let mut count: usize = 0;
        for seg in &self.rcv_queue {
            let n = seg.data.len();
            buffer[len..len + n].clone_from_slice(&seg.data[..]);
            len += n;
            count += 1;
            if seg.frg == 0 {
                break;
            }
        }
        self.rcv_queue.drain(count..);

        assert!(len == peeksize);
        count = 0;
        let mut nrcv_que = self.rcv_queue.len();
        for seg in &self.rcv_buf {
            if seg.sn == self.rcv_nxt && nrcv_que < self.rcv_wnd as usize {
                self.rcv_nxt += 1;
                nrcv_que += 1;
                count += 1;
            } else {
                break;
            }
        }

        let mut new_rcv_buf = self.rcv_buf.split_off(count);
        self.rcv_queue.append(&mut self.rcv_buf);
        self.rcv_buf = new_rcv_buf;

        if self.rcv_queue.len() < self.rcv_wnd as usize && recover {
            self.probe |= KCP_ASK_TELL;
        }
        Ok(len)
    }

    fn send(&mut self, buffer: &mut [u8]) -> Result<usize, i32> {
        let mut len = buffer.len();
        if len == 0 {
            return Err(-1);
        }
        let mut start: usize = 0;

        if self.stream != 0 {
            if let Some(old) = self.snd_queue.back_mut() {
                let l = old.data.len();
                if l < self.mss as usize {
                    let capacity = self.mss as usize - l;
                    let extend = if len < capacity {
                        len
                    } else {
                        capacity
                    };
                    old.data.extend_from_slice(&buffer[start..start + extend]);
                    start += extend;
                    old.frg = 0;
                    len -= extend; // TODO
                }
            };

            if len <= 0 {
                return Ok(0);
            }
        }

        let mut count = if len <= self.mss as usize {
            // TODO
            1
        } else {
            (len + self.mss as usize - 1) / self.mss as usize
        };

        if count > 255 {
            return Err(-2);
        }

        if count == 0 {
            count = 1;
        }

        for i in 0..count {
            let size = if len > self.mss as usize {
                self.mss as usize
            } else {
                len
            };
            let mut seg = Segment::new(size);
            if len > 0 {
                seg.data.extend_from_slice(&buffer[start..start + size]);
            }
            seg.frg = if self.stream == 0 {
                (count - i - 1) as u32
            } else {
                0
            };
            self.snd_queue.push_back(seg);

            if start + size < buffer.len() {
                start += size;
            }
            len -= size;
        }
        Ok(0)
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
        let rto = self.rx_srtt + cmp::max(1, 4 * self.rx_rttval);
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
        if timediff(sn, self.snd_una) < 0 || timediff(sn, self.snd_nxt) >= 0 {
            return;
        }
        let mut index: usize = 0;
        for seg in &self.snd_buf {
            if sn == seg.sn {
                break;
            }
            if timediff(sn, seg.sn) < 0 {
                return;
            }
            index += 1;
        }
        self.snd_buf.remove(index);
    }

    fn parse_una(&mut self, una: u32) {
        let mut index: usize = 0;
        for seg in &self.snd_buf {
            if timediff(una, seg.sn) > 0 {
                index += 1;
            } else {
                break;
            }
        }
        self.snd_buf.drain(index..);
    }

    fn parse_fastack(&mut self, sn: u32) {
        if timediff(sn, self.snd_una) < 0 || timediff(sn, self.snd_nxt) >= 0 {
            return;
        }
        for seg in &mut self.snd_buf {
            if timediff(sn, seg.sn) < 0 {
                break;
            } else if sn != seg.sn {
                seg.fastack += 1;
            }
        }
    }

    #[inline]
    fn ack_push(&mut self, sn: u32, ts: u32) {
        self.acklist.push((sn, ts));
    }

    #[inline]
    fn ack_get(&self, index: usize) -> (u32, u32) {
        self.acklist[index]
    }

    fn parse_data(&mut self, newseg: Segment) {
        let sn = newseg.sn;
        let mut repeat = false;

        if timediff(sn, self.rcv_nxt + self.rcv_wnd) >= 0 || timediff(sn, self.rcv_nxt) < 0 {
            // ikcp_segment_delete(kcp, newseg);
            return;
        }

        let mut index: usize = 0;
        for seg in self.rcv_buf.iter().rev() {
            if seg.sn == sn {
                repeat = true;
                break;
            }
            if timediff(sn, seg.sn) > 0 {
                break;
            }
            index += 1;
        }

        if !repeat {
            let len = self.rcv_buf.len();
            if len <= 1 {
                self.rcv_buf.push_front(newseg);
            } else {
                self.rcv_buf.insert(len - index - 1, newseg);
            }
        } else {
            // ikcp_segment_delete(kcp, newseg);
        }

        index = 0;
        for seg in &self.rcv_buf {
            if seg.sn == self.rcv_nxt && self.rcv_queue.len() < self.rcv_wnd as usize {
                self.rcv_nxt += 1;
            } else {
                break;
            }
            index += 1;
        }
        let mut new_rcv_buf = self.rcv_buf.split_off(index);
        self.rcv_queue.append(&mut self.rcv_buf);
        self.rcv_buf = new_rcv_buf;
    }

    fn input(&mut self, data: &[u8]) -> i32 {
        let mut size = data.len();
        if size < 24 {
            return -1;
        }
        let old_una = self.snd_una;
        let mut p: usize = 0;
        let mut flag = false;
        let mut maxack: u32 = 0;
        loop {
            if size < KCP_OVERHEAD as usize {
                break;
            }

            let conv = decode32u(data, &mut p);
            if conv != self.conv {
                return -1;
            }

            let cmd = decode8u(data, &mut p);
            let frg = decode8u(data, &mut p);
            let wnd = decode16u(data, &mut p);
            let ts = decode32u(data, &mut p);
            let sn = decode32u(data, &mut p);
            let una = decode32u(data, &mut p);
            let len = decode32u(data, &mut p);

            size -= KCP_OVERHEAD as usize;

            let len = len as usize;
            if size < len {
                return -2;
            }

            let cmd = cmd as u32;
            if cmd != KCP_CMD_PUSH && cmd != KCP_CMD_ACK && cmd != KCP_CMD_WASK &&
               cmd != KCP_CMD_WINS {
                return -3;
            }

            self.rmt_wnd = wnd as u32;
            self.parse_una(una);
            self.shrink_buf();
            if cmd == KCP_CMD_ACK {
                let diff = timediff(self.current, ts);
                if diff >= 0 {
                    self.update_ack(diff as u32);
                }
                self.parse_ack(sn);
                self.shrink_buf();
                if !flag {
                    flag = true;
                    maxack = sn;
                } else {
                    if timediff(sn, maxack) > 0 {
                        maxack = sn;
                    }
                }
            } else if cmd == KCP_CMD_PUSH {
                if timediff(sn, self.rcv_nxt + self.rcv_wnd) < 0 {
                    self.ack_push(sn, ts);
                    if timediff(sn, self.rcv_nxt) >= 0 {
                        let mut seg = Segment::new(len);
                        seg.conv = conv;
                        seg.cmd = cmd;
                        seg.frg = frg as u32;
                        seg.wnd = wnd as u32;
                        seg.ts = ts;
                        seg.sn = sn;
                        seg.una = una;

                        if len > 0 {
                            seg.data.extend_from_slice(&data[p..p + len]);
                            p += len;
                        }
                        self.parse_data(seg);
                    }
                }
            } else if cmd == KCP_CMD_WASK {
                self.probe |= KCP_ASK_TELL;
            } else if cmd == KCP_CMD_WINS {
                // do nothing
            } else {
                return -3;
            }

            p += len;
            size -= len;
        }
        if flag {
            self.parse_fastack(maxack);
        }

        if timediff(self.snd_una, old_una) > 0 {
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
        0
    }


    fn wnd_unused(&self) -> u32 {
        let nrcv_que = self.rcv_queue.len() as u32;
        if nrcv_que < self.rcv_wnd {
            return self.rcv_wnd - nrcv_que;
        }
        return 0;
    }

    fn flush(&mut self) {
        // 'ikcp_update' haven't been called.
        if self.updated == 0 {
            return;
        }
        let current = self.current;
        let mut lost = false;
        let mut change = false;
        let mut ptr: usize = 0;
        let mut seg = Segment::new(0);

        seg.conv = self.conv;
        seg.cmd = KCP_CMD_ACK;
        seg.frg = 0;
        seg.wnd = self.wnd_unused();
        seg.una = self.rcv_nxt;
        seg.sn = 0;
        seg.ts = 0;

        // flush acknowledges
        for ack in &self.acklist {
            let size = ptr as u32;
            if size + KCP_OVERHEAD > self.mtu {
                if let Some(f) = self.output {
                    f(&self.buffer[..ptr]);
                    ptr = 0;
                } else {
                    return;
                }
            }
            seg.sn = ack.0;
            seg.ts = ack.1;

            encode_seg(&mut self.buffer, &mut ptr, &seg);
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
            let size = ptr as u32;
            if size + KCP_OVERHEAD > self.mtu {
                if let Some(f) = self.output {
                    f(&self.buffer[..ptr]);
                    ptr = 0;
                } else {
                    return;
                }
            }
            encode_seg(&mut self.buffer, &mut ptr, &seg);
        }

        // flush window probing commands
        if (self.probe & KCP_ASK_TELL) != 0 {
            seg.cmd = KCP_CMD_WINS;
            let size = ptr as u32;
            if size + KCP_OVERHEAD > self.mtu {
                if let Some(f) = self.output {
                    f(&self.buffer[..ptr]);
                    ptr = 0;
                } else {
                    return;
                }
            }
            encode_seg(&mut self.buffer, &mut ptr, &seg);
        }

        self.probe = 0;

        // calculate window size
        let mut cwnd = cmp::min(self.snd_wnd, self.rmt_wnd);
        if self.nocwnd == 0 {
            cwnd = cmp::min(self.cwnd, cwnd);
        }

        // move data from snd_queue to snd_buf
        while timediff(self.snd_nxt, self.snd_una + cwnd) < 0 {
            if let Some(mut newseg) = self.snd_queue.pop_front() {
                newseg.conv = self.conv;
                newseg.cmd = KCP_CMD_PUSH;
                newseg.wnd = seg.wnd;
                newseg.ts = current;
                newseg.sn = self.snd_nxt + 1;
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
        let resent: u32 = if self.fastresend > 0 {
            self.fastresend as u32
        } else {
            0xffffffff
        };
        let rtomin: u32 = if self.nodelay == 0 {
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

                let size = ptr;
                let len = segment.data.len();
                let need = KCP_OVERHEAD as usize + len;

                if size + need > self.mtu as usize {
                    if let Some(f) = self.output {
                        f(&self.buffer[..ptr]);
                        ptr = 0;
                    } else {
                        return;
                    }
                }

                encode_seg(&mut self.buffer, &mut ptr, &segment);

                if len > 0 {
                    segment.data.extend_from_slice(&self.buffer[ptr..ptr + len]);
                    ptr += len;
                }

                if segment.xmit >= self.dead_link {
                    self.state = 0xFFFFFFFF;
                }
            }
        }

        // flash remain segments
        if ptr > 0 {
            if let Some(f) = self.output {
                f(&self.buffer[..ptr]);
                ptr = 0;
            } else {
                return;
            }
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

    fn nodelay(&mut self, nodelay: i32, interval: i32, resend: i32, nc: i32) {
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
            self.fastresend = resend;
        }
        if nc >= 0 {
            self.nocwnd = nc;
        }
    }

    fn wndsize(&mut self, sndwnd: i32, rcvwnd: i32) {
        if sndwnd > 0 {
            self.snd_wnd = sndwnd as u32;
        }
        if rcvwnd > 0 {
            self.rcv_wnd = rcvwnd as u32;
        }
    }

    fn ikcp_waitsnd(&self) -> usize {
        self.snd_buf.len() + self.snd_queue.len()
    }
}

#[inline]
fn timediff(later: u32, earlier: u32) -> i32 {
    (later - earlier) as i32
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

fn encode_seg(ptr: &mut [u8], p: &mut usize, seg: &Segment) {
    encode32u(&mut ptr[..], p, seg.conv);
    encode8u(&mut ptr[..], p, seg.cmd as u8);
    encode8u(&mut ptr[..], p, seg.frg as u8);
    encode16u(&mut ptr[..], p, seg.wnd as u16);
    encode32u(&mut ptr[..], p, seg.ts);
    encode32u(&mut ptr[..], p, seg.sn);
    encode32u(&mut ptr[..], p, seg.una);
    encode32u(&mut ptr[..], p, seg.data.len() as u32);
}
