use std::{io::{Read,Write}, os::unix::net::UnixStream, time::Duration};
fn options(tag: &str, socket: &UnixStream) {
 for duration in [Duration::from_millis(50),Duration::from_millis(1),Duration::from_micros(1),Duration::from_nanos(1)] {
  let read=socket.set_read_timeout(Some(duration));
  let write=socket.set_write_timeout(Some(duration));
  println!("{tag} duration_ns={} read={read:?} write={write:?}", duration.as_nanos());
 }
}
fn main() {
 let (mut client, mut peer)=UnixStream::pair().unwrap();
 options("connected", &client);
 peer.write_all(&[0,0,0,2,41,42]).unwrap();
 drop(peer);
 options("peer_closed_buffered", &client);
 let mut header=[0;4]; println!("header={:?}",client.read_exact(&mut header));
 options("peer_closed_body_pending", &client);
 let mut body=[0;2]; println!("body={:?}",client.read_exact(&mut body));
 options("peer_closed_after_full_frame", &client);
 println!("eof={:?}",client.read(&mut [0]));
 options("peer_closed_eof", &client);
}
