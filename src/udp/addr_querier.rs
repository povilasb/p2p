use priv_prelude::*;

#[derive(Debug, Clone, Hash)]
/// A remote `UdpRendezvousServer` that we can query for our external address.
pub struct RemoteUdpRendezvousServer {
    addr: SocketAddr,
    pub_key: PublicEncryptKey,
}

impl RemoteUdpRendezvousServer {
    /// Define a new remote server.
    pub fn new(addr: SocketAddr, pub_key: PublicEncryptKey) -> RemoteUdpRendezvousServer {
        RemoteUdpRendezvousServer { addr, pub_key }
    }
}

impl UdpAddrQuerier for RemoteUdpRendezvousServer {
    #[allow(trivial_casts)] // needed for as Box<Error>
    fn query(
        &self,
        bind_addr: &SocketAddr,
        handle: &Handle,
    ) -> BoxFuture<SocketAddr, Box<Error + Send>> {
        let socket = try_bfut!(
            UdpSocket::bind_connect_reusable(bind_addr, &self.addr, handle)
                .map_err(|e| Box::new(QueryPublicAddrError::Bind(e)) as Box<Error + Send>)
        );

        let server_addr = self.addr;
        let (client_pk, client_sk) = gen_encrypt_keypair();
        let shared_secret = client_sk.shared_secret(&self.pub_key);

        let msg = EchoRequest { client_pk };
        let msg = try_bfut!(
            self.pub_key
                .anonymously_encrypt(&msg)
                .map_err(|e| Box::new(QueryPublicAddrError::Encrypt(e)) as Box<Error + Send>)
        );

        let mut timeout = Timeout::new(Duration::new(0, 0), &handle);
        future::poll_fn(move || {
            while let Async::Ready(()) = timeout.poll().void_unwrap() {
                match socket.send(&msg[..]) {
                    Ok(n) => {
                        let len = msg.len();
                        if len != n {
                            let e = io::Error::new(
                                io::ErrorKind::Other,
                                format!(
                                    "failed to send complete request. \
                                     Sent {} bytes of {}",
                                    len, n
                                ),
                            );
                            return Err(QueryPublicAddrError::SendRequest(e));
                        }
                        timeout.reset(Instant::now() + Duration::from_millis(500));
                    }
                    Err(ref e) if e.kind() == io::ErrorKind::WouldBlock => {
                        break;
                    }
                    Err(e) => return Err(QueryPublicAddrError::SendRequest(e)),
                }
            }

            loop {
                let mut buffer = [0u8; 256];
                match socket.recv_from(&mut buffer) {
                    Ok((len, recv_addr)) => {
                        if recv_addr != server_addr {
                            continue;
                        }
                        let external_addr = shared_secret
                            .decrypt(&buffer[..len])
                            .map_err(QueryPublicAddrError::Decrypt)?;
                        return Ok(Async::Ready(external_addr));
                    }
                    Err(ref e) if e.kind() == io::ErrorKind::WouldBlock => {
                        break Ok(Async::NotReady);
                    }
                    Err(e) => return Err(QueryPublicAddrError::ReadResponse(e)),
                }
            }
        }).with_timeout(Duration::from_secs(3), &handle)
        .and_then(|opt| opt.ok_or(QueryPublicAddrError::ResponseTimeout))
        .map_err(|e| Box::new(e) as Box<Error + Send>)
        .into_boxed()
    }
}
