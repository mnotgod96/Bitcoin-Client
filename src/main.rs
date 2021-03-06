#[cfg(test)]
#[macro_use]
extern crate hex_literal;

pub mod api;
pub mod block;
pub mod blockchain;
pub mod crypto;
pub mod miner;
pub mod network;
pub mod transaction;

use clap::clap_app;
use crossbeam::channel;
use log::{error, info};
use api::Server as ApiServer;
use network::{server, worker};
use std::net;
use std::process;
use std::thread;
use std::time;
use std::sync::{Arc, Mutex};
use std::collections::HashMap;
use ring::digest;
use ring::signature::{self, Ed25519KeyPair, Signature, KeyPair, VerificationAlgorithm, EdDSAParameters};
use crypto::hash::{H160, H256, Hashable};
use network::message::Message;
use transaction::{TxIn, TxOut, Transaction, SignedTransaction, State};

fn main() {
    // parse command line arguments
    let matches = clap_app!(Bitcoin =>
     (version: "0.1")
     (about: "Bitcoin client")
     (@arg verbose: -v ... "Increases the verbosity of logging")
     (@arg peer_addr: --p2p [ADDR] default_value("127.0.0.1:6000") "Sets the IP address and the port of the P2P server")
     (@arg api_addr: --api [ADDR] default_value("127.0.0.1:7000") "Sets the IP address and the port of the API server")
     (@arg known_peer: -c --connect ... [PEER] "Sets the peers to connect to at start")
     (@arg p2p_workers: --("p2p-workers") [INT] default_value("4") "Sets the number of worker threads for P2P server")
    )
    .get_matches();

    // init logger
    let verbosity = matches.occurrences_of("verbose") as usize;
    stderrlog::new().verbosity(verbosity).init().unwrap();

    // parse p2p server address
    let p2p_addr = matches
        .value_of("peer_addr")
        .unwrap()
        .parse::<net::SocketAddr>()
        .unwrap_or_else(|e| {
            error!("Error parsing P2P server address: {}", e);
            process::exit(1);
        });

    // parse api server address
    let api_addr = matches
        .value_of("api_addr")
        .unwrap()
        .parse::<net::SocketAddr>()
        .unwrap_or_else(|e| {
            error!("Error parsing API server address: {}", e);
            process::exit(1);
        });

    // create channels between server and worker
    let (msg_tx, msg_rx) = channel::unbounded();

    // start the p2p server
    let (server_ctx, server) = server::new(p2p_addr, msg_tx).unwrap();
    server_ctx.start().unwrap();

    // start the worker
    let p2p_workers = matches
        .value_of("p2p_workers")
        .unwrap()
        .parse::<usize>()
        .unwrap_or_else(|e| {
            error!("Error parsing P2P workers: {}", e);
            process::exit(1);
        });

    let the_chain = blockchain::Blockchain::new();
    let chain_lock = Arc::new(Mutex::new(the_chain));
    let buffer = HashMap::new();
    let buffer_lock = Arc::new(Mutex::new(buffer));
    let the_mempool = transaction::Mempool::new();
    let mempool_lock = Arc::new(Mutex::new(the_mempool));
    let the_state = State::new();
    let state_lock = Arc::new(Mutex::new(the_state));

    let worker_ctx = worker::new(
        p2p_workers,
        msg_rx,
        &server,
        &chain_lock,
        &buffer_lock,
        &mempool_lock,
        &state_lock,
    );
    worker_ctx.start();

    let server_ = server.clone();
    let mempool_lock_ = mempool_lock.clone();
    thread::spawn(move || {
        loop {
            // use rand::Rng;
            // let mut rng = rand::thread_rng();
            // use crate::crypto::key_pair;

            thread::sleep(time::Duration::from_millis(10000));
            // let mut map_key = 0;
            // let mut map_val = 0;
            // for key in state_lock.utxo.keys() {
            //     map_key = key.clone();
            //     val = state_lock.utxo[&key].clone();
            //     break;
            // }

            // let key = key_pair::random();
            // let public_key = key.public_key();
            // let pk_hash: H256 = digest::digest(&digest::SHA256, public_key.as_ref()).into();
            // let recipient: H160 = pk_hash.to_addr().into();
            // let value: u64 = map_val.0;
            // let tx_out = TxOut { recipient: recipient, value: value };

            // let previous_output: H256 = map_key.0;
            // let index: u8 = map_key.1;
            // let tx_in = TxIn { previous_output: previous_output, index: index };

            let seed = [255u8; 32];
            let key = Ed25519KeyPair::from_seed_unchecked(&seed).unwrap();
            let public_key = key.public_key();
            let pk_hash: H256 = digest::digest(&digest::SHA256, public_key.as_ref()).into();
            let recipient: H160 = pk_hash.to_addr().into();
            let value: u64 = 10000;
            let tx_out = TxOut { recipient: recipient, value: value };

            let previous_output: H256 = [0u8; 32].into();
            let index: u8 = 0;
            let tx_in = TxIn { previous_output: previous_output, index: index };

            let inputs = vec![tx_in];
            let outputs = vec![tx_out];
            let tx = Transaction { input: inputs, output: outputs };
            let seed_sender = [0u8; 32];
            let key_sender = Ed25519KeyPair::from_seed_unchecked(&seed_sender).unwrap();
            let pk_sender = key_sender.public_key();
            let m = bincode::serialize(&tx).unwrap();
            let txid = digest::digest(&digest::SHA256, digest::digest(&digest::SHA256, m.as_ref()).as_ref());
            let sig = key_sender.sign(txid.as_ref());
            let signed_tx = SignedTransaction { transaction: tx, public_key: pk_sender.as_ref().to_vec(), signature: sig.as_ref().to_vec() };

            let mut mempool_un = mempool_lock_.lock().unwrap();
            mempool_un.insert(&signed_tx);
            let mut hash: H256 = signed_tx.hash();
            let pk_sender_hash: H256 = digest::digest(&digest::SHA256, pk_sender.as_ref()).into();
            let sender: H160 = pk_sender_hash.to_addr().into();
            println!("New transaction generated. Sending from {:?} to {:?}.", sender, recipient);
            server_.broadcast(Message::NewTransactionHashes(vec![hash]));
        }
    });

    // start the miner
    let (miner_ctx, miner) = miner::new(
        &server,
        &chain_lock,
        &mempool_lock,
        &state_lock,
    );
    miner_ctx.start();

    // connect to known peers
    if let Some(known_peers) = matches.values_of("known_peer") {
        let known_peers: Vec<String> = known_peers.map(|x| x.to_owned()).collect();
        let server = server.clone();
        thread::spawn(move || {
            for peer in known_peers {
                loop {
                    let addr = match peer.parse::<net::SocketAddr>() {
                        Ok(x) => x,
                        Err(e) => {
                            error!("Error parsing peer address {}: {}", &peer, e);
                            break;
                        }
                    };
                    match server.connect(addr) {
                        Ok(_) => {
                            info!("Connected to outgoing peer {}", &addr);
                            break;
                        }
                        Err(e) => {
                            error!(
                                "Error connecting to peer {}, retrying in one second: {}",
                                addr, e
                            );
                            thread::sleep(time::Duration::from_millis(1000));
                            continue;
                        }
                    }
                }
            }
        });
    }


    // start the API server
    ApiServer::start(
        api_addr,
        &miner,
        &server,
    );

    loop {
        std::thread::park();
    }
}
