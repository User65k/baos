use core::ptr::NonNull;
use std::convert::TryInto;
use std::ffi::CString;
use std::io::{Read, Write};
use std::net::{TcpListener, TcpStream};
use std::sync::Arc;
use std::sync::{
    mpsc::{channel, Receiver, RecvTimeoutError, Sender},
    Mutex,
};
use std::thread;
use std::time::{Duration, Instant};

mod kdrive;
use kdrive::{
    KDrive, KDriveFT12, KDriveTelegram, KDRIVE_CEMI_L_DATA_IND, KDRIVE_MAX_GROUP_VALUE_LEN,
};
mod types;
use types::{Blind, Direction, Pos, Angle, ChannelMsg, StateStore, GroupWriter};

///57.57s complete
///2s turn -> 7 steps -> 285 ms
const FULL_TRAVEL_TIME: Duration = Duration::from_millis(63_000);//57_600
const FULL_TURN_TIME: Duration = Duration::from_millis(2_000);


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
    let t = thread::spawn(move || {
        track_movements(receiver, states);
    });

    k.register_telegram_callback(on_telegram, NonNull::new(&mut sender as *mut _))
        .expect("could not set callback");

    let (sender, receiver) = channel::<(u16, Direction)>();
    thread::spawn(move || for (addr, d1) in &receiver {
        k.group_write(addr, &[d1 as u8]).expect("write err");
    });
    let mut buf = [0u8; 5];
    for stream in listener.incoming() {
        if let Err(e) = handle_connection(stream, &mut buf, &sender, &state) {
            println!("handle_connection failed {}", e);
        }
    }
}
fn move_to_pos(
    id: Blind,
    target_p: u8,
    target_a: u8,
    k: GroupWriter,
    state: &Arc<Mutex<StateStore>>,
) -> std::io::Result<()> {
    //get curr pos
    let (mut p, mut a) = match state.lock().unwrap().get(&id) {
        Some((_, _, p, a)) => (*p, *a),
        None => {
            //we don't know where it is, so drive it into the closest side
            let dir = if target_p < 50 {
                Direction::Down
            }else{
                Direction::Up
            };
            k.send((id.to_bus_addr(false), dir)).expect("send err");
            thread::sleep(FULL_TRAVEL_TIME);
            if target_p < 50 {
                (Pos::bottom(), Angle::bottom())
            }else{
                (Pos::top(), Angle::top())
            }
        }
    };
    let cur: u8 = p.into();
    //move for x ms (x=FULL_MOVE/100*(cur-target_p)))
    let (dir, div) = if cur > target_p {
        //go down
        (Direction::Down, cur - target_p)
    } else {
        //go up
        (Direction::Up, target_p - cur)
    };
    if div > 0 {
        //let ttm = FULL_TRAVEL_TIME.mul_f32((div as f32)/100f32);
        let ttm = FULL_TRAVEL_TIME * (div as u32) / 100u32;
        k.send((id.to_bus_addr(false), dir)).expect("send err");
        thread::sleep(ttm);
        shortened_move(id, ttm, dir, &mut p, &mut a);
    }
    let cur: u8 = a.into();
    let (dir, div) = if cur > target_a {
        //go down
        (Direction::Down, cur - target_a)
    }else{
        if cur == target_a {
            // just stop to move
            k.send((id.to_bus_addr(true), dir)).expect("send err");
            return Ok(());
        }
        //go up
        (Direction::Up, target_a - cur)
    };
    k.send((id.to_bus_addr(false), dir)).expect("send err");
    let ttm = FULL_TURN_TIME * (div as u32) / 8u32;
    thread::sleep(ttm);
    k.send((id.to_bus_addr(true), dir)).expect("send err");
    Ok(())
}
fn handle_connection(
    stream: std::io::Result<TcpStream>,
    buf: &mut [u8],
    k: &GroupWriter,
    state: &Arc<Mutex<StateStore>>,
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

    let (single_step, data) = match i.next() {
        Some(b"1") | Some(b"Z") => (false, Direction::Down),
        Some(b"0") | Some(b"A") => (false, Direction::Up),
        Some(b"S") => (true, Direction::Down),
        Some(b"D") => (true, Direction::Down),
        Some(b"U") => (true, Direction::Up),
        Some(b"?") => {
            //query data
            let s = state.lock().expect("not pos");
            for c in b'a'..=b'h' {
                let addr = Blind::from_port(c);
                if let Some((t, up, i, j)) = s.get(&addr) {
                    let mut stat = (*j).into(); //0 - 7 -> 0b111
                    if t.is_some() {
                        //its currently moving
                        match up {
                            Direction::Up => stat += 0b0011_0000,
                            Direction::Down => stat += 0b0010_0000,
                        }
                    }
                    stream.write_all(&[(*i).into(), stat])?;
                } else {
                    stream.write_all(&[255, 255])?;
                }
            }
            return Ok(());
        }
        Some(&[pos, ang]) if pos & 0x80 == 0x80 => {
            return for_all_targets(target, &|addr| {
                let k = k.clone();
                let state = state.clone();
                thread::spawn(move || move_to_pos(addr, pos & 0x7f, ang, k, &state));
            });
        }
        _ => {
            return Err(std::io::ErrorKind::InvalidData.into());
        }
    };
    for_all_targets(target, &|addr| k.send((addr.to_bus_addr(single_step), data)).expect("send err"))
}
fn for_all_targets(target: &[u8], f: &dyn Fn(Blind)) -> std::io::Result<()> {
    match target {
        b"A" => {
            for addr in b'a'..=b'h' {
                f(Blind::from_port(addr));
            }
        }
        b"B" => {
            f(Blind::from_port(b'g'));
            f(Blind::from_port(b'd'));
        }
        b"W" => {
            for addr in &[
                Blind::from_port(b'h'),
                Blind::from_port(b'f'),
                Blind::from_port(b'e'),
                Blind::from_port(b'c'),
            ] {
                f(*addr);
            }
        }
        l => {
            f(get_addr(l)?);
        }
    }    
    Ok(())
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
extern "C" fn on_telegram(
    data: *const u8,
    len: u32,
    user_data: Option<NonNull<Sender<ChannelMsg>>>,
) {
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
                                if unsafe { sender.as_mut() }
                                    .send((Instant::now(), Direction::Up, false, Blind::wind()))
                                    .is_err()
                                {
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
                                track_write(addr, msg[0], unsafe { sender.as_mut() });
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
                        unsafe { sender.as_mut() },
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
    let (_lower, upper) = match Blind::from_bus_addr(addr) {
        Ok((r, single_step)) => {
            println!("Bus: 0x{:x} {}", addr, val);
            let val = if val == Direction::Up as u8 {
                Direction::Up
            }else{
                Direction::Down
            };
            if sender
                .send((Instant::now(), val, single_step, r))
                .is_err()
            {
                println!("send failed")
            }
            return;
        },
        Err((a,b)) => (a,b),
    };
    if upper == Blind::CMD_SINGLE_STEP {
        //println!("Step: 0x{:x} {:?}", lower, val);
    } else if upper == Blind::CMD_FULL_MOVE {
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
fn track_single_press(
    time: Instant,
    goes_up: Direction,
    is_single_step: bool,
    id: Blind,
    states: &mut StateStore,
) {
    if id == Blind::wind() {
        //wind - all up
        for k in b'a'..=b'h' {
            let id = Blind::from_port(k);
            states.insert(id, (Some(time), goes_up, Pos::top(), Angle::top()));
        }
        return;
    }
    if is_single_step {
        if let Some((ref mut otime, ref mut moves_up, ref mut pos, ref mut ang)) =
            states.get_mut(&id)
        {
            if let Some(t) = otime.take() {
                //it was on the move... -> stop it
                let time_moving = time.duration_since(t);
                shortened_move(id, time_moving, *moves_up, pos, ang);
            } else {
                //just move a single step
                match goes_up {
                    Direction::Up => ang.step_up(1),
                    Direction::Down => ang.step_down(1),
                };
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
    } else {
        //remember this move
        let mut ang = Angle::bottom();
        let mut pos = Pos::bottom();
        //start at the opposite full range
        let moveit = match goes_up {
            Direction::Up => Direction::Down,
            Direction::Down => Direction::Up,
        };
        full_move(id, moveit, &mut pos, &mut ang);
        states.insert(id, (Some(time), goes_up, pos, ang));
    }
}

fn track_movements(receiver: Receiver<ChannelMsg>, states: Arc<Mutex<StateStore>>) {
    loop {
        match receiver.recv_timeout(Duration::from_secs(5)) {
            Err(RecvTimeoutError::Disconnected) => return,
            Ok((time, goes_up, is_single_step, id)) => {
                track_single_press(
                    time,
                    goes_up,
                    is_single_step,
                    id,
                    &mut states.lock().expect("poised"),
                );
                /*println!("states:");
                let s = states.lock().expect("poised");
                for (&k, (_i, _u, p, a)) in s.iter() {
                    println!("{:x}: {:?} {:?}", k, p, a);
                }*/
            }
            Err(RecvTimeoutError::Timeout) => {
                //clean up status -> look for full moves
                for k in b'a'..=b'h' {
                    let id = Blind::from_port(k);
                    if let Some((ref mut time, ref mut moves_up, ref mut pos, ref mut ang)) =
                        states.lock().expect("poised").get_mut(&id)
                    {
                        if let Some(t) = time {
                            if t.elapsed() >= FULL_TRAVEL_TIME {
                                *time = None;
                                full_move(id, *moves_up, pos, ang);
                            }
                        }
                    }
                }
            }
        }
    }
}
fn shortened_move(id: Blind, time_moving: Duration, moves_up: Direction, pos: &mut Pos, ang: &mut Angle) {
    if time_moving >= FULL_TRAVEL_TIME {
        full_move(id, moves_up, pos, ang);
        return;
    }
    println!(
        "{:?} moved {:?} for {} ms",
        id,
        moves_up,
        time_moving.as_millis()
    );
    match moves_up {
        Direction::Up => {
            pos.up(time_moving);
            ang.up(time_moving);
        },
        Direction::Down => {
            pos.down(time_moving);
            ang.down(time_moving);
        },
    }
}
fn full_move(id: Blind, moves_up: Direction, pos: &mut Pos, ang: &mut Angle) {
    //complete run
    match moves_up {
        Direction::Up => {
            *pos = Pos::top();
            *ang = Angle::top();
        },
        Direction::Down => {
            *pos = Pos::bottom();
            *ang = Angle::bottom();
        },
    }
    println!("{:?} moved {:?} completely", id, moves_up);
}