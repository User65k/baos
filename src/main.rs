use core::ptr::NonNull;
use std::convert::TryInto;
#[cfg(feature="mqtt")]
use std::env;
use std::ffi::CString;
use std::ops::RangeInclusive;
use std::rc::Rc;
#[cfg(feature="socket")]
use tokio::io::{AsyncReadExt as _, AsyncWriteExt as _};
#[cfg(feature="socket")]
use tokio::net::{TcpListener, TcpStream};
use std::sync::Arc;
use tokio::sync::{
    mpsc::{channel, Receiver, Sender},
    Mutex,
};
use std::time::{Duration, Instant};

mod ft12;
use ft12::{
    KDriveFT12, cEMIMsg, KDRIVE_CEMI_L_DATA_IND, KDRIVE_MAX_GROUP_VALUE_LEN,
};
mod types;
use types::{Blind, Direction, Pos, Angle, ChannelMsg, StateStore, GroupWriter};
#[cfg(feature="mqtt")]
use rumqttc::{AsyncClient, MqttOptions, QoS};

/// time a blind needs to go from top to bottom or visa verce
const FULL_TRAVEL_TIME: Duration = Duration::from_millis(63_500);//57_600
/// time a blind needs to turn upside down
const FULL_TURN_TIME: Duration = Duration::from_millis(2_800);

fn main() {
    let rt = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap();
    rt.block_on(async_main());
}

async fn async_main() {
    #[cfg(feature="socket")]
    let listener = TcpListener::bind("0.0.0.0:1337").await.expect("listen port");

    let serial = CString::new("/dev/ttyAMA0").unwrap();
    let k = KDriveFT12::open(&serial).await.expect("open FT12");
    let k1 = Rc::new(k);
    let k2 = k1.clone();

    let (sender, bus_receiver) = channel::<ChannelMsg>(32);

    let states = Arc::new(Mutex::new(std::collections::HashMap::with_capacity(8)));
    //let mut states = rustc_hash::FxHashMap::with_capacity_and_hasher(8, Default::default());
    let state = Arc::clone(&states);
    let state3 = Arc::clone(&states);


    let local = tokio::task::LocalSet::new();
    local.run_until(async move {
        // Spawn off an expensive computation
        #[cfg(not(feature="mqtt"))]
        tokio::task::spawn_local(track_movements(bus_receiver, states));
        
        tokio::task::spawn_local(async move {
            let mut buf = [0u8;32];
            loop {
                let r = {
                    k2.read_frame(&mut buf).await
                };
                let cemi = match r {
                    Ok(f) => f,
                    Err(e) => {
                        println!("Error reading {e}");
                        return;
                    }
                };
                on_telegram(cEMIMsg::from_bytes(&cemi), &sender).await
            }
        });
        let (bus_sender, mut receiver) = channel::<(u16, Direction)>(8);
        tokio::task::spawn_local(async move {
            while let Some((addr, d1)) = receiver.recv().await {
                k1.group_write(addr, &[d1 as u8]).await.expect("write err");
            }
        });
        let bus_sender2 = bus_sender.clone();
        #[cfg(feature="socket")]
        tokio::task::spawn_local(async move {
            let mut buf = [0u8; 5];
            loop {
                let stream = listener.accept().await.map(|(s, _)|s);
                if let Err(e) = handle_connection(stream, &mut buf, &bus_sender, &state).await {
                    println!("handle_connection failed {}", e);
                }
            }
        });

        #[cfg(feature="mqtt")]
        {
            let mut mqttoptions = MqttOptions::new("rumqtt-sync", "192.168.178.25", 1883);
            mqttoptions.set_credentials(env::var("HA_MQTT_USER").expect("no mqtt user"), env::var("HA_MQTT_PASS").expect("no mqtt password"));
            mqttoptions.set_keep_alive(Duration::from_secs(5));

            let (client, mut connection) = AsyncClient::new(mqttoptions, 32);
            for c in b'a'..=b'h' {
                client.subscribe(format!("cover/{}/set", c as char), QoS::AtMostOnce).await.expect("sub set");
                client.subscribe(format!("cover/{}/tilt", c as char), QoS::AtMostOnce).await.expect("sub tilt");
            }
            client.publish("cover/availability", QoS::AtLeastOnce, true, "online").await.expect("avail");

            tokio::task::spawn_local(track_movements(bus_receiver, states, client));

            loop {
                match connection.poll().await {
                    Ok(notification) =>{
                        //println!("Notification = {:?}", notification);
                        if let rumqttc::Event::Incoming(rumqttc::Packet::Publish(publish)) = notification {
                            if let Some(s) = publish.topic.strip_prefix("cover/") {
                                if let Some(id) = s.strip_suffix("/set") {
                                    println!("move {} to {:?}", id, publish.payload);//payload is STOP CLOSE ...
                                    if let Some(b) = blind_from_str(id){
                                        mqtt_set(b, &publish.payload, &bus_sender2).await
                                    }
                                }
                                if let Some(id) = s.strip_suffix("/tilt") {
                                    println!("tilt {} to {:?}", id, publish.payload);//payload is 0-7 STOP
                                    if let Some(b) = blind_from_str(id){
                                        mqtt_tilt(b, &publish.payload, &bus_sender2, &state3).await
                                    }
                                }
                            }
                        }
                    },
                    Err(e) => {
                        connection.clean();
                        eprintln!("MQTT Error: {}", e);
                        break;
                    }
                }
            }
        }
    }).await;
}
#[cfg(feature="mqtt")]
fn blind_from_str(id: &str) -> Option<Blind> {
    let id = *id.as_bytes().first()?;
    if id > b'h' {
        return None;
    }
    Some(Blind::from_port(id))
}
#[cfg(feature="mqtt")]
async fn mqtt_set(b: Blind, cmd: &[u8], sender: &GroupWriter) {
    match cmd {
        b"OPEN" => {sender.send((b.to_bus_addr(false), Direction::Up)).await.expect("msg")},
        b"CLOSE" => {sender.send((b.to_bus_addr(false), Direction::Down)).await.expect("msg")},
        b"STOP" => {sender.send((b.to_bus_addr(true), Direction::Up)).await.expect("msg")},
        _ => return
    }
}
#[cfg(feature="mqtt")]
async fn mqtt_tilt(b: Blind, cmd: &[u8], sender: &GroupWriter, state: &Arc<Mutex<StateStore>>) {
    if cmd == b"STOP" {return}
    if cmd.len() != 1 {return}
    let a = cmd[0] - b'0';
    if a > 7 {return}
    let Some(p) = state.lock().await.get(&b).map(|(_, _, p, _)|*p) else{
        return;
    };
    
    move_to_pos(
        b,
        p.into(),
        a,
        sender,
        state
    ).await.expect("nope")
}

#[cfg(any(feature="mqtt",feature="socket"))]
async fn move_to_pos(
    id: Blind,
    target_p: u8,
    target_a: u8,
    k: &GroupWriter,
    state: &Arc<Mutex<StateStore>>,
) -> std::io::Result<()> {
    //get curr pos
    let a = state.lock().await.get(&id).map(|(_, _, p, a)|(*p, *a));
    //match on a in oder to avoid blocking the mutex in None case
    let (mut p, mut a) = match a {
        Some(a) => a,
        None => {
            //we don't know where it is, so drive it into the closest side
            let dir = if target_p < 50 {
                Direction::Down
            }else{
                Direction::Up
            };
            k.send((id.to_bus_addr(false), dir)).await.expect("send err");
            tokio::time::sleep(FULL_TRAVEL_TIME).await;
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
        k.send((id.to_bus_addr(false), dir)).await.expect("send err");
        tokio::time::sleep(ttm).await;
        shortened_move(id, ttm, dir, &mut p, &mut a);
    }
    let cur: u8 = a.into();
    let (dir, div) = if cur > target_a {
        //go down
        (Direction::Down, cur - target_a)
    }else{
        if cur == target_a {
            // just stop to move
            k.send((id.to_bus_addr(true), dir)).await.expect("send err");
            return Ok(());
        }
        //go up
        (Direction::Up, target_a - cur)
    };
    k.send((id.to_bus_addr(false), dir)).await.expect("send err");
    let ttm = FULL_TURN_TIME * (div as u32) / 8u32;
    tokio::time::sleep(ttm).await;
    k.send((id.to_bus_addr(true), dir)).await.expect("send err");
    Ok(())
}
#[cfg(feature="socket")]
async fn handle_connection(
    stream: std::io::Result<TcpStream>,
    buf: &mut [u8],
    k: &GroupWriter,
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
                    stream.write_all(&[(*i).into(), stat]).await?;
                } else {
                    stream.write_all(&[255, 255]).await?;
                }
            }
            return Ok(());
        }
        Some(&[pos, ang]) if pos & 0x80 == 0x80 => {
            for addr in TargetIter::new(target)? {
                let k = k.clone();
                let state = state.clone();
                tokio::spawn(async move {
                    move_to_pos(addr, pos & 0x7f, ang, &k, &state).await
                });
            }
            return Ok(());
        }
        _ => {
            return Err(std::io::ErrorKind::InvalidData.into());
        }
    };
    for addr in TargetIter::new(target)? {
        k.send((addr.to_bus_addr(single_step), data)).await.expect("send err");
    }
    Ok(())
}
enum TargetIter {
    All(RangeInclusive<u8>),
    Some(core::slice::Iter<'static,u8>),
    Single(Blind),
    None
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
            TargetIter::Some(iter) => iter.next().map(|c|Blind::from_port(*c)),
            TargetIter::Single(blind) => {
                let b = Some(*blind);
                *self = TargetIter::None;
                b
            },
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
async fn on_telegram(
    data: cEMIMsg<'_>,
    user_data: &Sender<ChannelMsg>,
) {
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
                            if user_data.send((Instant::now(), Direction::Up, false, Blind::wind()))
                            .await
                            .is_err()
                            {
                                println!("send failed (wind)")
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
                            track_write(addr, msg[0], user_data).await;
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
                track_write(
                    u16::from_be_bytes(data[6..8].try_into().unwrap()),
                    data[10] & 0x7F,
                    user_data,
                ).await;
                return;
            }
        }
    }
    //println!("Data: {:?}", data);
}

//heliocron::calc::SolarCalculations

async fn track_write(addr: u16, val: u8, sender: &Sender<ChannelMsg>) {
    let (_lower, upper) = match Blind::from_bus_addr(addr) {
        Ok((r, single_step)) => {
            println!("Bus: 0x{:x} {}", addr, val);
            let val = if val == Direction::Up as u8 {
                Direction::Up
            }else{
                Direction::Down
            };
            if sender
                .send((Instant::now(), val, single_step, r)).await
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

async fn track_movements(mut receiver: Receiver<ChannelMsg>, states: Arc<Mutex<StateStore>>, #[cfg(feature="mqtt")] client: AsyncClient) {
    loop {
        match tokio::time::timeout(Duration::from_secs(5), receiver.recv()).await {
            Ok(None) => return,
            Ok(Some((time, goes_up, is_single_step, id))) => {
                let mut ss = states.lock().await;
                track_single_press(
                    time,
                    goes_up,
                    is_single_step,
                    id,
                    &mut ss,
                );
                drop(ss);
                #[cfg(feature="mqtt")]
                report(id, &states, &client).await;
                /*println!("states:");
                let s = states.lock().expect("poised");
                for (&k, (_i, _u, p, a)) in s.iter() {
                    println!("{:x}: {:?} {:?}", k, p, a);
                }*/
            }
            Err(_timeout) => {
                //clean up status -> look for full moves
                for k in b'a'..=b'h' {
                    let id = Blind::from_port(k);
                    let mut ss = states.lock().await;
                    if let Some((ref mut time, ref mut moves_up, ref mut pos, ref mut ang)) =
                        ss.get_mut(&id)
                    {
                        if let Some(t) = time {
                            if t.elapsed() >= FULL_TRAVEL_TIME {
                                *time = None;
                                full_move(id, *moves_up, pos, ang);
                                drop(ss);
                                #[cfg(feature="mqtt")]
                                report(id, &states, &client).await;
                            }
                        }
                    }
                }
            }
        }
    }
}
#[cfg(feature="mqtt")]
async fn report(id: Blind, state: &Mutex<StateStore>, client: &AsyncClient) {
    //cover/a..h/state position availability tilt-state

    //query data
    let s = state.lock().await;
    if let Some((t, up, position, tilt)) = s.get(&id) {
        let pos = (*position).into();
        let state = if t.is_some() {
            //its currently moving
            match up {
                Direction::Up => "opening",
                Direction::Down => "closing",
            }
        }else{
            match pos {
                100u8 => "open",
                0u8 => "closed",
                _ => "stopped"
            }
        };
        let tilt: u8 = (*tilt).into();
        client.publish(format!("cover/{}/position", id.letter()), QoS::AtLeastOnce, true, format!("{}", pos)).await.unwrap();
        client.publish(format!("cover/{}/tilt-state", id.letter()), QoS::AtLeastOnce, true, format!("{}", tilt)).await.unwrap();
        client.publish(format!("cover/{}/state", id.letter()), QoS::AtLeastOnce, true, state).await.unwrap();
    } else {
        client.publish(format!("cover/{}/state", id.letter()), QoS::AtLeastOnce, true, "None").await.unwrap();
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