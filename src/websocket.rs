use std::collections::HashMap;
use std::net::Ipv4Addr;
use std::sync::{Arc, Mutex};
use std::thread;

use crate::common::*;
use crate::tun::IfaceWrapper;

use etherparse::Ipv4HeaderSlice;
use ipnetwork::Ipv4Network;
use ws::util::{Timeout, Token};
use ws::{CloseCode, Frame, Handshake, Message, Sender};

const PING_INTERVAL_MS: u64 = 10_000;
const TIMEOUT_MS: u64 = 60_000;

const PING: Token = Token(0);
const TIMEOUT: Token = Token(1);

#[derive(Debug)]
pub struct WebSocket {
    clients: PeerMap,

    iface: Arc<IfaceWrapper>,
    sender: ChanSender,
    receiver: ChanReceiver,
}

impl WebSocket {
    pub fn new(
        sender: ChanSender,
        receiver: ChanReceiver,
        iface: Arc<IfaceWrapper>,
        cidr: Option<&str>,
    ) -> WebSocket {
        let mut clients = HashMap::new();
        if let Some(cidr) = cidr {
            let network = cidr.parse::<Ipv4Network>().unwrap();
            let first = network.nth(0).unwrap();
            for addr in network.iter().filter(|&addr| {
                addr != network.network() && addr != first && addr != network.broadcast()
            }) {
                clients.insert(addr, None);
            }
        }
        WebSocket {
            clients: Arc::new(Mutex::new(clients)),
            sender,
            receiver,
            iface,
        }
    }

    pub fn listen(&self, addr: String) {
        let clients = self.clients.clone();
        let sender = self.sender.clone();
        let iface = self.iface.clone();

        thread::spawn(move || {
            ws::listen(&addr, |peer| {
                Handler::new_server(peer, clients.clone(), iface.clone(), sender.clone())
            })
            .expect("cannot open listening socket")
        });

        loop {
            self.recv(self.receiver.recv().unwrap());
        }
    }

    pub fn connect(&self, addr: String) {
        let clients = self.clients.clone();
        let sender = self.sender.clone();
        let iface = self.iface.clone();

        thread::spawn(move || {
            ws::connect(addr, |peer| {
                Handler::new_client(peer, clients.clone(), iface.clone(), sender.clone())
            })
            .expect("cannot connect to server")
        });

        loop {
            self.recv(self.receiver.recv().unwrap());
        }
    }

    /// receive data from the interface via the channel
    fn recv(&self, s: Vec<u8>) {
        trace!("rx data from tun interface");
        match Ipv4HeaderSlice::from_slice(&s) {
            Err(_) => (),
            Ok(iph) => {
                let dst = iph.destination_addr();
                let clients = self
                    .clients
                    .lock()
                    .expect("failed to acquire lock on PeerMap");
                trace!("forwarding packet to {}", dst);
                match clients.get(&dst) {
                    Some(Some(peer)) => peer.send(s).unwrap(),
                    _ => {
                        warn!("no peer found for {:?}", dst);
                    }
                };
            }
        };
    }
}

#[derive(Debug)]
struct Handler {
    peers: PeerMap,

    peer: Sender,
    ip: Option<Ipv4Addr>,

    sender: ChanSender,

    is_server: bool,

    iface: Arc<IfaceWrapper>,
    timeout: Option<Timeout>,
}

impl Handler {
    fn new(
        peer: Sender,
        peers: PeerMap,
        iface: Arc<IfaceWrapper>,
        sender: ChanSender,
        is_server: bool,
    ) -> Self {
        Handler {
            peer,
            peers,
            sender,
            iface,
            is_server,
            ip: None,
            timeout: None,
        }
    }

    fn new_server(
        peer: Sender,
        peers: PeerMap,
        iface: Arc<IfaceWrapper>,
        sender: ChanSender,
    ) -> Self {
        Self::new(peer, peers, iface, sender, true)
    }

    fn new_client(
        peer: Sender,
        peers: PeerMap,
        iface: Arc<IfaceWrapper>,
        sender: ChanSender,
    ) -> Self {
        Self::new(peer, peers, iface, sender, false)
    }
}

impl ws::Handler for Handler {
    fn on_open(&mut self, _: Handshake) -> ws::Result<()> {
        trace!("handler.on_open");
        if !self.is_server {
            debug!("sending HELLO to initiate handshake");
            self.peer.send("HELLO")
        } else {
            Ok(())
        }
    }

    fn on_frame(&mut self, frame: Frame) -> ws::Result<Option<Frame>> {
        if self.is_server {
            // reset activity timeout counter
            self.peer.timeout(TIMEOUT_MS, TIMEOUT).unwrap();
        }
        Ok(Some(frame))
    }
    fn on_message(&mut self, msg: Message) -> ws::Result<()> {
        match (self.is_server, &msg) {
            (_, Message::Binary(_)) => {
                // TODO: can we write directly to `self.iface.0.send` here?
                self.sender.send(msg.into_data()).unwrap();
                Ok(())
            }
            (true, Message::Text(_)) if msg.as_text()? == "THANKS" => {
                if let Some(peer_ip) = self.ip {
                    info!("completed handshake with peer {:?}", peer_ip);
                }
                Ok(())
            }
            (_, Message::Text(_)) if &msg.as_text()?[..5] == "HELLO" => {
                if self.is_server {
                    // I am the server, give the user an IP address
                    // and store them in our map.
                    let id = self.peer.connection_id();
                    debug!("received hello from clientid={}", id);
                    let mut peers = self
                        .peers
                        .lock()
                        .expect("failed to acquire lock on self.peers");
                    let mut available_ips =
                        peers.iter().filter_map(
                            |(&addr, peer)| if peer.is_none() { Some(addr) } else { None },
                        );
                    match (self.iface.network(), available_ips.next()) {
                        (Some(network), Some(ip)) => {
                            self.ip = Some(ip);
                            peers.insert(ip, Some(self.peer.clone()));
                            // setup keepalive for peer
                            self.peer.timeout(PING_INTERVAL_MS, PING).unwrap();
                            self.peer.timeout(TIMEOUT_MS, TIMEOUT).unwrap();
                            // finalise handshake
                            self.peer.send(format!(
                                "HELLO {}/{} {}",
                                ip,
                                network.prefix(),
                                network.nth(1).unwrap(),
                            ))
                        }
                        (Some(_), None) => {
                            warn!("rejecting client, ip pool depleted");
                            self.peer.close(CloseCode::Extension).unwrap();
                            Ok(())
                        }
                        (None, _) => {
                            warn!("rejecting client, server interface not ready");
                            self.peer.close(CloseCode::Extension).unwrap();
                            Ok(())
                        }
                    }
                } else {
                    // I am a client, parse `HELLO <ip> <peer>`
                    // FIXME: this is ugly and it sucks
                    let hello_tokens = msg.as_text()?.split(' ').collect::<Vec<&str>>();
                    if hello_tokens.len() != 3 {
                        error!("Handshake Error!");
                        self.peer.close(CloseCode::Extension).unwrap();
                        return Ok(());
                    }
                    let cidr = String::from(hello_tokens[1]);
                    self.iface.configure(cidr);
                    let peer = hello_tokens[2].parse::<Ipv4Addr>().unwrap();
                    self.peers
                        .lock()
                        .expect("failed to acquire lock on self.peers")
                        .insert(peer, Some(self.peer.clone()));
                    info!("completed handshake with peer {:?}", peer);
                    self.peer.send("THANKS")
                }
            }
            _ => Ok(()),
        }
    }

    fn on_timeout(&mut self, event: Token) -> ws::Result<()> {
        match event {
            PING => {
                self.peer.ping(vec![]).unwrap();
                self.peer.timeout(5_000, PING)
            }
            TIMEOUT => self.peer.close(CloseCode::Away),
            _ => Err(ws::Error::new(
                ws::ErrorKind::Internal,
                "Invalid timeout token encountered!",
            )),
        }
    }

    fn on_new_timeout(&mut self, event: Token, timeout: Timeout) -> ws::Result<()> {
        if event == TIMEOUT {
            if let Some(t) = self.timeout.take() {
                self.peer.cancel(t).unwrap();
            }
            self.timeout = Some(timeout)
        }
        Ok(())
    }

    fn on_close(&mut self, code: CloseCode, reason: &str) {
        debug!("Connection closing due to ({:?}) {}", code, reason);
        if self.is_server {
            // client disconnected, free IP
            if let Some(ip) = self.ip {
                self.peers
                    .lock()
                    .expect("failed to acquire lock on self.peers")
                    .remove(&ip)
                    .expect("error unregistering client");
            }
        } else {
            warn!("shutting down...");
            std::process::exit(1);
        }
    }
}
