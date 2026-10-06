use std::{io::{Read,Write},os::unix::net::UnixStream,time::{Duration,Instant}};
fn main(){
 let(mut client,mut peer)=UnixStream::pair().unwrap();
 peer.write_all(&[0,0,0,2,41,42]).unwrap(); drop(peer);
 println!("closed_read_timeout={:?}",client.set_read_timeout(Some(Duration::from_millis(50))));
 println!("closed_nonblocking={:?}",client.set_nonblocking(true));
 let mut header=[0;4]; println!("buffered_header={:?}:{header:?}",client.read_exact(&mut header));
 let mut body=[0;2]; println!("buffered_body={:?}:{body:?}",client.read_exact(&mut body));
 println!("closed_eof={:?}",client.read(&mut[0]));
 println!("closed_restore_blocking={:?}",client.set_nonblocking(false));
 let(mut client,_peer)=UnixStream::pair().unwrap(); client.set_nonblocking(true).unwrap();
 let start=Instant::now(); let deadline=start+Duration::from_millis(50);let mut attempts=0;
 while Instant::now()<deadline { match client.read(&mut[0]){Err(e)if e.kind()==std::io::ErrorKind::WouldBlock=>{attempts+=1;std::thread::park_timeout(Duration::from_millis(1));},x=>panic!("unexpected {x:?}")} }
 println!("live_missing_body attempts={attempts} elapsed_us={}",start.elapsed().as_micros());
}
