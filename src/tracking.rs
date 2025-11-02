use crate::{
    ft12::{cEMIMsg, MessageCode, KDRIVE_MAX_GROUP_VALUE_LEN},
    types::{Angle, Blind, ChannelMsg, Direction, Pos, StateStore},
    FULL_TRAVEL_TIME,
};
use std::{
    convert::TryInto,
    sync::Arc,
    time::{Duration, Instant},
};
use tokio::sync::{
    mpsc::{Receiver, Sender},
    Mutex,
};

/// called on every KNX message received
pub async fn on_telegram(data: cEMIMsg<'_>, user_data: &Sender<ChannelMsg>) {
    let mut msg = [0; KDRIVE_MAX_GROUP_VALUE_LEN];

    if data.is_group_write() {
        if let Ok(addr) = data.get_dest() {
            if let Ok(msg) = data.get_group_data(&mut msg) {
                match data.get_msg_code() {
                    MessageCode::LDataInd => {
                        // data from the bus
                        match addr {
                            1 => {
                                //zero  1: alles hoch / 0: alles cool
                                if msg != [0] {
                                    println!("Group Write: 1 {:?}", msg);
                                    //set all to UP
                                    if user_data
                                        .send((Instant::now(), Direction::Up, false, Blind::wind()))
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
                                println!(
                                    "Wind: {}",
                                    u16::from_be_bytes(msg[..2].try_into().unwrap())
                                );
                            }
                            addr if is_cover_addr(addr) && msg.len() == 1 => {
                                //keep track of own IDs
                                //println!("Incomming Bus: 0x{:x} {}", addr, msg[0]);
                                track_write(addr, msg[0], user_data).await;
                                return;
                            }
                            _ => {}
                        }
                        println!("Group Write: 0x{:x} {:?}", addr, msg);
                        return;
                    }
                    MessageCode::LDataCon if is_cover_addr(addr) && msg.len() == 1 => {
                        // initiated by our group_write
                        //println!("Outgoing Bus: 0x{:x} {}", addr, msg[0]);
                        track_write(addr, msg[0], user_data).await;
                        return;
                    }
                    _ => {}
                }
            }
        }
    }
    println!("Bus Data: {:?}", data.as_ref());
}
/// addr is a jal-0810.02 cover address (`0x10xx` or `0x11xx`)
#[inline]
fn is_cover_addr(addr: u16) -> bool {
    addr & 0xFE00 == 0x1000
}
/// further process an incoming message
/// sends it over a channel to `track_movements`
async fn track_write(addr: u16, val: u8, sender: &Sender<ChannelMsg>) {
    let (_lower, _upper) = match Blind::from_bus_addr(addr) {
        Ok((blind_id, single_step)) => {
            let direction = if val == Direction::Up as u8 {
                Direction::Up
            } else {
                Direction::Down
            };
            if sender
                .send((Instant::now(), direction, single_step, blind_id))
                .await
                .is_err()
            {
                println!("send failed")
            }
            return;
        }
        Err((a, b)) => (a, b),
    };
    /*
    //lower might be a cover from another apartment
    if upper == Blind::CMD_SINGLE_STEP {
        //println!("Step: 0x{:x} {:?}", lower, val);
    } else if upper == Blind::CMD_FULL_MOVE {
        //println!("Voll: 0x{:x} {:?}", lower, val);
    } else {
        unreachable!("track_write is only ever called after is_cover_addr")
    }*/
}
/// called from a channel in `track_write`
pub async fn track_movements(
    mut receiver: Receiver<ChannelMsg>,
    states: Arc<Mutex<StateStore>>,
    #[cfg(feature = "mqtt")] client: rumqttc::v5::AsyncClient,
) {
    loop {
        match tokio::time::timeout(Duration::from_secs(5), receiver.recv()).await {
            Ok(None) => return,
            Ok(Some((time, goes_up, is_single_step, id))) => {
                let mut ss = states.lock().await;
                track_single_press(time, goes_up, is_single_step, id, &mut ss);
                drop(ss);
                #[cfg(feature = "mqtt")]
                crate::mqtt::report(id, &states, &client).await;
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
                                #[cfg(feature = "mqtt")]
                                crate::mqtt::report(id, &states, &client).await;
                            }
                        }
                    }
                }
            }
        }
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
            states.insert(id, (Some(time), goes_up, Pos::TOP, Angle::TOP));
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
        let mut ang = Angle::BOTTOM;
        let mut pos = Pos::BOTTOM;
        //start at the opposite full range
        let moveit = match goes_up {
            Direction::Up => Direction::Down,
            Direction::Down => Direction::Up,
        };
        full_move(id, moveit, &mut pos, &mut ang);
        states.insert(id, (Some(time), goes_up, pos, ang));
    }
}
/// update a Blinds state according to its time_moving
pub fn shortened_move(
    id: Blind,
    time_moving: Duration,
    moves_up: Direction,
    pos: &mut Pos,
    ang: &mut Angle,
) {
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
        }
        Direction::Down => {
            pos.down(time_moving);
            ang.down(time_moving);
        }
    }
}
/// update a Blinds state according to its time_moving
fn full_move(id: Blind, moves_up: Direction, pos: &mut Pos, ang: &mut Angle) {
    //complete run
    match moves_up {
        Direction::Up => {
            *pos = Pos::TOP;
            *ang = Angle::TOP;
        }
        Direction::Down => {
            *pos = Pos::BOTTOM;
            *ang = Angle::BOTTOM;
        }
    }
    println!("{:?} moved {:?} completely", id, moves_up);
}
