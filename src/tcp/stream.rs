use maidsafe_utilities::serialisation;
use priv_prelude::*;
use rendezvous_addr::{rendezvous_addr, RendezvousAddrError};
use std::error::Error;
use tcp::builder::TcpBuilderExt;

const RENDEZVOUS_TIMEOUT_SEC: u64 = 10;
const RENDEZVOUS_INFO_EXCHANGE_TIMEOUT_SEC: u64 = 120;

#[derive(Debug, Serialize, Deserialize)]
pub enum TcpRendezvousMsg {
    Init {
        enc_pk: PublicEncryptKey,
        rendezvous_addr: SocketAddr,
    },
}

quick_error! {
    /// Errors returned by `TcpStreamExt::connect_reusable`.
    #[derive(Debug)]
    pub enum ConnectReusableError {
        /// Failure to bind socket to address.
        Bind(e: io::Error) {
            description("error binding to port")
            display("error binding to port: {}", e)
            cause(e)
        }
        /// Connection failure.
        Connect(e: io::Error) {
            description("error connecting")
            display("error connecting: {}", e)
            cause(e)
        }
    }
}

/// Errors returned by `TcpStreamExt::rendezvous_connect`.
#[derive(Debug)]
pub enum TcpRendezvousConnectError<Ei, Eo> {
    /// Failure to bind socket to some address.
    Bind(io::Error),
    /// Failure to get socket bind addresses.
    IfAddrs(io::Error),
    /// Rendezvous connection info exchange channel was closed.
    ChannelClosed,
    /// Rendezvous connection info exchange timed out.
    ChannelTimedOut,
    /// Failure to read from rendezvous connection info exchange channel.
    ChannelRead(Ei),
    /// Failure to write to rendezvous connection info exchange channel.
    ChannelWrite(Eo),
    /// Failure to serialize message sent via rendezvous channel
    SerializeMsg(SerialisationError),
    /// Failure to deserialize  message received via rendezvous channel
    DeserializeMsg(SerialisationError),
    /// Failure to encrypt message
    Encrypt(EncryptionError),
    /// Failure to decrypt message from remote peer
    Decrypt(EncryptionError),
    /// Used when all rendezvous connection attempts failed.
    AllAttemptsFailed(Vec<SingleRendezvousAttemptError>),
    /// Failure to get rendezvous address.
    RendezvousAddrError(RendezvousAddrError),
}

impl<Ei, Eo> fmt::Display for TcpRendezvousConnectError<Ei, Eo>
where
    Ei: Error,
    Eo: Error,
{
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        use TcpRendezvousConnectError::*;
        write!(f, "{}. ", self.description())?;
        match *self {
            Bind(ref e) | IfAddrs(ref e) => {
                write!(f, "IO error: {}", e)?;
            }
            ChannelClosed | ChannelTimedOut => (),
            ChannelRead(ref e) => {
                write!(f, "channel error: {}", e)?;
            }
            ChannelWrite(ref e) => {
                write!(f, "channel error: {}", e)?;
            }
            SerializeMsg(ref e) => {
                write!(f, "error serializing message: {}", e)?;
            }
            DeserializeMsg(ref e) => {
                write!(f, "error deserializing message: {}", e)?;
            }
            Encrypt(ref e) => {
                write!(f, "error encrypting message: {}", e)?;
            }
            Decrypt(ref e) => {
                write!(f, "error decrypting message: {}", e)?;
            }
            AllAttemptsFailed(ref attempt_errors) => {
                write!(
                    f,
                    "All {} connection attempts failed with errors: {:#?}",
                    attempt_errors.len(),
                    attempt_errors
                )?;
            }
            RendezvousAddrError(ref e) => {
                write!(f, "Failed to find rendezvous address: {}", e)?;
            }
        }
        Ok(())
    }
}

impl<Ei, Eo> Error for TcpRendezvousConnectError<Ei, Eo>
where
    Ei: Error,
    Eo: Error,
{
    fn description(&self) -> &'static str {
        use TcpRendezvousConnectError::*;
        match *self {
            Bind(..) => "error binding to local address",
            IfAddrs(..) => "error getting network interface addresses",
            ChannelClosed => "rendezvous channel closed unexpectedly",
            ChannelTimedOut => "timed out waiting for message via rendezvous channel",
            ChannelRead(..) => "error reading from rendezvous channel",
            ChannelWrite(..) => "error writing to rendezvous channel",
            SerializeMsg(..) => "error serializing rendezvous message",
            DeserializeMsg(..) => "error deserializing rendezvous message",
            Encrypt(..) => "error encrypting message to send to remote peer",
            Decrypt(..) => "error decrypting message received from remote peer",
            AllAttemptsFailed(..) => "all attempts to connect to the remote host failed",
            RendezvousAddrError(..) => "failed to find rendezvous address",
        }
    }

    fn cause(&self) -> Option<&Error> {
        use TcpRendezvousConnectError::*;
        match *self {
            Bind(ref e) | IfAddrs(ref e) => Some(e),
            ChannelRead(ref e) => Some(e),
            ChannelWrite(ref e) => Some(e),
            SerializeMsg(ref e) => Some(e),
            DeserializeMsg(ref e) => Some(e),
            Encrypt(ref e) => Some(e),
            Decrypt(ref e) => Some(e),
            RendezvousAddrError(ref e) => Some(e),
            ChannelClosed | ChannelTimedOut | AllAttemptsFailed(..) => None,
        }
    }
}

quick_error! {
    #[derive(Debug)]
    pub enum SingleRendezvousAttemptError {
        Connect(e: ConnectReusableError) {
            description("error performing reusable connect")
            display("error performing reusable connect: {}", e)
            cause(e)
        }
        Accept(e: io::Error) {
            description("error accepting incoming stream")
            display("error accepting incoming stream: {}", e)
            cause(e)
        }
        Write(e: io::Error) {
            description("error writing handshake to connection candidate socket")
            display("error writing handshake to connection candidate socket: {}", e)
            cause(e)
        }
        Read(e: io::Error) {
            description("error reading handshake on connection candidate socket")
            display("error reading handshake on connection candidate socket: {}", e)
            cause(e)
        }
        Decrypt(e: EncryptionError) {
            description("error decrypting data")
            display("error decrypting data: {:?}", e)
            cause(e)
        }
        Encrypt(e: SerialisationError) {
            description("error decrypting data")
            display("error decrypting data: {:?}", e)
            cause(e)
        }
    }
}

/// Extension methods for `TcpStream`.
pub trait TcpStreamExt {
    /// Connect to `addr` using a reusably-bound socket, bound to `bind_addr`. This can be used to
    /// create multiple TCP connections with the same local address, or with the same local address
    /// as a reusably-bound `TcpListener`.
    fn connect_reusable(
        bind_addr: &SocketAddr,
        addr: &SocketAddr,
        handle: &Handle,
    ) -> BoxFuture<TcpStream, ConnectReusableError>;

    /// Perform a TCP rendezvous connect. Both peers must call this method simultaneously in order
    /// to form one TCP connection, connected from both ends. `channel` must provide a channel
    /// through which the two connecting peers can communicate with each other out-of-band while
    /// negotiating the connection.
    fn rendezvous_connect<C>(channel: C, handle: &Handle, mc: &P2p) -> TcpRendezvousConnect<C>
    where
        C: Stream<Item = Bytes>,
        C: Sink<SinkItem = Bytes>,
        <C as Stream>::Error: fmt::Debug,
        <C as Sink>::SinkError: fmt::Debug,
        C: 'static;
}

impl TcpStreamExt for TcpStream {
    fn connect_reusable(
        bind_addr: &SocketAddr,
        addr: &SocketAddr,
        handle: &Handle,
    ) -> BoxFuture<TcpStream, ConnectReusableError> {
        let try = || {
            let builder =
                { TcpBuilder::bind_reusable(bind_addr).map_err(ConnectReusableError::Bind)? };
            let stream = unwrap!(builder.to_tcp_stream());
            Ok({
                TcpStream::connect_stream(stream, addr, handle)
                    .map_err(ConnectReusableError::Connect)
            })
        };

        future::result(try()).flatten().into_boxed()
    }

    fn rendezvous_connect<C>(channel: C, handle: &Handle, mc: &P2p) -> TcpRendezvousConnect<C>
    where
        C: Stream<Item = Bytes>,
        C: Sink<SinkItem = Bytes>,
        <C as Stream>::Error: fmt::Debug,
        <C as Sink>::SinkError: fmt::Debug,
        C: 'static,
    {
        // TODO(canndrew): In the current implementation, we send all data in the first message
        // along the channel. This is because we can't (currently) rely on routing to forward
        // anything other than the first message to the other peer.

        let handle0 = handle.clone();
        let (our_pk, our_sk) = gen_encrypt_keypair();

        let try = || {
            trace!("starting tcp rendezvous connect");
            let listener = {
                TcpListener::bind_reusable(&addr!("0.0.0.0:0"), &handle0)
                    .map_err(TcpRendezvousConnectError::Bind)
            }?;
            let bind_addr = {
                listener
                    .local_addr()
                    .map_err(TcpRendezvousConnectError::Bind)?
            };

            Ok({
                trace!("getting rendezvous address");
                rendezvous_addr(Protocol::Tcp, &bind_addr, &handle0, mc)
                    .map_err(TcpRendezvousConnectError::RendezvousAddrError)
                    .and_then(move |(our_rendezvous_addr, _nat_type)| {
                        trace!("got rendezvous address: {}", our_rendezvous_addr);
                        let msg = TcpRendezvousMsg::Init {
                            enc_pk: our_pk,
                            rendezvous_addr: our_rendezvous_addr,
                        };

                        trace!("exchanging rendezvous info with peer");

                        exchange_conn_info(channel, &handle0, &msg).and_then(move |msg| {
                            let TcpRendezvousMsg::Init {
                                enc_pk: their_pk,
                                rendezvous_addr: their_rendezvous_addr,
                            } = msg;

                            let connector = TcpStream::connect_reusable(
                                &bind_addr,
                                &their_rendezvous_addr,
                                &handle0,
                            ).map_err(SingleRendezvousAttemptError::Connect);
                            let incoming = {
                                listener
                                    .incoming()
                                    .map(|(stream, _addr)| stream)
                                    .map_err(SingleRendezvousAttemptError::Accept)
                                    .until({
                                        Timeout::new(
                                            Duration::from_secs(RENDEZVOUS_TIMEOUT_SEC),
                                            &handle0,
                                        ).infallible()
                                    })
                            };
                            let all_incoming =
                                connector.into_stream().select(incoming).into_boxed();
                            choose_connections(all_incoming, &their_pk, &our_sk, &our_pk)
                                .map(move |tcp_stream| (tcp_stream, our_rendezvous_addr))
                        })
                    })
            })
        };

        TcpRendezvousConnect {
            inner: future::result(try()).flatten().into_boxed(),
        }
    }
}

fn exchange_conn_info<C>(
    channel: C,
    handle: &Handle,
    msg: &TcpRendezvousMsg,
) -> BoxFuture<TcpRendezvousMsg, TcpRendezvousConnectError<C::Error, C::SinkError>>
where
    C: Stream<Item = Bytes>,
    C: Sink<SinkItem = Bytes>,
    <C as Stream>::Error: fmt::Debug,
    <C as Sink>::SinkError: fmt::Debug,
    C: 'static,
{
    let handle = handle.clone();
    let msg =
        try_bfut!(serialisation::serialise(&msg).map_err(TcpRendezvousConnectError::SerializeMsg));
    let msg = Bytes::from(msg);
    channel
        .send(msg)
        .map_err(TcpRendezvousConnectError::ChannelWrite)
        .and_then(move |channel| {
            channel
                .map_err(TcpRendezvousConnectError::ChannelRead)
                .next_or_else(|| TcpRendezvousConnectError::ChannelClosed)
                .with_timeout(
                    Duration::from_secs(RENDEZVOUS_INFO_EXCHANGE_TIMEOUT_SEC),
                    &handle,
                ).and_then(|opt| opt.ok_or(TcpRendezvousConnectError::ChannelTimedOut))
                .and_then(|(msg, _channel)| {
                    serialisation::deserialise(&msg)
                        .map_err(TcpRendezvousConnectError::DeserializeMsg)
                })
        }).into_boxed()
}

#[derive(Debug, Serialize, Deserialize)]
struct ChooseMessage;

/// Finalizes rendezvous connection with sending special message 'choose'.
/// Only one peer sends this message while the other receives and validates it. Who is who is
/// determined by public keys.
fn choose_connections<Ei: 'static, Eo: 'static>(
    all_incoming: BoxStream<TcpStream, SingleRendezvousAttemptError>,
    their_pk: &PublicEncryptKey,
    our_sk: &SecretEncryptKey,
    our_pk: &PublicEncryptKey,
) -> BoxFuture<TcpStream, TcpRendezvousConnectError<Ei, Eo>> {
    let shared_secret = our_sk.shared_secret(&their_pk);
    let encrypted_msg = try_bfut!(
        shared_secret
            .encrypt(&ChooseMessage)
            .map_err(TcpRendezvousConnectError::Encrypt)
    );

    if our_pk > their_pk {
        all_incoming
            .and_then(move |stream| {
                trace!(
                    "sending choose from {:?} to {:?}",
                    stream.local_addr(),
                    stream.peer_addr()
                );
                let framed = FramedUnbuffered::new(stream);
                let encrypted_msg = Bytes::from(&encrypted_msg[..]);
                framed
                    .send(encrypted_msg.clone())
                    .map_err(SingleRendezvousAttemptError::Write)
                    .map(|framed| unwrap!(framed.into_inner()))
            }).into_boxed()
    } else {
        all_incoming
            .and_then(move |stream| {
                trace!(
                    "trying to receive choose on {:?} from {:?}",
                    stream.local_addr(),
                    stream.peer_addr()
                );
                let framed = FramedUnbuffered::new(stream);
                recv_choose_conn_msg(framed, shared_secret.clone())
            }).filter_map(|stream_opt| stream_opt)
            .into_boxed()
    }.first_ok()
    .map_err(TcpRendezvousConnectError::AllAttemptsFailed)
    .into_boxed()
}

/// Receives incoming data stream and check's if it's connection choose message.
/// If it is, returns the stream. Otherwise None is returned.
fn recv_choose_conn_msg(
    framed: FramedUnbuffered<TcpStream>,
    shared_secret: SharedSecretKey,
) -> BoxFuture<Option<TcpStream>, SingleRendezvousAttemptError> {
    framed
        .into_future()
        .map_err(|(e, _framed)| SingleRendezvousAttemptError::Read(e))
        .and_then(move |(msg_opt, framed)| {
            let msg = match msg_opt {
                Some(msg) => msg,
                None => return future::ok(None).into_boxed(),
            };
            let _decrypted_msg: ChooseMessage = try_bfut!(
                shared_secret
                    .decrypt(&msg)
                    .map_err(SingleRendezvousAttemptError::Decrypt)
            );
            future::ok(Some(unwrap!(framed.into_inner()))).into_boxed()
        }).into_boxed()
}

/// TCP stream and it's public rendezvous address.
type RendezvousConnectResult = (TcpStream, SocketAddr);

/// Future that yields `TcpStream` and our public address, if one was detected.
pub struct TcpRendezvousConnect<C>
where
    C: Stream<Item = Bytes>,
    C: Sink<SinkItem = Bytes>,
    C: 'static,
{
    inner: BoxFuture<RendezvousConnectResult, TcpRendezvousConnectError<C::Error, C::SinkError>>,
}

impl<C> Future for TcpRendezvousConnect<C>
where
    C: Stream<Item = Bytes>,
    C: Sink<SinkItem = Bytes>,
    C: 'static,
{
    type Item = RendezvousConnectResult;
    type Error = TcpRendezvousConnectError<C::Error, C::SinkError>;

    fn poll(
        &mut self,
    ) -> Result<Async<Self::Item>, TcpRendezvousConnectError<C::Error, C::SinkError>> {
        self.inner.poll()
    }
}
