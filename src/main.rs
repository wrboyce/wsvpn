use std::sync::mpsc::channel;
use std::thread;

mod common;
mod tun;
mod websocket;

use crate::common::ChanMessage;
use crate::tun::Interface;
use crate::websocket::WebSocket;

#[macro_use]
extern crate clap;
#[macro_use]
extern crate log;
extern crate pretty_env_logger;

use clap::{App, Arg, SubCommand};

pub fn main() {
    pretty_env_logger::init();

    let version = crate_version!();
    let matches = App::new("wsvpn")
        .version(&version[..])
        .author("Will Boyce <will@balena.io>")
        .about("IP over WebSockets VPN")
        .subcommand(SubCommand::with_name("client").arg(Arg::with_name("server").required(true)))
        .subcommand(
            SubCommand::with_name("server")
                .arg(Arg::with_name("cidr").required(true))
                .arg(
                    Arg::with_name("bind")
                        .required(false)
                        .default_value("0.0.0.0:80"),
                ),
        )
        .get_matches();

    let mode = match matches.subcommand_name() {
        Some(cmd) => cmd,
        None => panic!("See help."),
    };
    let args = matches.subcommand_matches(mode).unwrap();
    let args_cidr = args.value_of("cidr");
    let args_bind = args.value_of("bind");
    let args_server = args.value_of("server");
    trace!("args: {:?}", args);
    info!("wsvpn {} v{}", mode, version);

    let (iface_sender, iface_receiver) = channel::<ChanMessage>();
    let (wsock_sender, wsock_receiver) = channel::<ChanMessage>();
    let iface = Interface::new(wsock_sender, iface_receiver, args_cidr);
    let wsock = WebSocket::new(iface_sender, wsock_receiver, iface.iface.clone(), args_cidr);
    let iface_thread = thread::spawn(move || iface.start());

    let wsock_thread = if mode == "server" {
        let bind = String::from(args_bind.unwrap());
        info!("Listening on {}", bind);
        thread::spawn(move || wsock.listen(bind.to_string()))
    } else {
        let server = String::from(args_server.unwrap());
        info!("Connecting to {}", server);
        thread::spawn(move || wsock.connect(server.to_string()))
    };

    iface_thread.join().unwrap();
    wsock_thread.join().unwrap();
}
