//! macOS Local Network permission.
//!
//! Since macOS 15 an app needs the user's OK before it may connect to
//! addresses on the local network (a router admin page, a test server on the
//! LAN, a phone or a VM). Plonix's proxy and Bench make those connections from
//! inside the app, so the app is what macOS asks about. macOS only asks when
//! the app first tries such a connection, and the first connection is refused
//! while the question is on screen; asking up front means a local target works
//! the first time it is opened. The text of the question is
//! NSLocalNetworkUsageDescription in Info.plist.
//!
//! There is no API that asks directly. Connecting a UDP socket to a
//! local-network address is enough to make macOS ask, and sends nothing.
//! Once the user has answered, this does nothing visible; they can change the
//! answer in System Settings › Privacy & Security › Local Network.

use std::net::{SocketAddr, UdpSocket};
use std::time::Duration;

/// Multicast groups count as local-network addresses. Port 9 is the discard
/// service, so even a stray datagram would go nowhere.
const PROBES: [&str; 2] = ["224.0.0.251:9", "[ff02::fb]:9"];

/// Makes macOS ask for Local Network access if it has not asked yet. Best
/// effort: errors (including a refusal) are ignored.
pub fn ask() {
    let sockets: Vec<UdpSocket> = PROBES
        .iter()
        .filter_map(|a| {
            let addr: SocketAddr = a.parse().ok()?;
            let bind = if addr.is_ipv4() {
                "0.0.0.0:0"
            } else {
                "[::]:0"
            };
            let socket = UdpSocket::bind(bind).ok()?;
            socket.connect(addr).ok().map(|()| socket)
        })
        .collect();
    // macOS may not show the question for a socket that is gone at once.
    std::thread::sleep(Duration::from_secs(5));
    drop(sockets);
}
