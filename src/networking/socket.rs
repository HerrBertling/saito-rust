use std::sync::Arc;

use futures::{FutureExt, StreamExt};
// use crate::{Client, Clients};

use serde::Deserialize;
use std::convert::TryInto;
//use serde_json::from_str;
use tokio::sync::{mpsc, RwLock};
use tokio_stream::wrappers::UnboundedReceiverStream;
use warp::ws::{Message, WebSocket};

use crate::{
    blockchain::Blockchain,
    crypto::{hash, verify, SaitoHash, SaitoPublicKey},
    mempool::{AddTransactionResult, Mempool},
    networking::network::{
        APIMessage, HandshakeChallenge, Peer, Peers, CHALLENGE_EXPIRATION_TIME, CHALLENGE_SIZE,
    },
    time::create_timestamp,
    transaction::Transaction,
    wallet::Wallet,
};

#[derive(Deserialize, Debug)]
pub struct TopicsRequest {
    topics: Vec<String>,
}

async fn peer_msg(
    id: SaitoHash,
    msg: Message,
    peers: Peers,
    wallet_lock: Arc<RwLock<Wallet>>,
    mempool_lock: Arc<RwLock<Mempool>>,
    blockchain_lock: Arc<RwLock<Blockchain>>,
) {
    let api_message = APIMessage::deserialize(&msg.as_bytes().to_vec());
    let command = String::from_utf8_lossy(api_message.message_name());
    match &command.to_string()[..] {
        "SHAKINIT" => {
            tokio::spawn(async move {
                if let Ok(serialized_handshake_challenge) =
                    new_handshake_challenge(&api_message, wallet_lock).await
                {
                    let api_message_response = APIMessage {
                        message_name: String::from("RESULT__").as_bytes().try_into().unwrap(),
                        message_id: api_message.message_id,
                        message_data: serialized_handshake_challenge,
                    };
                    let peers = peers.write().await;
                    let peer = peers.get(&id).unwrap();
                    let _foo = peer
                        .sender
                        .as_ref()
                        .unwrap()
                        .send(Ok(Message::binary(api_message_response.serialize())));
                }
            });
        }
        "SHAKCOMP" => {
            tokio::spawn(async move {
                if let Some(_hash) = socket_handshake_complete(&api_message, wallet_lock) {
                    let api_message_response = APIMessage {
                        message_name: String::from("RESULT__").as_bytes().try_into().unwrap(),
                        message_id: api_message.message_id,
                        message_data: String::from("OK").as_bytes().try_into().unwrap(),
                    };
                    let mut peers = peers.write().await;
                    let mut peer = peers.get_mut(&id).unwrap();
                    peer.has_handshake = true;
                    let _foo = peer
                        .sender
                        .as_ref()
                        .unwrap()
                        .send(Ok(Message::binary(api_message_response.serialize())));
                }
            });
        }
        "SENDTRXN" => {
            tokio::spawn(async move {
                let message_id = api_message.message_id;
                if let Some(tx) = socket_receive_transaction(api_message.clone()) {
                    let mut mempool = mempool_lock.write().await;
                    let api_message_response;
                    match mempool.add_transaction(tx).await {
                        AddTransactionResult::Accepted | AddTransactionResult::Exists => {
                            api_message_response = APIMessage {
                                message_name: String::from("RESULT__")
                                    .as_bytes()
                                    .try_into()
                                    .unwrap(),
                                message_id: message_id,
                                message_data: String::from("OK").as_bytes().try_into().unwrap(),
                            };

                            // the tx is accepted, we will propagate it to all available peers
                            let mut peers = peers.write().await;
                            peers.retain(|&k, _| k != id);

                            for (_, peer) in peers.iter() {
                                let _foo = peer
                                    .sender
                                    .as_ref()
                                    .unwrap()
                                     .send(Ok(Message::binary(api_message.serialize())));
                            }
                        }
                        AddTransactionResult::Invalid => {
                            api_message_response = APIMessage {
                                message_name: String::from("ERROR___")
                                    .as_bytes()
                                    .try_into()
                                    .unwrap(),
                                message_id: message_id,
                                message_data: String::from("ERROR").as_bytes().try_into().unwrap(),
                            };
                        }
                        AddTransactionResult::Rejected => {
                            api_message_response = APIMessage {
                                message_name: String::from("ERROR___")
                                    .as_bytes()
                                    .try_into()
                                    .unwrap(),
                                message_id: message_id,
                                message_data: String::from("ERROR").as_bytes().try_into().unwrap(),
                            };
                        }
                    }

                    // Return message to original peer
                    let mut peers = peers.write().await;
                    let mut peer = peers.get_mut(&id).unwrap();
                    peer.has_handshake = true;
                    let _foo = peer
                        .sender
                        .as_ref()
                        .unwrap()
                        .send(Ok(Message::binary(api_message_response.serialize())));
                }
            });
        }
        "REQCHAIN" => {
            tokio::spawn(async move {
                let message_id = api_message.message_id;
                // let message = api_message.message_data();
                // let _block_id: u64 = u64::from_be_bytes(message[0..8].try_into().unwrap());
                // let block_hash: SaitoHash = message[8..40].try_into().unwrap();
                // let _fork_id: SaitoHash = message[40..62].try_into().unwrap();
                // let blockchain = blockchain_lock.read().await;
                // let block = blockchain.get_block(&block_hash);
                // if block.is_none() {
                //     // Not sure what to do in this case...
                // } else {
                //     // I tried looking at returnLastSharedBlockId and updateForkId and coudln't figure out what fork_id is or how to use it
                // }
                let api_message_response;
                if let Some(bytes) = socket_send_blockchain(api_message, blockchain_lock).await {
                    println!("OUR BYTES: {:?}", bytes);
                    api_message_response = APIMessage {
                        message_name: String::from("RESULT__")
                            .as_bytes()
                            .try_into()
                            .unwrap(),
                        message_id: message_id,
                        message_data: bytes,
                    };
                } else {
                    api_message_response = APIMessage {
                        message_name: String::from("ERROR___")
                            .as_bytes()
                            .try_into()
                            .unwrap(),
                        message_id: message_id,
                        message_data: String::from("ERROR").as_bytes().try_into().unwrap(),
                    };
                }

                let mut peers = peers.write().await;
                let mut peer = peers.get_mut(&id).unwrap();
                peer.has_handshake = true;
                let _foo = peer
                    .sender
                    .as_ref()
                    .unwrap()
                    .send(Ok(Message::binary(api_message_response.serialize())));
            });
        }
        "SNDCHAIN" => {
            tokio::spawn(async move {
                let _message_id = api_message.message_id;
            });
        }
        "REQBLKHD" => {
            tokio::spawn(async move {
                let _message_id = api_message.message_id;
            });
        }
        "SNDBLKHD" => {
            tokio::spawn(async move {
                let _message_id = api_message.message_id;
            });
        }
        "SNDTRANS" => {
            tokio::spawn(async move {
                let _message_id = api_message.message_id;
            });
        }
        "REQBLOCK" => {
            tokio::spawn(async move {
                let _message_id = api_message.message_id;
            });
        }
        "SNDKYCHN" => {
            tokio::spawn(async move {
                let _message_id = api_message.message_id;
            });
        }
        _ => {}
    }
}

pub async fn peer_connection(
    ws: WebSocket,
    id: SaitoHash,
    peers: Peers,
    mut peer: Peer,
    wallet_lock: Arc<RwLock<Wallet>>,
    mempool_lock: Arc<RwLock<Mempool>>,
    blockchain_lock: Arc<RwLock<Blockchain>>,
) {
    println!("peer_connection");
    let (peer_ws_sender, mut peer_ws_rcv) = ws.split();
    let (peer_sender, peer_rcv) = mpsc::unbounded_channel();
    let peer_rcv = UnboundedReceiverStream::new(peer_rcv);
    tokio::task::spawn(peer_rcv.forward(peer_ws_sender).map(|result| {
        if let Err(e) = result {
            eprintln!("error sending websocket msg: {}", e);
        }
    }));

    println!("peer channel created");
    peer.sender = Some(peer_sender);
    println!("peer sender set");
    peers.write().await.insert(id.clone(), peer);

    println!("{:?} connected", id);

    while let Some(result) = peer_ws_rcv.next().await {
        let msg = match result {
            Ok(msg) => msg,
            Err(e) => {
                eprintln!(
                    "error receiving ws message for id: {:?}): {}",
                    id.clone(),
                    e
                );
                break;
            }
        };
        peer_msg(
            id,
            msg,
            peers.clone(),
            wallet_lock.clone(),
            mempool_lock.clone(),
            blockchain_lock.clone(),
        )
        .await;
    }

    peers.write().await.remove(&id);
    println!("{:?} disconnected", id);
}
/*
    The serialized handshake init message shoudl fit this format
        challenger_ip           4 bytes(IP as 4 bytes)
        challenger_pubkey       33 bytes (SECP256k1 compact form)
*/
pub async fn new_handshake_challenge(
    message: &APIMessage,
    wallet_lock: Arc<RwLock<Wallet>>,
) -> crate::Result<Vec<u8>> {
    let wallet = wallet_lock.read().await;
    let my_pubkey = wallet.get_publickey();
    let my_privkey = wallet.get_privatekey();

    // let mut hex_pubkey: [u8; 66] = [0; 66];
    // hex_pubkey.clone_from_slice(raw_query_str[2..68].as_bytes());

    // let mut peer_pubkey: SaitoPublicKey = [0u8; 33];
    // match hex::decode_to_slice(hex_pubkey, &mut peer_pubkey as &mut [u8]) {
    //     // TODO figure out how to return more meaningful errors from Warp and replace all the warp::reject
    //     Err(_e) => return Err(warp::reject()),
    //     _ => {}
    // };

    let mut peer_octets: [u8; 4] = [0; 4];
    peer_octets[0..4].clone_from_slice(&message.message_data[0..4]);
    let peer_pubkey: SaitoPublicKey = message.message_data[4..37].try_into().unwrap();

    // TODO configure the node's IP somewhere...
    let my_octets: [u8; 4] = [42, 42, 42, 42];

    // TODO get the IP of this socket connection somehow and validate it..
    // let peer_octets: [u8; 4] = match addr.unwrap().ip() {
    //     IpAddr::V4(ip4) => ip4.octets(),
    //     _ => panic!("Saito Handshake does not support IPV6"),
    // };

    let challenge = HandshakeChallenge::new(my_octets, peer_octets, my_pubkey, peer_pubkey);
    let serialized_challenge = challenge.serialize_with_sig(my_privkey);

    Ok(serialized_challenge)
}

pub fn socket_handshake_complete(
    message: &APIMessage,
    _wallet_lock: Arc<RwLock<Wallet>>,
) -> Option<SaitoHash> {
    let (challenge, my_sig, their_sig) =
        HandshakeChallenge::deserialize_with_both_sigs(&message.message_data());
    if challenge.timestamp() < create_timestamp() - CHALLENGE_EXPIRATION_TIME {
        println!("Error validating timestamp for handshake complete");
        return None;
    }
    if !verify(
        &hash(&message.message_data[..CHALLENGE_SIZE + 64].to_vec()),
        their_sig,
        challenge.challengie_pubkey(),
    ) {
        // TODO figure out how to return more meaningful errors from Warp and replace all the warp::reject
        // return Err("ERROR WITH SIG VALIDATION");
        println!("Error with validating challengie sig");
        return None;
    }
    if !verify(
        &hash(&message.message_data[..CHALLENGE_SIZE].to_vec()),
        my_sig,
        challenge.challenger_pubkey(),
    ) {
        // TODO figure out how to return more meaningful errors from Warp and replace all the warp::reject
        println!("Error with validating challenger sig");
        return None;
    }

    Some(hash(&message.message_data))
}

pub fn socket_receive_transaction(message: APIMessage) -> Option<Transaction> {
    let tx = Transaction::deserialize_from_net(message.message_data);
    Some(tx)
}

pub async fn socket_send_blockchain(
    message: APIMessage,
    blockchain_lock: Arc<RwLock<Blockchain>>,
) -> Option<Vec<u8>> {
    let block_hash: SaitoHash = message.message_data[0..32].try_into().unwrap();
    let _fork_id: SaitoHash = message.message_data[32..64].try_into().unwrap();

    let mut hashes: Vec<u8> = vec![];
    let blockchain = blockchain_lock.read().await;

    if let Some(target_block) = blockchain.get_latest_block() {
        let target_block_hash = target_block.get_hash();
        println!("TARGET BLOCK HASH: {:?}", target_block_hash);
        println!("SENT BLOCK HASH: {:?}", block_hash);
        println!("ARE THEY EQUAL? {}", target_block_hash == block_hash);
        if target_block_hash != block_hash {
            hashes.extend_from_slice(&target_block_hash);
            let mut previous_block_hash = target_block.get_previous_block_hash();
            while !blockchain.get_block(&previous_block_hash).is_none()
                && previous_block_hash != block_hash
            {
                // println!("{:?}", previous_block_hash);
                if let Some(block) = blockchain.get_block(&previous_block_hash) {
                    hashes.extend_from_slice(&block.get_hash());
                    previous_block_hash = block.get_previous_block_hash();
                }
            }
        } else {
            // println!("THEY ARE EQUAL");
            // println!("{:?}", hashes);
        }
        Some(hashes)
    } else {
        None
    }

}