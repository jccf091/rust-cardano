use crate::{gen, service::NodeService};

use network_core::server::Node;

use futures::prelude::*;
use tokio::{
    net::{TcpListener, TcpStream},
    runtime::current_thread::TaskExecutor,
};

use std::{error::Error as ErrorTrait, fmt, net::SocketAddr};

/// The gRPC server for the blockchain node.
///
/// This type encapsulates the gRPC protocol server providing the
/// Node service. The application instantiates a `Server` wrapping a
/// blockchain service implementation satisfying the abstract network
/// service trait `Node`.
pub struct Server<T>
where
    T: Node,
    <T as Node>::BlockService: Clone,
    <T as Node>::TransactionService: Clone,
{
    h2: tower_h2::Server<
        gen::node::server::NodeServer<NodeService<T>>,
        TaskExecutor,
        gen::node::server::node::ResponseBody<NodeService<T>>,
    >,
}

/// Connection of a client peer to the gRPC server.
pub struct Connection<T>(
    tower_h2::server::Connection<
        TcpStream,
        gen::node::server::NodeServer<NodeService<T>>,
        TaskExecutor,
        gen::node::server::node::ResponseBody<NodeService<T>>,
        (),
    >,
)
where
    T: Node,
    <T as Node>::BlockService: Clone,
    <T as Node>::TransactionService: Clone;

impl<T> Future for Connection<T>
where
    T: Node + 'static,
    <T as Node>::BlockService: Clone,
    <T as Node>::TransactionService: Clone,
{
    type Item = ();
    type Error = Error;
    fn poll(&mut self) -> Poll<(), Error> {
        self.0.poll().map_err(|e| e.into())
    }
}

impl<T> Server<T>
where
    T: Node + 'static,
    <T as Node>::BlockService: Clone,
    <T as Node>::TransactionService: Clone,
{
    /// Creates a server instance around the node service implementation.
    pub fn new(node: T) -> Self {
        let grpc_service = gen::node::server::NodeServer::new(NodeService::new(node));
        let h2 = tower_h2::Server::new(grpc_service, Default::default(), TaskExecutor::current());
        Server { h2 }
    }

    /// Initializes a client peer connection based on an accepted TCP
    /// connection socket.
    /// The socket can be obtained from a stream returned by `listen`.
    pub fn serve(&mut self, sock: TcpStream) -> Connection<T> {
        Connection(self.h2.serve(sock))
    }
}

/// Sets up a listening TCP socket bound to the given address.
/// If successful, returns an asynchronous stream of `TcpStream` socket
/// objects representing accepted TCP connections from clients.
/// The TCP_NODELAY option is disabled on the returned sockets as
/// necessary for the HTTP/2 protocol.
pub fn listen(
    addr: &SocketAddr,
) -> Result<impl Stream<Item = TcpStream, Error = tokio::io::Error>, tokio::io::Error> {
    let listener = TcpListener::bind(&addr)?;
    let stream = listener.incoming().and_then(|sock| {
        sock.set_nodelay(true)?;
        Ok(sock)
    });
    Ok(stream)
}

/// The error type for gRPC server operations.
#[derive(Debug)]
pub enum Error {
    Io(tokio::io::Error),
    Http2Handshake(h2::Error),
    Http2Protocol(h2::Error),
    NewService(h2::Error),
    Service(h2::Error),
    Execute,
}

impl From<tokio::io::Error> for Error {
    fn from(err: tokio::io::Error) -> Self {
        Error::Io(err)
    }
}

type H2Error<T> = tower_h2::server::Error<gen::node::server::NodeServer<NodeService<T>>>;

// Incorporating tower_h2::server::Error would be too cumbersome due to the
// type parameter baggage, see https://github.com/tower-rs/tower-h2/issues/50
// So we match it into our own variants.
impl<T> From<H2Error<T>> for Error
where
    T: Node,
    <T as Node>::BlockService: Clone,
    <T as Node>::TransactionService: Clone,
{
    fn from(err: H2Error<T>) -> Self {
        use tower_h2::server::Error::*;
        match err {
            Handshake(e) => Error::Http2Handshake(e),
            Protocol(e) => Error::Http2Protocol(e),
            NewService(e) => Error::NewService(e),
            Service(e) => Error::Service(e),
            Execute => Error::Execute,
        }
    }
}

impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        match self {
            Error::Io(e) => write!(f, "I/O error: {}", e),
            Error::Http2Handshake(e) => write!(f, "HTTP/2 handshake error: {}", e),
            Error::Http2Protocol(e) => write!(f, "HTTP/2 protocol error: {}", e),
            Error::NewService(e) => write!(f, "service creation error: {}", e),
            Error::Service(e) => write!(f, "error returned by service: {}", e),
            Error::Execute => write!(f, "task execution error"),
        }
    }
}

impl ErrorTrait for Error {
    fn source(&self) -> Option<&(dyn ErrorTrait + 'static)> {
        match self {
            Error::Io(e) => Some(e),
            Error::Http2Handshake(e) => Some(e),
            Error::Http2Protocol(e) => Some(e),
            Error::NewService(e) => Some(e),
            Error::Service(e) => Some(e),
            Error::Execute => None,
        }
    }
}