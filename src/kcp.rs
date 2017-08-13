use std::cmp;
use std::collections::VecDeque;
use std::io::{self, Cursor, Error, ErrorKind, Read, Write};

use bytes::{Buf, BufMut, BytesMut, LittleEndian};

const KCP_RTO_NDL: u32 = 30; // no delay min rto
const KCP_RTO_MIN: u32 = 100; // normal min rto
const KCP_RTO_DEF: u32 = 200;
const KCP_RTO_MAX: u32 = 60000;
const KCP_CMD_PUSH: u8 = 81; // cmd: push data
const KCP_CMD_ACK: u8 = 82; // cmd: ack
const KCP_CMD_WASK: u8 = 83; // cmd: window probe (ask)
const KCP_CMD_WINS: u8 = 84; // cmd: window size (tell)
const KCP_ASK_SEND: u32 = 1; // need to send KCP_CMD_WASK
const KCP_ASK_TELL: u32 = 2; // need to send KCP_CMD_WINS
const KCP_WND_SND: u32 = 32;
const KCP_WND_RCV: u32 = 32;
const KCP_MTU_DEF: usize = 1400;
// const KCP_ACK_FAST: u32 = 3; // never used
const KCP_INTERVAL: u32 = 100;
const KCP_OVERHEAD: usize = 24;
// const KCP_DEADLINK: u32 = 20; // never used
const KCP_THRESH_INIT: u32 = 2;
const KCP_THRESH_MIN: u32 = 2;
const KCP_PROBE_INIT: u32 = 7000; // 7 secs to probe window size
const KCP_PROBE_LIMIT: u32 = 120000; // up to 120 secs to probe window

#[derive(Default)]
struct Segment {
    conv: u32,
    cmd: u8,
    frg: u8,
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
    fn encode(&self, buf: &mut BytesMut) {
        buf.put_u32::<LittleEndian>(self.conv);
        buf.put::<u8>(self.cmd);
        buf.put::<u8>(self.frg);
        buf.put_u16::<LittleEndian>(self.wnd as u16);
        buf.put_u32::<LittleEndian>(self.ts);
        buf.put_u32::<LittleEndian>(self.sn);
        buf.put_u32::<LittleEndian>(self.una);
        buf.put_u32::<LittleEndian>(self.data.len() as u32);
        buf.put_slice(&self.data);
    }
}

/// kcp control block
pub struct KCP<W: Write> {
    conv: u32,
    mtu: usize,
    mss: usize,
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

    // user: String,
    buffer: BytesMut,

    fastresend: u32,

    nocwnd: bool,
    stream: bool,

    output: W,
}

impl<W: Write> KCP<W> {
    /// create a new kcp control object, `conv` must equal in two endpoint
    /// from the same connection. `user` will be passed to the output callback
    pub fn new(conv: u32, output: W) -> KCP<W> {
        KCP {
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
            // user: user,
            buffer: BytesMut::with_capacity((KCP_MTU_DEF + KCP_OVERHEAD) * 3),
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
            output: output,
        }
    }

    /// user/upper level recv: returns size, returns Err for EAGAIN
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
        let mut buf = Cursor::new(buf);
        let mut index: usize = 0;
        for seg in &self.rcv_queue {
            buf.write_all(&seg.data)?;
            index += 1;
            if seg.frg == 0 {
                break;
            }
        }
        if index > 0 {
            let new_rcv_queue = self.rcv_queue.split_off(index);
            self.rcv_queue = new_rcv_queue;
        }
        assert!(buf.position() as usize == peeksize);

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
        Ok(buf.position() as usize)
    }

    /// check the size of next message in the recv queue
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

    /// user/upper level send, returns Err for error
    pub fn send(&mut self, buf: &[u8]) -> io::Result<usize> {
        let n = buf.len();
        if n == 0 {
            return Err(Error::new(ErrorKind::InvalidInput, "no data available"));
        }
        let mut buf = Cursor::new(buf);

        // append to previous segment in streaming mode (if possible)
        if self.stream {
            if let Some(seg) = self.snd_queue.back_mut() {
                let l = seg.data.len();
                if l < self.mss as usize {
                    let new_len = cmp::min(l + n, self.mss as usize);
                    seg.data.resize(new_len, 0);
                    buf.read_exact(&mut seg.data[l..new_len])?;
                    seg.frg = 0;
                    if buf.remaining() == 0 {
                        return Ok(1);
                    }
                }
            };
        }

        let count = if buf.remaining() <= self.mss as usize {
            1
        } else {
            (buf.remaining() + self.mss as usize - 1) / self.mss as usize
        };

        if count > 255 {
            return Err(Error::new(ErrorKind::InvalidInput, "data too long"));
        }
        assert!(count > 0);
        let count = count as u8;

        // fragment
        for i in 0..count {
            let size = cmp::min(self.mss as usize, buf.remaining());
            let mut seg = Segment::default();
            seg.data.resize(size, 0);
            buf.read_exact(&mut seg.data)?;
            seg.frg = if !self.stream { count - i - 1 } else { 0 };
            self.snd_queue.push_back(seg);
        }
        Ok(n - buf.remaining())
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

    /// when you received a low level packet (eg. UDP packet), call it
    pub fn input(&mut self, buf: &[u8]) -> io::Result<usize> {
        let n = buf.len();
        let mut buf = Cursor::new(buf);

        if buf.remaining() < KCP_OVERHEAD {
            return Err(Error::new(ErrorKind::InvalidData, "invalid data"));
        }
        let old_una = self.snd_una;
        let mut flag = false;
        let mut maxack: u32 = 0;
        loop {
            if buf.remaining() < KCP_OVERHEAD {
                break;
            }

            let conv = buf.get_u32::<LittleEndian>();
            if conv != self.conv {
                return Err(Error::new(ErrorKind::InvalidData, "invalid data"));
            }

            let cmd = buf.get_u8();
            let frg = buf.get_u8();
            let wnd = buf.get_u16::<LittleEndian>();
            let ts = buf.get_u32::<LittleEndian>();
            let sn = buf.get_u32::<LittleEndian>();
            let una = buf.get_u32::<LittleEndian>();
            let len = buf.get_u32::<LittleEndian>();

            let len = len as usize;
            if buf.remaining() < len {
                return Err(Error::new(ErrorKind::UnexpectedEof, "unexpected EOF"));
            }

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
                        let mut seg = Segment::default();
                        seg.conv = conv;
                        seg.cmd = cmd;
                        seg.frg = frg;
                        seg.wnd = wnd as u32;
                        seg.ts = ts;
                        seg.sn = sn;
                        seg.una = una;
                        seg.data.resize(len, 0);
                        buf.read_exact(&mut seg.data)?;
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
        }
        if flag {
            self.parse_fastack(maxack);
        }

        if self.snd_una > old_una {
            if self.cwnd < self.rmt_wnd {
                let mss = self.mss as u32;
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
        Ok(n - buf.remaining())
    }

    fn wnd_unused(&self) -> u32 {
        let nrcv_que = self.rcv_queue.len() as u32;
        if nrcv_que < self.rcv_wnd {
            return self.rcv_wnd - nrcv_que;
        }
        0
    }

    /// flush pending data
    pub fn flush(&mut self) {
        // `update` haven't been called.
        if !self.updated {
            return;
        }
        let current = self.current;
        let mut lost = false;
        let mut change = false;
        let mut seg = Segment::default();

        seg.conv = self.conv;
        seg.cmd = KCP_CMD_ACK;
        seg.wnd = self.wnd_unused();
        seg.una = self.rcv_nxt;

        // flush acknowledges
        for ack in &self.acklist {
            if self.buffer.remaining_mut() + KCP_OVERHEAD > self.mtu {
                self.output.write_all(&self.buffer);
                self.buffer.clear();
            }
            seg.sn = ack.0;
            seg.ts = ack.1;
            seg.encode(&mut self.buffer);
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
            if self.buffer.remaining_mut() + KCP_OVERHEAD > self.mtu {
                self.output.write_all(&self.buffer);
                self.buffer.clear();
            }
            seg.encode(&mut self.buffer);
        }

        // flush window probing commands
        if (self.probe & KCP_ASK_TELL) != 0 {
            seg.cmd = KCP_CMD_WINS;
            if self.buffer.remaining_mut() + KCP_OVERHEAD > self.mtu {
                self.output.write_all(&self.buffer);
                self.buffer.clear();
            }
            seg.encode(&mut self.buffer);
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
                let need = KCP_OVERHEAD + len;

                if self.buffer.remaining_mut() + need > self.mtu {
                    self.output.write_all(&self.buffer);
                    self.buffer.clear();
                }
                segment.encode(&mut self.buffer);

                // never used
                // if segment.xmit >= self.dead_link {
                //     self.state = -1;
                // }
            }
        }

        // flash remain segments
        if self.buffer.remaining_mut() > 0 {
            self.output.write_all(&self.buffer);
            self.buffer.clear();
        }

        // update ssthresh
        if change {
            let inflight = self.snd_nxt - self.snd_una;
            self.ssthresh = inflight / 2;
            if self.ssthresh < KCP_THRESH_MIN {
                self.ssthresh = KCP_THRESH_MIN;
            }
            self.cwnd = self.ssthresh + resent;
            self.incr = self.cwnd * self.mss as u32;
        }

        if lost {
            self.ssthresh = cwnd / 2;
            if self.ssthresh < KCP_THRESH_MIN {
                self.ssthresh = KCP_THRESH_MIN;
            }
            self.cwnd = 1;
            self.incr = self.mss as u32;
        }

        if self.cwnd < 1 {
            self.cwnd = 1;
            self.incr = self.mss as u32;
        }
    }

    /// update state (call it repeatedly, every 10ms-100ms), or you can ask
    /// `check` when to call it again (without `input`/`send` calling).
    /// `current` - current timestamp in millisec.
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

    /// Determine when should you invoke `update`:
    /// returns when you should invoke `update` in millisec, if there
    /// is no `input`/`send` calling. you can call `update` in that
    /// time, instead of call `update` repeatly.
    /// Important to reduce unnacessary `update` invoking. use it to
    /// schedule `update` (eg. implementing an epoll-like mechanism,
    /// or optimize `update` when handling massive kcp connections)
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

    /// change MTU size, default is 1400
    pub fn setmtu(&mut self, mtu: usize) -> bool {
        if mtu < 50 || mtu < KCP_OVERHEAD {
            return false;
        }
        self.mtu = mtu;
        self.mss = self.mtu - KCP_OVERHEAD;
        let additional = (mtu + KCP_OVERHEAD) * 3 - self.buffer.capacity();
        if additional > 0 {
            self.buffer.reserve(additional);
        }
        true
    }

    /// fastest: nodelay(1, 20, 2, 1)
    /// `nodelay`: 0:disable(default), 1:enable
    /// `interval`: internal update timer interval in millisec, default is 100ms
    /// `resend`: 0:disable fast resend(default), 1:enable fast resend
    /// `nc`: false:normal congestion control(default), true:disable congestion control
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

    /// set maximum window size: `sndwnd`=32, `rcvwnd`=32 by default
    pub fn wndsize(&mut self, sndwnd: i32, rcvwnd: i32) {
        if sndwnd > 0 {
            self.snd_wnd = sndwnd as u32;
        }
        if rcvwnd > 0 {
            self.rcv_wnd = rcvwnd as u32;
        }
    }

    /// get how many packet is waiting to be sent
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
