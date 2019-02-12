use futures::Future;
use futures::future::Executor;
use tokio_core::reactor::Core;

use network::api::{Api, BlockRef};
use network::Result;

//to_socket_addr
use network_core::client::block::HeaderService;
use network_ntt::client as ntt;
use std::net::SocketAddr;

use cardano::{
    //block::{block, Block, BlockDate, BlockHeader, HeaderHash, RawBlock},
    block::{Block, BlockHeader, HeaderHash, RawBlock},
    tx::{TxAux, TxId},
};

pub struct NetworkCore {
    handle: ntt::ClientHandle<Block, TxId>,
    pub core: Core,
}

impl NetworkCore {
    pub fn new(sockaddr: SocketAddr) -> Result<Self> {
        println!("New network core: {}", sockaddr);
        let connecting = ntt::connect(sockaddr);
        match connecting.wait() {
            Ok((connection, handle)) => {
                println!("Connection: ok");
                let mut core = Core::new().unwrap();
                core.execute(connection.map(|_| {println!("Exited");}))
                    .unwrap();
                println!("Connection: core ok");
                Ok(NetworkCore { handle, core })
            }
            Err(_err) => unimplemented!(),
        }
    }
}

impl Api for NetworkCore {
    fn get_tip(&mut self) -> Result<BlockHeader> {
        println!("get_tip");
        self.handle.tip_header().map_err(|e| {
            println!("error {}", e);
            unreachable!()
        }).wait()
    }

    fn wait_for_new_tip(&mut self, _prev_tip: &HeaderHash) -> Result<BlockHeader> {
        panic!("not yet ready")
    }

    fn get_block(&mut self, _hash: &HeaderHash) -> Result<RawBlock> {
        panic!("not yet ready")
    }

    fn get_blocks<F>(
        &mut self,
        _from: &BlockRef,
        _incluside: bool,
        _to: &BlockRef,
        _got_block: &mut F,
    ) -> Result<()>
    where
        F: FnMut(&HeaderHash, &Block, &RawBlock) -> (),
    {
        unimplemented!("not yet ready")
    }

    fn send_transaction(&mut self, _txaux: TxAux) -> Result<bool> {
        unimplemented!("not yet ready")
    }
}
