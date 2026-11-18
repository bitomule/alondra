//! What the daemon answers on its hook socket when a peer asks whether it serves there.
pub const ACK_PREFIX: &[u8] = b"alondrad:";

pub fn probe_ack() -> Vec<u8> {
    let mut ack = ACK_PREFIX.to_vec();
    ack.extend(format!("{}\n", std::process::id()).into_bytes());
    ack
}
