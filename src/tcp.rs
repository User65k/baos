use crate::types::{Blind, Direction, GroupWriter, StateStore};
use std::{ops::RangeInclusive, sync::Arc};
use tokio::{
    io::{AsyncReadExt as _, AsyncWriteExt as _},
    net::{TcpListener, TcpStream},
    sync::Mutex,
};

pub async fn drive(listener: TcpListener, bus_sender: GroupWriter, state: Arc<Mutex<StateStore>>) {
    let mut buf = [0u8; 5];
    loop {
        let stream = listener.accept().await.map(|(s, _)| s);
        if let Err(e) = handle_connection(stream, &mut buf, &bus_sender, &state).await {
            println!("handle_connection failed {}", e);
        }
    }
}
async fn handle_connection(
    stream: std::io::Result<TcpStream>,
    buf: &mut [u8],
    bus: &GroupWriter,
    state: &Arc<Mutex<StateStore>>,
) -> std::io::Result<()> {
    let mut stream = match stream {
        Ok(s) => s,
        Err(e) => panic!("accept: {:?}", e),
    };
    let len = stream.read(buf).await?;
    let buf = &buf[..len];
    println!("Cmd: {:?}", String::from_utf8_lossy(buf));
    let mut i = buf.split(|&c| c == b' ');

    let target = i.next().ok_or(std::io::ErrorKind::AddrNotAvailable)?;

    let (single_step, data) = match i.next() {
        Some(b"1") | Some(b"Z") => (false, Direction::Down),
        Some(b"0") | Some(b"A") => (false, Direction::Up),
        Some(b"S") => (true, Direction::Down),
        Some(b"D") => (true, Direction::Down),
        Some(b"U") => (true, Direction::Up),
        Some(b"?") => {
            //query data
            let s = state.lock().await;
            for addr in TargetIter::new(target)? {
                if let Some((t, up, i, j)) = s.get(&addr) {
                    let mut stat = (*j).into(); //0 - 7 -> 0b111
                    if t.is_some() {
                        //its currently moving
                        match up {
                            Direction::Up => stat += 0b0011_0000,
                            Direction::Down => stat += 0b0010_0000,
                        }
                    }
                    stream.write_all(&[(*i).into(), stat]).await?;
                } else {
                    stream.write_all(&[255, 255]).await?;
                }
            }
            return Ok(());
        }
        Some(&[pos, ang]) if pos & 0x80 == 0x80 => {
            for addr in TargetIter::new(target)? {
                let bus = bus.clone();
                let state = state.clone();
                tokio::task::spawn_local(async move {
                    super::move_to_pos(addr, pos & 0x7f, ang, &bus, &state)
                        .await
                        .expect("cant send")
                });
            }
            return Ok(());
        }
        _ => {
            return Err(std::io::ErrorKind::InvalidData.into());
        }
    };
    for addr in TargetIter::new(target)? {
        bus.send((addr.to_bus_addr(single_step), data))
            .await
            .expect("send err");
    }
    Ok(())
}
enum TargetIter {
    All(RangeInclusive<u8>),
    Some(core::slice::Iter<'static, u8>),
    Single(Blind),
    None,
}
impl TargetIter {
    pub fn new(target: &[u8]) -> std::io::Result<Self> {
        Ok(match target {
            b"A" => TargetIter::All(b'a'..=b'h'),
            b"B" => TargetIter::Some([b'g', b'd'].iter()),
            b"W" => TargetIter::Some([b'h', b'f', b'e', b'c'].iter()),
            l => TargetIter::Single(get_addr(l)?),
        })
    }
}
impl Iterator for TargetIter {
    type Item = Blind;

    fn next(&mut self) -> Option<Self::Item> {
        match self {
            TargetIter::All(range_inclusive) => range_inclusive.next().map(Blind::from_port),
            TargetIter::Some(iter) => iter.next().map(|c| Blind::from_port(*c)),
            TargetIter::Single(blind) => {
                let b = Some(*blind);
                *self = TargetIter::None;
                b
            }
            TargetIter::None => None,
        }
    }
}
fn get_addr(c: &[u8]) -> std::io::Result<Blind> {
    Ok(match c {
        b"W2" => Blind::from_port(b'h'),
        b"BR" => Blind::from_port(b'g'),
        b"W1" => Blind::from_port(b'f'),
        b"W4" => Blind::from_port(b'e'),
        b"BL" => Blind::from_port(b'd'),
        b"W3" => Blind::from_port(b'c'),
        b"S" => Blind::from_port(b'b'),
        b"K" => Blind::from_port(b'a'),
        _ => {
            return Err(std::io::ErrorKind::InvalidData.into());
        }
    })
}
