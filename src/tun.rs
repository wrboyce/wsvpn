use std::process::Command;
use std::sync::Arc;
use std::thread;

use crate::common::*;

use ipnetwork::Ipv4Network;
use tun_tap_mac::{Iface, Mode};

#[derive(Debug)]
pub struct IfaceWrapper {
    pub iface: Iface,
    network: Option<Ipv4Network>,
}

impl IfaceWrapper {
    pub fn new(iface: Iface) -> Self {
        IfaceWrapper {
            iface,
            network: None,
        }
    }

    pub fn name(&self) -> &str {
        self.iface.name()
    }

    pub fn set_network(&mut self, cidr: String, configure: bool) {
        self.network = Some(cidr.parse::<Ipv4Network>().unwrap());
        if configure {
            self.configure(cidr);
        }
    }

    pub fn network(&self) -> Option<Ipv4Network> {
        self.network
    }

    pub fn configure(&self, cidr: String) {
        debug!("bringing /dev/{} up with IP={}", self.name(), cidr);
        cmd("ip", &["addr", "add", "dev", self.name(), &cidr]);
        cmd("ip", &["link", "set", "up", "dev", self.name()]);
    }
}

#[derive(Debug)]
pub struct Interface {
    pub iface: Arc<IfaceWrapper>,
    sender: ChanSender,
    receiver: ChanReceiver,
}

impl Interface {
    pub fn new(sender: ChanSender, receiver: ChanReceiver, cidr: Option<&str>) -> Interface {
        let mut iface = IfaceWrapper::new(
            Iface::without_packet_info("wsvpn%d", Mode::Tun).expect("interface creation failed"),
        );

        if let Some(cidr) = cidr {
            iface.set_network(cidr.to_string(), true);
        };

        Interface {
            iface: Arc::new(iface),
            sender,
            receiver,
        }
    }

    pub fn start(&self) {
        let iface = self.iface.clone();
        let wtx = self.sender.clone();

        thread::spawn(move || {
            let mut buf = vec![0; 1500];
            loop {
                let len = iface.iface.recv(&mut buf).unwrap();
                // TODO: if Interface was aware of peers, then we could send this directly.
                wtx.send(buf[..len].to_vec()).unwrap();
            }
        });

        loop {
            match self.receiver.recv() {
                // TODO: Interface refs can be passed around, so can WebSocket do some magic here?
                Ok(data) => self.recv(data),
                Err(e) => panic!("Something bad happened: {:?}", e),
            }
        }
    }

    fn recv(&self, s: Vec<u8>) {
        trace!("[Iface (pushing to other end))] rx data from websocket");
        self.iface.iface.send(&s).unwrap();
    }
}

fn cmd(cmd: &str, args: &[&str]) {
    let ecode = Command::new(cmd)
        .args(args)
        .spawn()
        .unwrap()
        .wait()
        .unwrap();
    assert!(ecode.success(), "Failed to execte {}", cmd);
}
