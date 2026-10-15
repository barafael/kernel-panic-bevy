# webrtc-data 0.17.2, patched for kernel-panic

This is the crates.io release of `webrtc-data` 0.17.2 (MIT / Apache-2.0,
<https://github.com/webrtc-rs/webrtc>), used through `[patch.crates-io]` in
the workspace `Cargo.toml`, with one change in `src/data_channel/mod.rs`
(`DataChannel::client`, marked "kernel-panic patch"):

**Negotiated data channels commit their reliability parameters.** Upstream,
the parameters a channel was created with (`ordered`, `max_retransmits`,
`max_packet_life_time`) reach the SCTP stream only in `server()` (after a
DCEP `DATA_CHANNEL_OPEN`) or when a DCEP `DATA_CHANNEL_ACK` arrives. A
negotiated channel exchanges no DCEP, so its stream kept the default:
ordered and fully reliable. matchbox creates every channel negotiated, so
its "unreliable" channel (`ordered: false, max_retransmits: 0`) was a
reliable ordered channel on native peers: each lost packet held up every
packet behind it until a fast retransmit (~100–200 ms) or the 1 s SCTP
retransmission timeout. Measured in
`net-experiments/m2-transport.md` §7. Drop this patch once upstream commits
the parameters for negotiated channels.

The doc image and the crate's CI files were left out.

---

<h1 align="center">
 <a href="https://webrtc.rs">WebRTC.rs</a>
 <br>
</h1>
<p align="center">
 <a href="https://github.com/webrtc-rs/data/actions">
  <img src="https://github.com/webrtc-rs/data/workflows/cargo/badge.svg">
 </a>
 <a href="https://codecov.io/gh/webrtc-rs/data">
  <img src="https://codecov.io/gh/webrtc-rs/data/branch/main/graph/badge.svg">
 </a>
 <a href="https://deps.rs/repo/github/webrtc-rs/data">
  <img src="https://deps.rs/repo/github/webrtc-rs/data/status.svg">
 </a>
 <a href="https://crates.io/crates/webrtc-data">
  <img src="https://img.shields.io/crates/v/webrtc-data.svg">
 </a>
 <a href="https://docs.rs/webrtc-data">
  <img src="https://docs.rs/webrtc-data/badge.svg">
 </a>
 <a href="https://doc.rust-lang.org/1.6.0/complement-project-faq.html#why-dual-mitasl2-license">
  <img src="https://img.shields.io/badge/license-MIT%2FApache--2.0-blue" alt="License: MIT/Apache 2.0">
 </a>
 <a href="https://discord.gg/4Ju8UHdXMs">
  <img src="https://img.shields.io/discord/800204819540869120?logo=discord" alt="Discord">
 </a>
</p>
<p align="center">
 A pure Rust implementation of WebRTC DataChannel. Rewrite <a href="https://github.com/pion/datachannel/releases/tag/v1.4.21">Pion</a> DataChannel in Rust
</p>
