//! prostor server: the thin I/O shell around the sans-IO core.
//!
//! Responsibilities (Phase 0): serve the built web app, relay WebRTC signaling
//! between producer and guests, receive FLV from the producer over a binary
//! WebSocket, publish it to RTMP destinations and record it to disk. Policy lives
//! in the `prostor-*` core crates; this crate owns sockets, files and the clock.

#![forbid(unsafe_code)]

pub mod app;
pub mod config;
pub mod egress;
pub mod ingest;
pub mod room;
pub mod rooms;
pub mod shutdown;
pub mod signaling;
pub mod ws;
