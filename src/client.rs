use tokio::net::TcpStream;
use tokio::io::AsyncReadExt;
use std::io;
use crossbeam_channel::*;
use tokio::time::timeout;
use std::time::Duration;

struct ClientChannel {
    tx: Sender<u64>
}
impl ClientChannel {
    fn new(tx: Sender<u64>) -> ClientChannel {
	ClientChannel { tx }
    }

    async fn start (self) -> io::Result<()>{
        let mut transport = TcpStream::connect("127.0.0.1:6142").await?;

	loop {
	    transport.readable().await?;
	    let mut buf = [0 as u8; 8];
	    if let Ok(_) = timeout(Duration::from_millis(1), transport.read(&mut buf)).await {
    		self.tx.try_send(u64::from_le_bytes(buf));
	    }
//	    tokio::task::yield_now().await;
	}
    }
    
}
pub struct Client {
    rx: Receiver<u64>
}
impl Client {
    pub fn new() -> Client {
	let (tx, rx) = bounded(1);
	let cc = ClientChannel::new(tx);
	tokio::task::spawn(async move {
	    
	    match cc.start().await {
		Ok(()) => (),
		Err(message) => println!("{}", message)
	    }
	});
    
	Client { rx }
    }
    pub fn recv_next_beat_frame(&self) -> Result<u64, RecvError> {
	self.rx.recv()
    }
    pub fn try_recv_next_beat_frame(&self) -> Result<u64, TryRecvError> {
	self.rx.try_recv()
    }
}
