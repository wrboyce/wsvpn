use std::collections::HashMap;
use std::net::Ipv4Addr;
use std::sync::mpsc::{Receiver, Sender};
use std::sync::{Arc, Mutex};

pub type ChanMessage = Vec<u8>;
pub type ChanSender = Sender<ChanMessage>;
pub type ChanReceiver = Receiver<ChanMessage>;
pub type PeerMap = Arc<Mutex<HashMap<Ipv4Addr, Option<ws::Sender>>>>;
