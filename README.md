# kcp-rs
A KCP implementation in Rust

[![KCP Powered](https://img.shields.io/badge/KCP-Powered-blue.svg)](https://github.com/skywind3000/kcp)
[![Build Status](https://travis-ci.org/en/kcp-rs.svg?branch=master)](https://travis-ci.org/en/kcp-rs)

## Testing
I use Rust nightly, stable version should work too.
```
$ cargo test --release -- --nocapture

# results:
default mode result (27879ms):
avgrtt=3820 maxrtt=7887

normal mode result (20166ms):
avgrtt=144 maxrtt=363

fast mode result (20133ms):
avgrtt=138 maxrtt=339
```

## TODO
- [x] Migrate all tests from C version and fix bugs
- [ ] Verify correctness
- [ ] Improve the quality of code and make it more Rust-y
