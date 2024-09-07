use core::ptr::NonNull;
use std::convert::TryInto;
use std::ffi::CString;
use std::io::{Read, Write};
use std::net::{TcpListener, TcpStream};
use std::sync::Arc;
use std::time::{Duration, Instant};
use std::sync::{mpsc::{channel, Receiver, RecvTimeoutError, Sender}, Mutex};
use std::thread;

mod kdrive;
use kdrive::{
    KDrive, KDriveFT12, KDriveTelegram, KDRIVE_CEMI_L_DATA_IND, KDRIVE_MAX_GROUP_VALUE_LEN,
};

///time, up, single step, ID
type ChannelMsg = (Instant, bool, bool, u8);
///time of last change, direction, curr_pos, curr_ang
type StateStore = std::collections::HashMap<u8, (Option<Instant>, bool, Pos, Angle)>;

fn main() {
    let listener = TcpListener::bind("0.0.0.0:1337").expect("listen port");

    let serial = CString::new("/dev/ttyAMA0").unwrap();
    let k = KDrive::new().expect("KDrive");
    let k = KDriveFT12::open(k, &serial).expect("open FT12");

	let (mut sender, receiver) = channel::<ChannelMsg>();

    let states = Arc::new(Mutex::new(std::collections::HashMap::with_capacity(8)));
	//let mut states = rustc_hash::FxHashMap::with_capacity_and_hasher(8, Default::default());
    let state = Arc::clone(&states);	
	// Spawn off an expensive computation
	let t = thread::spawn(move|| {
		track_movements(receiver, states);
	});

    k.register_telegram_callback(on_telegram, NonNull::new(&mut sender as *mut _));

    let mut buf = [0u8; 4];
    for stream in listener.incoming() {
        if let Err(e) = handle_connection(stream, &mut buf, &k, &state) {
            println!("handle_connection failed {}", e);
        }
    }
}
fn handle_connection(
    stream: std::io::Result<TcpStream>,
    buf: &mut [u8],
    k: &KDrive,
    state: &Arc<Mutex<StateStore>>
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
        Some(b"?") => {//query data
            let s = state.lock().expect("not pos");
            for addr in to_bus_addr(b'a')..=to_bus_addr(b'h') {
                if let Some((_, _, i, j)) = s.get(&(addr as u8)) {
                    stream.write_all(&[(*i).into(), (*j).into()])?;
                }else{
                    stream.write_all(&[255, 255])?;
                }
            }
            return Ok(());
        },
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
//57.57s complete
//2s turn -> 7 steps -> 285 ms
const FULL_TRAVEL_TIME: Duration = Duration::from_millis(57_600);

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
                            	if unsafe{sender.as_mut()}.send((Instant::now(),true, false, 0)).is_err() {
                                    println!("send failed (wind)")
                                }
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
						data[10] & 0x7F,
						unsafe{sender.as_mut()}
					);
				}
                return;
            }
        }
    }
    println!("Data: {:?}", data);
}

//heliocron::calc::SolarCalculations

fn track_write(addr: u16, val: u8, sender: &mut Sender<ChannelMsg>) {
    let upper = addr & 0xFF00;
    let lower = (addr & 0xff) as u8;
    if let Some(r) = lower.checked_sub(BUS_START_ADDR) {
        if r <= b'h' - b'a' {
            println!("Bus: 0x{:x} {}", addr, val);
			if sender.send((Instant::now(),val==0, upper == 0x11_00, lower)).is_err() {
                println!("send failed")
            }
			return;
        }
    }
    if upper == 0x11_00 {
        //println!("Step: 0x{:x} {:?}", lower, val);
    } else if upper == 0x10_00 {
        //println!("Voll: 0x{:x} {:?}", lower, val);
    } else {
        println!("Group Write: 0x{:x} {:?}", addr, val);
    }
}
/// time: Time of Event
/// goes_up: direction of move
/// is_single_step
/// id: bus address
/// states: HashMap to store it all
#[inline]
fn track_single_press(time: Instant, goes_up: bool, is_single_step: bool, id: u8, states: &mut StateStore)
{
    if id == 0 {
        //wind - all up
        for k in 0..=b'h' - b'a' {
            let id = k+BUS_START_ADDR;
            states.insert(id, (Some(time), goes_up, Pos::top(), Angle::top()));
        }
        return;
    }
    if is_single_step {
        if let Some((ref mut otime, ref mut moves_up, ref mut pos, ref mut ang)) = states.get_mut(&id) {
            if let Some(t) = otime.take() {
                //it was on the move... -> stop it
                let time_moving = time.duration_since(t);
                shortened_move(id, time_moving, *moves_up, pos, ang);
            }else{
                //just move a single step (1/7) 14pts
                if goes_up {
                    ang.up(14);
                }else{
                    ang.down(14);
                }
                //TODO move a little, if in saturation
            }
        }
        //else
        // just move a single step
        // - but we dont know anything about its pos
        // -> so ignore it
        return;
    }
    if let Some((ref mut otime, ref mut moves_up, ref mut pos, ref mut ang)) = states.get_mut(&id) {
        //check privious entry
        if let Some(t) = otime.take() {
            //update
            let time_moving = time.duration_since(t);
            shortened_move(id, time_moving, *moves_up, pos, ang);
        }
        //remember this move
        *otime = Some(time);
        *moves_up = goes_up;
    }else{
        //remember this move
        let mut ang = Angle::bottom();
        let mut pos = Pos::bottom();
        //start at the opposite full range
        full_move(id, !goes_up, &mut pos, &mut ang);
        states.insert(id, (Some(time), goes_up, pos, ang));
    }
}

fn track_movements(receiver: Receiver<ChannelMsg>, states: Arc<Mutex<StateStore>>) {
	loop {
		match receiver.recv_timeout(Duration::from_secs(10)) {
			Err(RecvTimeoutError::Disconnected) => return,
			Ok((time, goes_up, is_single_step, id)) => {
                track_single_press(time, goes_up, is_single_step, id, &mut states.lock().expect("poised"));
                println!("states:");
                let s = states.lock().expect("poised");
                for (&k,(_i, _u, p, a)) in s.iter() {
                    println!("{:x}: {:?} {:?}", k, p, a);
                }
			},
			Err(RecvTimeoutError::Timeout) => {
				//clean up status -> look for full moves
				for k in 0..=b'h' - b'a' {
					let id = k+BUS_START_ADDR;
					if let Some((ref mut time, ref mut moves_up, ref mut pos, ref mut ang)) = states.lock().expect("poised").get_mut(&id) {
						if let Some(t) = time {
							if t.elapsed() >= FULL_TRAVEL_TIME {
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
fn shortened_move(id: u8, time_moving: Duration, moves_up: bool, pos: &mut Pos, ang: &mut Angle) {
    if time_moving >= FULL_TRAVEL_TIME {
        full_move(id, moves_up, pos, ang);
        return;
    }
    println!("{:x} moved {} for {} ms", id, moves_up, time_moving.as_millis());
    if time_moving.as_secs() >= 2 {
        if moves_up {
            *ang = Angle::top();
        }else{
            *ang = Angle::bottom();
        }
    }
    if moves_up {
        pos.up(time_moving);
    }else{
        pos.down(time_moving);
    }
}
fn full_move(id: u8, moves_up: bool, pos: &mut Pos, ang: &mut Angle) {
    //complete run
    if moves_up {
        *pos = Pos::top();
        *ang = Angle::top();
    }else{
        *pos = Pos::bottom();
        *ang = Angle::bottom();
    }
    println!("{:x} moved {} completely", id, moves_up);
}
#[derive(Debug, Clone, Copy)]
struct Angle(u8);
impl Angle {
    fn up(&mut self, arg: u8) -> bool {
        let t = self.0 + arg;
        if t > 100 {
            self.0 = 100;
            true
        }else{
            self.0 = t;
            false
        }
    }
    
    fn down(&mut self, arg: u8) -> bool {
        if let Some(n) = self.0.checked_sub(arg) {
            self.0 = n;
            false
        }else{
            self.0 = 0;
            true
        }
    }
    fn top() ->  Angle {
        Angle(100)
    }
    fn bottom() ->  Angle {
        Angle(0)
    }
}
impl From<Angle> for u8 {
    fn from(val: Angle) -> Self {
        val.0
    }
}
#[derive(Debug, Clone, Copy)]
struct Pos(u8);
impl Pos {
    fn top() ->  Pos {
        Pos(100)
    }
    fn bottom() ->  Pos {
        Pos(0)
    }
    fn up(&mut self, time_moving: Duration) {
        if time_moving >= FULL_TRAVEL_TIME {
            self.0 = 100;
            return;
        }
        let div = 100 * time_moving.as_nanos() / FULL_TRAVEL_TIME.as_nanos();
        let t = self.0 + div as u8;
        if t > 100 {
            self.0 = 100;
        }else{
            self.0 = t;
        }
    }
    
    fn down(&mut self, time_moving: Duration) {
        if time_moving >= FULL_TRAVEL_TIME {
            self.0 = 0;
            return;
        }
        let div = 100 * time_moving.as_nanos() / FULL_TRAVEL_TIME.as_nanos();
        if let Some(n) = self.0.checked_sub(div as u8) {
            self.0 = n;
        }else{
            self.0 = 0;
        }
    }
}
impl From<Pos> for u8 {
    fn from(val: Pos) -> Self {
        val.0
    }
}