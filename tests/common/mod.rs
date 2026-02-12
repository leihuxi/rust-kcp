//! Shared test helpers for KCP integration tests

use kcp_tokio::engine::KcpEngine;

/// Send all output packets from one engine into another engine's input.
pub fn transfer(src: &mut KcpEngine, dst: &mut KcpEngine) {
    for packet in src.drain_output() {
        let _ = dst.input(packet);
    }
}
