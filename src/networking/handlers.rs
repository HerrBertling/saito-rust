use crate::blockchain::Blockchain;
use crate::mempool::Mempool;
use crate::networking::network::Result;
use crate::storage::Storage;
use crate::transaction::Transaction;
use crate::wallet::Wallet;
use base58::ToBase58;
use std::sync::Arc;
use tokio::sync::RwLock;
use warp::reject::Reject;
use warp::reply::Response;
use warp::{Buf, Rejection, Reply};

use super::peer::{handle_inbound_peer_connection, PeersDB};

#[derive(Debug)]
struct Invalid;
impl Reject for Invalid {}

#[derive(Debug)]
struct AlreadyExists;
impl Reject for AlreadyExists {}

/// It seems that Warp handlers must return a Result<impl Reply>.
/// It looks like this was used as a simple way to turn a String
/// into a warp::Reply. It may be possilbe to use use Response::new
/// directly, or this may not be needed if we get rid of the http
/// handlers defined below...
struct Message {
    msg: String,
}

impl warp::Reply for Message {
    fn into_response(self) -> warp::reply::Response {
        Response::new(format!("message: {}", self.msg).into())
    }
}

/// websocket upgrade handler. accepts an http connection and upgrades it to WebSocket.
/// https://developer.mozilla.org/en-US/docs/Web/HTTP/Headers/Upgrade
/// Thanks, Ryan Dahl!!
pub async fn ws_upgrade_handler(
    ws: warp::ws::Ws,
    peer_db_lock: Arc<RwLock<PeersDB>>,
    wallet_lock: Arc<RwLock<Wallet>>,
    mempool_lock: Arc<RwLock<Mempool>>,
    blockchain_lock: Arc<RwLock<Blockchain>>,
) -> std::result::Result<impl Reply, Rejection> {
    Ok(ws.on_upgrade(move |socket| {
        handle_inbound_peer_connection(
            socket,
            peer_db_lock,
            wallet_lock,
            mempool_lock,
            blockchain_lock,
        )
    }))
}
/// POST tx filter.
/// TODO remove this? I believe we want ot use the socket for everything...
pub async fn post_transaction_handler(
    mut body: impl Buf,
    mempool_lock: Arc<RwLock<Mempool>>,
    blockchain_lock: Arc<RwLock<Blockchain>>,
) -> Result<impl Reply> {
    let mut buffer = vec![];
    while body.has_remaining() {
        buffer.append(&mut body.chunk().to_vec());
        let cnt = body.chunk().len();
        body.advance(cnt);
    }

    let mut tx = Transaction::deserialize_from_net(buffer);
    let blockchain = blockchain_lock.read().await;
    tx.generate_metadata(tx.inputs[0].get_publickey());
    if tx.validate(&blockchain.utxoset, &blockchain.staking) {
        let response = std::str::from_utf8(&tx.get_signature().to_base58().as_bytes())
            .unwrap()
            .to_string();
        let mut mempool = mempool_lock.write().await;
        mempool.add_transaction(tx).await;
        Ok(Message { msg: response })
    } else {
        Err(warp::reject::custom(Invalid))
    }
}

/// get block handler.
/// TODO remove this? I believe we want ot use the socket for everything...
pub async fn get_block_handler(str_block_hash: String) -> Result<impl Reply> {
    let mut block_hash = [0u8; 32];
    hex::decode_to_slice(str_block_hash, &mut block_hash).expect("Failed to parse hash");

    match Storage::stream_block_from_disk(block_hash).await {
        Ok(block_bytes) => Ok(block_bytes),
        Err(_err) => Err(warp::reject()),
    }
}
