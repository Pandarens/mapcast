//! Lets the phone app find the companion on the local network without typing its address.
//!
//! The app sends a UDP broadcast "MAPCAST?" to the companion's port; the companion answers the
//! sender with "MAPCAST <page address>". The token is not sent: the page asks for it once.

use std::net::UdpSocket;
use std::time::Duration;

pub const QUESTION: &[u8] = b"MAPCAST?";

/// Answers the app's question on `port` (UDP) in a thread of its own.
pub fn serve(port: u16, url: String) {
    let socket = match UdpSocket::bind(("0.0.0.0", port)) {
        Ok(s) => s,
        Err(e) => {
            log::warn!("the phone app cannot find this PC by itself (UDP port {port}: {e}); type the address in it");
            return;
        }
    };
    let _ = std::thread::Builder::new().name("discovery".into()).spawn(move || answer(socket, &url));
}

fn answer(socket: UdpSocket, url: &str) {
    let reply = format!("MAPCAST {url}");
    let mut buf = [0u8; 64];
    loop {
        match socket.recv_from(&mut buf) {
            Ok((n, from)) if &buf[..n] == QUESTION => {
                log::debug!("found by the phone app at {from}");
                let _ = socket.send_to(reply.as_bytes(), from);
            }
            Ok(_) => {}
            Err(_) => std::thread::sleep(Duration::from_millis(200)),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn answers_the_question_and_nothing_else() {
        let probe = UdpSocket::bind("127.0.0.1:0").unwrap();
        let server = UdpSocket::bind("127.0.0.1:0").unwrap();
        let port = server.local_addr().unwrap().port();
        std::thread::spawn(move || answer(server, "http://192.168.1.5:8765"));
        probe.set_read_timeout(Some(Duration::from_millis(300))).unwrap();
        let mut buf = [0u8; 128];
        probe.send_to(b"hello", ("127.0.0.1", port)).unwrap();
        assert!(probe.recv_from(&mut buf).is_err());
        probe.set_read_timeout(Some(Duration::from_secs(2))).unwrap();
        probe.send_to(QUESTION, ("127.0.0.1", port)).unwrap();
        let (n, _) = probe.recv_from(&mut buf).unwrap();
        assert_eq!(&buf[..n], b"MAPCAST http://192.168.1.5:8765");
    }
}
