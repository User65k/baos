use core::ptr::NonNull;
use std::convert::TryInto;
use std::ffi::CString;
use std::io::Read;
use std::net::{TcpListener, TcpStream};
use std::time::{Duration, Instant};
use std::sync::mpsc::{channel, Receiver, RecvTimeoutError, Sender};
use std::thread;

mod kdrive;
use kdrive::{
    KDrive, KDriveFT12, KDriveTelegram, KDRIVE_CEMI_L_DATA_IND, KDRIVE_MAX_GROUP_VALUE_LEN,
};

///time, up, single step, ID
type ChannelMsg = (Instant, bool, bool, u8);

fn main() {
    let listener = TcpListener::bind("0.0.0.0:1337").expect("listen port");

    let serial = CString::new("/dev/ttyAMA0").unwrap();
    let k = KDrive::new().expect("KDrive");
    let k = KDriveFT12::open(k, &serial).expect("open FT12");

	let (mut sender, receiver) = channel::<ChannelMsg>();
	
	// Spawn off an expensive computation
	let t = thread::spawn(move|| {
		track_movements(receiver);
	});

    k.register_telegram_callback(on_telegram, NonNull::new(&mut sender as *mut _));

    let mut buf = [0u8; 4];
    for stream in listener.incoming() {
        if let Err(e) = handle_connection(stream, &mut buf, &k) {
            println!("handle_connection failed {}", e);
        }
    }
}
fn handle_connection(
    stream: std::io::Result<TcpStream>,
    buf: &mut [u8],
    //serial: &CString
    k: &KDrive,
) -> std::io::Result<()> {
    let mut stream = match stream {
        Ok(s) => s,
        Err(e) => panic!("accept: {:?}", e),
    };
    let len = stream.read(buf)?;
    let buf = &buf[..len];
    println!("Cmd: {:?}", String::from_utf8_lossy(buf));
    let mut i = buf.split(|&c| c == b' ');

    let target = i.next().ok_or(std::io::ErrorKind::AddrNotAvailable)?;

    let (upper_addr, data) = match i.next() {
        Some(b"1") => (0x1000, &[1]), //zu
        Some(b"0") => (0x1000, &[0]), //auf
        Some(b"Z") => (0x1000, &[1]), //zu
        Some(b"A") => (0x1000, &[0]), //auf
        Some(b"S") => (0x1100, &[1]),
        Some(b"D") => (0x1100, &[1]), //runter
        Some(b"U") => (0x1100, &[0]), //rauf
        _ => {
            return Err(std::io::ErrorKind::InvalidData.into());
        }
    };
    match target {
        b"A" => {
            for addr in to_bus_addr(b'a')..=to_bus_addr(b'h') {
                k.group_write(addr + upper_addr, data);
            }
        }
        b"B" => {
            k.group_write(to_bus_addr(b'g') + upper_addr, data);
            k.group_write(to_bus_addr(b'd') + upper_addr, data);
        }
        b"W" => {
            for addr in &[
                to_bus_addr(b'h'),
                to_bus_addr(b'f'),
                to_bus_addr(b'e'),
                to_bus_addr(b'c'),
            ] {
                k.group_write(*addr + upper_addr, data);
            }
        }
        l => k.group_write(get_addr(l)? + upper_addr, data),
    }
    Ok(())
}
fn get_addr(c: &[u8]) -> std::io::Result<u16> {
    Ok(match c {
        b"W2" => to_bus_addr(b'h'),
        b"BR" => to_bus_addr(b'g'),
        b"W1" => to_bus_addr(b'f'),
        b"W4" => to_bus_addr(b'e'),
        b"BL" => to_bus_addr(b'd'),
        b"W3" => to_bus_addr(b'c'),
        b"S" => to_bus_addr(b'b'),
        b"K" => to_bus_addr(b'a'),
        _ => {
            return Err(std::io::ErrorKind::InvalidData.into());
        }
    })
}
const fn to_bus_addr(c: u8) -> u16 {
    (c - b'a' + BUS_START_ADDR) as u16
}
const BUS_START_ADDR: u8 = 0xaa;

extern "C" fn on_telegram(data: *const u8, len: u32, user_data: Option<NonNull<Sender<ChannelMsg>>>) {
    let data = KDriveTelegram::new(data, len);
    let mut msg = [0; KDRIVE_MAX_GROUP_VALUE_LEN];

    if KDRIVE_CEMI_L_DATA_IND == data.get_msg_code() && data.is_group_write() {
        if let Ok(addr) = data.get_dest() {
            if let Ok(msg) = data.get_group_data(&mut msg) {
                match addr {
                    1 => {
                        //zero  1: alles hoch / 0: alles cool
                        if msg != [0] {
                            println!("Group Write: 1 {:?}", msg);
                            //set all to UP
                            if let Some(mut sender) = user_data {
                            	unsafe{sender.as_mut()}.send((Instant::now(),true, false, 0));
							}
                        }
                        return;
                    }
                    2 => {
                        //wind - no longer avail
                        println!("Wind: {}", u16::from_be_bytes(msg[..2].try_into().unwrap()));
                    }
                    addr if addr & 0xFE00 == 0x1000 => {
                        if msg.len() == 1 {
                            //keep track of own IDs
							if let Some(mut sender) = user_data {
                            	track_write(addr, msg[0], unsafe{sender.as_mut()});
							}
							//[29, 0, bc, e0, 12, 12, 11, 32, 1, 0, 80]
							// KDRIVE_CEMI_L_DATA_IND
							//        xx  xx - ctrl
							//                xx  xx - src
							//                        xx  xx - dst
							//                                len
							//                                    
                            return;
                        }
                    }
                    _ => {}
                }
                println!("Group Write: 0x{:x} {:?}", addr, msg);
                return;
            }
        }
    }
    if data[..6] == [46, 0, 188, 224, 255, 255] && data[8..10] == [1, 0] {
        //initiated by our group_write
        if data[6] & 0x10 != 0 {
            // 0x10 | 0x11
            if data[10] & 0x80 != 0 {
                // 0x80 | 0x81
				if let Some(mut sender) = user_data {
					track_write(
						u16::from_be_bytes(data[6..8].try_into().unwrap()),
						data[10] & 0x70,
						unsafe{sender.as_mut()}
					);
				}
            }
        }
    }
    println!("Data: {:?}", data);
}


fn track_write(addr: u16, val: u8, sender: &mut Sender<ChannelMsg>) {
    let upper = addr & 0xFF00;
    let lower = (addr & 0xff) as u8;
    if let Some(r) = lower.checked_sub(BUS_START_ADDR) {
        if r <= b'h' - b'a' {
			if let Err(_) = sender.send((Instant::now(),val==0, upper == 0x11_00, lower)) {
                println!("send failed")
            }
			return;
        }
    }
    if upper == 0x11_00 {
        println!("Step: 0x{:x} {:?}", lower, val);
    } else if upper == 0x10_00 {
        println!("Voll: 0x{:x} {:?}", lower, val);
    } else {
        println!("Group Write: 0x{:x} {:?}", addr, val);
    }
}

fn track_movements(receiver: Receiver<ChannelMsg>) {
	//let mut states = rustc_hash::FxHashMap::with_capacity_and_hasher(8, Default::default());
    let mut states = std::collections::HashMap::with_capacity(8);
	loop {
		//70s complete
		//2s turn -> 7 steps -> 285 ms
		match receiver.recv_timeout(Duration::from_secs(10)) {
			Err(RecvTimeoutError::Disconnected) => return,
			Ok((time, goes_up, is_single_step, id)) => {
				if id == 0 {
					//all up
					for k in 0..=b'h' - b'a' {
						let id = k+BUS_START_ADDR;
						states.insert(id, (Some(time), goes_up, 0, 100));
					}
					continue;
				}//0
				if is_single_step {
					if let Some((ref mut otime, ref mut moves_up, ref mut pos, ref mut ang)) = states.get_mut(&id) {
                        if let Some(t) = otime.take() {
                            //update
                            let time_old = time.duration_since(t);//1
                            shortened_move(id, time_old, *moves_up, pos, ang);//2
                        }
						//TODO
					}
					continue;
				}
				if let Some((ref mut otime, ref mut moves_up, ref mut pos, ref mut ang)) = states.get_mut(&id) {
					//check privious entry
					if let Some(t) = otime.take() {
                        //update
                        let time_old = time.duration_since(t);//3
                        shortened_move(id, time_old, *moves_up, pos, ang);//4
					}
					*otime = Some(time);
					*moves_up = goes_up;
				}else{
					states.insert(id, (Some(time), goes_up, 0, 100));
				}
			},
			Err(RecvTimeoutError::Timeout) => {
				//clean up status?
				for k in 0..=b'h' - b'a' {
					let id = k+BUS_START_ADDR;
					if let Some((ref mut time, ref mut moves_up, ref mut pos, ref mut ang)) = states.get_mut(&id) {
						if let Some(t) = time {
							if t.elapsed() >= Duration::from_secs(70) {
								*time = None;
								full_move(id, *moves_up, pos, ang);
							}
						}
					}
				}
			},
		}
	} 
}
fn shortened_move(id: u8, time_old: Duration, moves_up: bool, pos: &mut i32, ang: &mut i32) {
    if time_old >= Duration::from_secs(70) {
        full_move(id, moves_up, pos, ang);
        return;
    }
    println!("{} moved {} for {} ms", id, moves_up, time_old.as_millis());
    if time_old.as_secs() >= 2 {
        if moves_up {
            *ang = 100;
        }else{
            *ang = 0;
        }
    }
    //TODO
}
fn full_move(id: u8, moves_up: bool, pos: &mut i32, ang: &mut i32) {
    //complete run
    if moves_up {
        *pos = 0;
        *ang = 100;
    }else{
        *pos = 100;
        *ang = 0;
    }
    println!("{} moved {} completely", id, moves_up);
}