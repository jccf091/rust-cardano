use chain_core::property::{Block, HasHeader, Header, TransactionId};
use future::Either;
use futures::{sync::mpsc, sync::oneshot};
use network_core::client::{self as core_client, block::BlockService, block::HeaderService};
use protocol::{
    network_transport::LightWeightConnectionId, protocol::BlockHeaders, protocol::GetBlockHeaders,
    protocol::GetBlocks, Inbound, InboundError, InboundStream, Message,
    OutboundError, OutboundSink, Response,
};
use std::collections::HashMap;
use std::marker::PhantomData;
use std::net::SocketAddr;
use tokio::net::TcpStream;
use tokio::prelude::*;

/// A handle that can be used in order for communication
/// with the client thread.
pub struct ClientHandle<T: Block + HasHeader, Tx> {
    channel: mpsc::UnboundedSender<Request<T>>,
    phantom: PhantomData<Tx>,
}

/// Connect to the remote client. Returns future that can
/// be run on any executor.
pub fn connect<B, H, I, D, Tx>(
    sockaddr: SocketAddr,
) -> impl Future<
    Item = (impl Future<Item = (), Error = ()>, ClientHandle<B, Tx>),
    Error = core_client::Error,
>
where
    Tx: TransactionId + cbor_event::Serialize + cbor_event::Deserialize,
    B: NttBlock<D, I, H>,
    H: NttHeader<D, I>,
    I: NttId,
    D: NttDate,
{
    TcpStream::connect(&sockaddr)
        .map_err(move |err| core_client::Error::new(core_client::ErrorKind::Rpc, err))
        .and_then(move |stream| {
            protocol::Connection::connect(stream)
                .map_err(move |err| core_client::Error::new(core_client::ErrorKind::Rpc, err))
                .and_then(move |connection: protocol_tokio::Connection<_, B, Tx>| {
                    let (stream, source) = mpsc::unbounded();
                    let handle = ClientHandle {
                        channel: stream,
                        phantom: PhantomData,
                    };
                    future::ok((run_connection(connection, source), handle))
                })
        })
}

/// Internal message that is used to load reply from the client.
pub struct RequestFuture<T>(oneshot::Receiver<Result<T, core_client::Error>>);

impl<T> Future for RequestFuture<T> {
    type Item = T;
    type Error = core_client::Error;

    fn poll(&mut self) -> Poll<T, Self::Error> {
        match self.0.poll() {
            Ok(Async::Ready(Ok(x))) => Ok(Async::Ready(x)),
            Ok(Async::Ready(Err(x))) => Err(x),
            Ok(Async::NotReady) => Ok(Async::NotReady),
            Err(e) => Err(core_client::Error::new(core_client::ErrorKind::Rpc, e)),
        }
    }
}

pub struct RequestStream<T>(oneshot::Receiver<T>);

impl<T> Stream for RequestStream<T> {
    type Item = T;
    type Error = core_client::Error;

    fn poll(&mut self) -> Poll<Option<T>, Self::Error> {
        match self.0.poll() {
            _ => unimplemented!(),
        }
    }
}

pub struct PullBlocksToTip<T: Block + HasHeader> {
    chan: TipFuture<T::Header>,
    from: T::Id,
    request: mpsc::UnboundedSender<Request<T>>,
}

impl<T: Block + HasHeader> Future for PullBlocksToTip<T>
where
    T::Header: Header<Id = <T as Block>::Id, Date = <T as Block>::Date>,
{
    type Item = PullBlocksToTipStream<T>;
    type Error = core_client::Error;

    fn poll(&mut self) -> Poll<PullBlocksToTipStream<T>, Self::Error> {
        match self.chan.poll() {
            Ok(Async::Ready((tip, _date))) => {
                let (sender, receiver) = mpsc::unbounded();
                self.request
                    .unbounded_send(Request::Block(sender, self.from.clone(), tip))
                    .unwrap();
                let stream = PullBlocksToTipStream { channel: receiver };
                Ok(Async::Ready(stream))
            }
            Ok(Async::NotReady) => Ok(Async::NotReady),
            Err(e) => Err(core_client::Error::new(core_client::ErrorKind::Rpc, e)),
        }
    }
}

pub struct PullBlocksToTipStream<T> {
    channel: mpsc::UnboundedReceiver<Result<T, core_client::Error>>,
}

impl<T: Block> Stream for PullBlocksToTipStream<T> {
    type Item = T;
    type Error = core_client::Error;

    fn poll(&mut self) -> Poll<Option<Self::Item>, Self::Error> {
        match self.channel.poll() {
            Ok(Async::Ready(None)) => Ok(Async::Ready(None)),
            Ok(Async::Ready(Some(Ok(block)))) => Ok(Async::Ready(Some(block))),
            Ok(Async::Ready(Some(Err(err)))) => Err(err),
            Ok(Async::NotReady) => Ok(Async::NotReady),
            Err(_) => Err(core_client::Error::new(
                core_client::ErrorKind::Rpc,
                "error reading from unbounded channel",
            )),
        }
    }
}

pub struct TipFuture<T>(RequestFuture<T>);

impl<T: Header> Future for TipFuture<T> {
    type Item = (T::Id, T::Date);
    type Error = core_client::Error;

    fn poll(&mut self) -> Poll<(T::Id, T::Date), Self::Error> {
        match self.0.poll() {
            Ok(Async::Ready(hdr)) => {
                let id = hdr.id();
                let date = hdr.date();
                Ok(Async::Ready((id, date)))
            }
            Ok(Async::NotReady) => Ok(Async::NotReady),
            Err(e) => Err(core_client::Error::new(core_client::ErrorKind::Rpc, e)),
        }
    }
}



impl<T: Block + HasHeader, Tx> BlockService<T> for ClientHandle<T, Tx>
where
    T::Header: Header<Id = <T as Block>::Id, Date = <T as Block>::Date>,
{
    type TipFuture = TipFuture<T::Header>;
    type PullBlocksToTipStream = PullBlocksToTipStream<T>;
    type PullBlocksToTipFuture = PullBlocksToTip<T>;
    type GetBlocksStream = RequestStream<T>;
    type GetBlocksFuture = RequestFuture<RequestStream<T>>;

    fn tip(&mut self) -> Self::TipFuture {
        let (source, sink) = oneshot::channel();
        self.channel.unbounded_send(Request::Tip(source)).unwrap();
        TipFuture(RequestFuture(sink))
    }

    fn pull_blocks_to_tip(&mut self, from: &[T::Id]) -> Self::PullBlocksToTipFuture {
        let (source, sink) = oneshot::channel();
        self.channel.unbounded_send(Request::Tip(source)).unwrap();
        PullBlocksToTip {
            chan: TipFuture(RequestFuture(sink)),
            from: from[0].clone(),
            request: self.channel.clone(),
        }
    }
}

impl<T: Block, Tx> HeaderService<T> for ClientHandle<T, Tx>
where
    T: HasHeader,
{
    //type GetHeadersStream = Self::GetHeadersStream<T::Header>;
    //type GetHeadersFuture = Self::GetHeaders<T::Header>;
    type GetTipFuture = RequestFuture<T::Header>;

    fn tip_header(&mut self) -> Self::GetTipFuture {
        let (source, sink) = oneshot::channel();
        self.channel.unbounded_send(Request::Tip(source)).unwrap();
        RequestFuture(sink)
    }
}

enum Request<T: Block + HasHeader> {
    Tip(oneshot::Sender<Result<T::Header, core_client::Error>>),
    Block(
        mpsc::UnboundedSender<Result<T, core_client::Error>>,
        T::Id,
        T::Id,
    ),
}

pub trait NttBlock<D, I, H>:
    Block<Id = I, Date = D>
    + core::fmt::Debug
    + HasHeader<Header = H>
    + cbor_event::Deserialize
    + cbor_event::Serialize
where
    D: NttDate,
    I: NttId,
    H: NttHeader<D, I> + Clone + core::fmt::Debug,
{
}

impl<D, I, H, T> NttBlock<D, I, H> for T
where
    T: Block<Id = I, Date = D>
        + core::fmt::Debug
        + HasHeader<Header = H>
        + cbor_event::Deserialize
        + cbor_event::Serialize,
    D: NttDate,
    I: NttId,
    H: NttHeader<D, I> + Clone + core::fmt::Debug,
{
}

pub trait NttHeader<D, I>:
    Header<Id = I, Date = D> + cbor_event::Deserialize + cbor_event::Serialize + core::fmt::Debug + Clone
where
    D: chain_core::property::BlockDate + core::fmt::Debug,
    I: cbor_event::Deserialize
        + cbor_event::Serialize
        + chain_core::property::BlockId
        + core::fmt::Debug,
{
}

impl<D, I, T> NttHeader<D, I> for T
where
    T: Header<Id = I, Date = D> + cbor_event::Deserialize + cbor_event::Serialize + core::fmt::Debug + Clone,
    D: chain_core::property::BlockDate + core::fmt::Debug,
    I: cbor_event::Deserialize
        + cbor_event::Serialize
        + chain_core::property::BlockId
        + core::fmt::Debug,
{
}

pub trait NttDate: chain_core::property::BlockDate + core::fmt::Debug {}

impl<T> NttDate for T where T: chain_core::property::BlockDate + core::fmt::Debug {}

pub trait NttId:
    cbor_event::Deserialize + cbor_event::Serialize + chain_core::property::BlockId + core::fmt::Debug
{
}

impl<T> NttId for T where
    T: cbor_event::Deserialize
        + cbor_event::Serialize
        + chain_core::property::BlockId
        + core::fmt::Debug
{
}

#[derive(Debug)]
pub enum Error {
    Inbound(InboundError),
    Outbound(OutboundError),
}

impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        match self {
            Error::Inbound(e) => write!("network input error"),
            Error::Outbound(e) => write!("network output error"),
        }
    }
}

impl error::Error for Error {
    fn source(&self) -> Option<&(dyn error::Error + 'static)> {
        match self {
            Error::Inbound(e) => Some(e),
            Error::Outbound(e) => Some(e),
        }
    }
}

impl From<InboundError> for Error {
    fn from(err: InboundError) -> Self {
        Error::Inbound(err)
    }
}

impl From<OutboundError> for Error {
    fn from(err: OutboundError) -> Self {
        Error::Outbound(err)
    }
}

pub struct Connection<T, B, Tx>
{
    inbound: Option<InboundStream<T, B, Tx>>,
    outbound: OutboundSink<T, B, Tx>,
    requests: HashMap<LightWeightConnectionId, Request<B>>,
}

impl<T, B, Tx> Connection<T, B, Tx>
where
    T: AsyncRead + AsyncWrite,
    B: Block + HasHeader,
    Tx: TransactionId,
{
    pub fn new() -> Self {
        Connection {
            requests: HashMap::new(),
        }
    }
}

impl<B: Block + HasHeader> Future for Connection<B> {
    type Item = ();
    type Error = Error;

    fn poll(&mut self) -> Poll<(), Self::Error> {
        while let Some(ref mut inbound) = self.inbound {
            let mut events_processed = false;
            match inbound.poll() {
                Ok(Async::NotReady) => {}
                Ok(Async::Ready(None)) => {
                    break;
                }
                Ok(Async::Ready(Some(msg))) => {
                    self.process_inbound(msg);
                    events_processed = true;
                }
                Err(err) => {
                    return Err(err.into())
                }
            }
            match self.requests.poll() {
                Ok(Async::NotReady) => {}
                Ok(Async::Ready(Some(req))) => {
                    self.process_request(request);
                    events_processed = true;
                }
                Ok(Async::Ready(None)) => {
                    break;
                }
                Err(err) => panic!("unexpected error in command queue: {:?}", err),
            }
            match self.outbound.poll_complete() {
                Ok(Async::NotReady) => {}
                Ok(Async::Ready(())) => {}
                Err(err) => {
                    return Err(err.into())
                }
            }
            if !events_processed {
                return Ok(Async::NotReady);
            }
        }

        // We only get here if the inbound stream or the request queue
        // is closed.
        // Make sure the inbound half is dropped.
        self.inbound.take();

        // Manage shutdown of the outbound half,
        // returning result as the result of the connection poll.
        try_ready!(self.outbound.close())
    }
}

fn unexpected_response_error() -> core_client::Error {
    core_client::Error::new(
        core_client::ErrorKind::Rpc,
        "unexpected response".into(),
    )
}

fn convert_response<P, F>(
    response: Response<P, String>,
    conversion: F
) -> Result<Q, core_client::Error>
where
    F: FnOnce(P) -> Q,
{
    match response {
        Response::Ok(x) => Ok(conversion(x)),
        Response::Err(err) => {
            Err(core_client::Error::new(
                core_client::ErrorKind::Rpc,
                err,
            ))
        }
    }
}

impl<B: Block + HasHeader> Connection<B> {
    fn process_inbound(&mut self, inbound: Inbound) {
        match inbound {
            Inbound::NothingExciting => {}
            Inbound::BlockHeaders(lwcid, response) => {
                let request = self.requests.remove(&lwid);
                match request {
                    None => {
                        // TODO: log the bogus response
                    }
                    Some(Request::Tip(chan)) => {
                        let res = convert_response(response, |p| {
                            let header = p.0[0];
                            (header.id(), header.date())
                        });
                        chan.send(res).unwrap();
                    }
                    Some(_) => {
                        chan.send(Err(unexpected_response_error())).unwrap();
                    }
                }
            }
            Inbound::Block(lwcid, response) => {
                use hash_map::Entry::*;

                match self.requests.entry(lwcid) {
                    Vacant(_) => {
                        // TODO: log the bogus response
                    }
                    Occupied(entry) => {
                        match entry.get() {
                            Request::Block(chan) => {
                                let res = convert_response(response, |p| p);
                                chan.send(res).unwrap();
                            }
                            _ => {
                                chan.send(Err(unexpected_response_error())).unwrap();
                            }
                        }
                    }
                }
            }
            Inbound::TransactionReceived(_lwcid, _response) => {
                // TODO: to be implemented
            }
            Inbound::CloseConnection(lwcid) => {
                match self.requests.remove(&lwcid) {
                    None => {
                        // TODO: log the bogus close message
                    }
                    Some(Request::Tip(chan)) => {
                        chan.send(Err(core_client::Error::new(
                            core_client::ErrorKind::Rpc,
                            "unexpected close",
                        )))
                        .unwrap();
                    }
                    Some(Request::Block(mut chan)) => {
                        chan.close().unwrap();
                    }
                    _ => (),
                };
            }
            _ => {}
        }
    }

    fn process_request(&mut self, request: Request) {
        self.outbound.new_light_connection()
            .and_then(move |(lwcid, sink)| match request {
                Request::Tip(t) => {
                    cc.requests.insert(lwcid, Request::Tip(t));
                    future::Either::A({
                        sink.send(Message::GetBlockHeaders(
                            lwcid,
                            GetBlockHeaders {
                                from: vec![],
                                to: None,
                            },
                        ))
                        .and_then(|sink| future::ok((sink, cc)))
                    })
                }
                Request::Block(t, from, to) => {
                    let from1 = from.clone();
                    let to1 = to.clone();
                    cc.requests.insert(lwcid, Request::Block(t, from1, to1));
                    future::Either::B({
                        sink.send(Message::GetBlocks(lwcid, GetBlocks { from, to }))
                            .and_then(|sink| future::ok((sink, cc)))
                    })
                }
            })
    }
}

fn run_connection<T, B: NttBlock<D, I, H>, H: NttHeader<D, I>, I: NttId, D: NttDate, Tx>(
    connection: protocol::Connection<T, B, Tx>,
    input: mpsc::UnboundedReceiver<Request<B>>,
) -> impl future::Future<Item = (), Error = ()>
where
    T: tokio::io::AsyncRead + tokio::io::AsyncWrite,
    Tx: TransactionId + cbor_event::Serialize + cbor_event::Deserialize,
{
    let (sink, stream) = connection.split();
    let (sink_tx, sink_rx) = mpsc::unbounded();
    let sink2_tx = sink_tx.clone();
    // process messages comming from the network.
    let stream = stream
        .for_each(move |inbound| match inbound {
            Inbound::NothingExciting => future::ok(()),
            Inbound::BlockHeaders(lwcid, response) => {
                sink_tx
                    .unbounded_send(Command::BlockHeaders(lwcid, response))
                    .unwrap();
                future::ok(())
            }
            Inbound::Block(lwcid, response) => {
                sink_tx
                    .unbounded_send(Command::Blocks(lwcid, response))
                    .unwrap();
                future::ok(())
            }
            Inbound::TransactionReceived(_lwcid, _response) => future::ok(()),
            Inbound::CloseConnection(lwcid) => {
                sink_tx
                    .unbounded_send(Command::CloseConnection(lwcid))
                    .unwrap();
                future::ok(())
            }
            _ => future::ok(()),
        })
        .map_err(|_| ())
        .map(|_| ());
    // Accept all commands from the program and send that
    // further in the ppeline.
    let commands = input
        .for_each(move |request| {
            sink2_tx.unbounded_send(Command::Request(request)).unwrap();
            future::ok(())
        })
        .map_err(|_err| ())
        .map(|_| ());

    // Receive commands.
    let sink = sink
        .subscribe(false)
        .map_err(|_err| ())
        .and_then(move |(_lwcid, sink)| {
            let cc: ConnectionState<B> = ConnectionState::new();
            sink_rx
                .fold((sink, cc), move |(sink, mut cc), outbound| match outbound {
                    Command::Message(Message::AckNodeId(_lwcid, node_id)) => V::A1(
                        sink.ack_node_id(node_id)
                            .map_err(|_err| ())
                            .map(|x| (x, cc)),
                    ),
                    Command::Message(message) => {
                        V::A2(sink.send(message).map_err(|_err| ()).map(|x| (x, cc)))
                    }
                    Command::BlockHeaders(lwid, resp) => {
                        let request = cc.requests.remove(&lwid);
                        V::A3(match request {
                            Some(Request::Tip(chan)) => match resp {
                                Response::Ok(x) => {
                                    let s = x.0[0].clone();
                                    chan.send(Ok(s)).unwrap();
                                    Either::A(future::ok((sink, cc)))
                                }
                                Response::Err(x) => {
                                    chan.send(Err(core_client::Error::new(
                                        core_client::ErrorKind::Rpc,
                                        x,
                                    )))
                                    .unwrap();
                                    Either::A(future::ok((sink, cc)))
                                }
                            },
                            Some(Request::Block(chan, _, _)) => Either::B(
                                chan.send(Err(core_client::Error::new(
                                    core_client::ErrorKind::Rpc,
                                    "unexpected reply".to_string(),
                                )))
                                .map_err(|_| ())
                                .and_then(|_| future::ok((sink, cc))),
                            ),
                            None => Either::A(future::ok((sink, cc))),
                        })
                    }
                    Command::Blocks(lwid, resp) => {
                        let val = cc.requests.remove(&lwid);
                        V::A4(
                            match val {
                                Some(Request::Block(chan, a, b)) => Either::B(
                                    match resp {
                                        Response::Ok(x) => {
                                            cc.requests
                                                .insert(lwid, Request::Block(chan.clone(), a, b));
                                            chan.send(Ok(x))
                                        }
                                        Response::Err(x) => chan.send(Err(
                                            core_client::Error::new(core_client::ErrorKind::Rpc, x),
                                        )),
                                    }
                                    .map_err(|_| ())
                                    .map(|_| ()),
                                ),
                                Some(Request::Tip(chan)) => {
                                    chan.send(Err(core_client::Error::new(
                                        core_client::ErrorKind::Rpc,
                                        "unexpected response".to_string(),
                                    )))
                                    .unwrap();
                                    Either::A(future::ok(()))
                                }
                                None => Either::A(future::ok(())),
                            }
                            .and_then(move |_| {
                                sink.close_light_connection(lwid)
                                    .and_then(|x| future::ok((x, cc)))
                                    .map_err(|_| ())
                            }),
                        )
                    }
                    Command::Transaction(_, _) => V::A5(future::ok((sink, cc))),
                    Command::Request(request) => V::A6(
                        sink.new_light_connection()
                            .and_then(move |(lwcid, sink)| match request {
                                Request::Tip(t) => {
                                    cc.requests.insert(lwcid, Request::Tip(t));
                                    future::Either::A({
                                        sink.send(Message::GetBlockHeaders(
                                            lwcid,
                                            GetBlockHeaders {
                                                from: vec![],
                                                to: None,
                                            },
                                        ))
                                        .and_then(|sink| future::ok((sink, cc)))
                                    })
                                }
                                Request::Block(t, from, to) => {
                                    let from1 = from.clone();
                                    let to1 = to.clone();
                                    cc.requests.insert(lwcid, Request::Block(t, from1, to1));
                                    future::Either::B({
                                        sink.send(Message::GetBlocks(lwcid, GetBlocks { from, to }))
                                            .and_then(|sink| future::ok((sink, cc)))
                                    })
                                }
                            })
                            .map_err(|_| ()),
                    ),
                    Command::CloseConnection(lwcid) => V::A7({
                        match cc.requests.remove(&lwcid) {
                            Some(Request::Tip(chan)) => {
                                chan.send(Err(core_client::Error::new(
                                    core_client::ErrorKind::Rpc,
                                    "unexpected close",
                                )))
                                .unwrap();
                            }
                            Some(Request::Block(mut chan, _, _)) => {
                                chan.close().unwrap();
                            }
                            _ => (),
                        };
                        future::ok((sink, cc))
                    }),
                })
                .map_err(|_| ())
                .map(|_| ())
        });
    let cmds = commands.select(sink).map_err(|_err| ()).map(|_| ());

    stream.select(cmds).then(|_| Ok(()))
}
