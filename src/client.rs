use tokio::net::TcpStream;
use tokio::io::AsyncReadExt;
use std::io;
use crossbeam_channel::*;

pub struct Client {
    tx: Sender<u64>
}
impl Client {
    pub fn new(tx: Sender<u64>) -> Client {
	Client { tx }
    }

    pub async fn start (self) -> io::Result<()>{
        let mut transport = TcpStream::connect("127.0.0.1:6142").await?;

	loop {
	    let mut buf = [0 as u8; 8];
	    transport.read(&mut buf).await?;
    	    self.tx.send(u64::from_le_bytes(buf));	    
	}
    }
}
