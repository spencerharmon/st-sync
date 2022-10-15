use tokio::net::TcpStream;
use tokio::io::AsyncReadExt;
use std::io;

pub struct Client {}
impl Client {
    pub fn new() -> Client {
	Client {  }
    }

    pub async fn start (self) -> io::Result<()>{

        let mut stream = TcpStream::connect("127.0.0.1:6142").await?;

	loop {
	    let mut buffer = Vec::new();

	    stream.read_to_end(&mut buffer).await?;
	    if !buffer.is_empty() {
    	        println!("{:?}", buffer);
	    }
	}
    }
}
