#[cfg(feature = "socket")]
mod tcp;
use std::{ffi::CString, rc::Rc, sync::Arc, time::Duration};
#[cfg(feature = "socket")]
use tokio::net::TcpListener;
use tokio::{
    sync::{mpsc::channel, Mutex},
    task::JoinSet,
    time::sleep,
};

mod ft12;
use ft12::{cEMIMsg, KDriveFT12};
mod tracking;
mod types;
use types::{Angle, Blind, ChannelMsg, Direction, GroupWriter, Pos, StateStore};
#[cfg(feature = "mqtt")]
mod mqtt;

/// time a blind needs to go from top to bottom or visa verce
const FULL_TRAVEL_TIME: Duration = Duration::from_millis(63_500); //57_600
/// time a blind needs to turn upside down
const FULL_TURN_TIME: Duration = Duration::from_millis(2_000);

fn main() {
    let rt = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap();
    rt.block_on(async_main());
}

async fn async_main() {
    #[cfg(feature = "socket")]
    let listener = TcpListener::bind("0.0.0.0:1337")
        .await
        .expect("listen port");

    let serial = CString::new("/dev/ttyAMA0").unwrap();
    let k = KDriveFT12::open(&serial).await.expect("open FT12");
    let k1 = Rc::new(k);
    let k2 = k1.clone();

    let (sender, bus_receiver) = channel::<ChannelMsg>(32);

    let states = Arc::new(Mutex::new(std::collections::HashMap::with_capacity(8)));

    let local = tokio::task::LocalSet::new();
    local
        .run_until(async move {
            let mut set = JoinSet::new();
            #[cfg(not(feature = "mqtt"))]
            set.spawn_local(tracking::track_movements(bus_receiver, states));
            // read incoming KNX messages
            set.spawn_local(async move {
                let mut buf = [0u8; 32];
                loop {
                    let r = { k2.read_frame(&mut buf).await };
                    let cemi = match r {
                        Ok(f) => f,
                        Err(e) => {
                            println!("Error reading {e}");
                            return;
                        }
                    };
                    tracking::on_telegram(cEMIMsg::from_bytes(&cemi), &sender).await
                }
            });
            let (bus_sender, mut receiver) = channel::<(u16, Direction)>(8);
            // write outgoing KNX messages
            set.spawn_local(async move {
                while let Some((addr, d1)) = receiver.recv().await {
                    k1.group_write(addr, &[d1 as u8]).await.expect("write err");
                }
            });
            #[cfg(feature = "socket")]
            set.spawn_local(tcp::drive(listener, bus_sender.clone(), states.clone()));

            #[cfg(feature = "mqtt")]
            {
                let (client, connection) = mqtt::setup().await;

                set.spawn_local(tracking::track_movements(
                    bus_receiver,
                    states.clone(),
                    client.clone(),
                ));
                set.spawn_local(mqtt::drive(
                    connection,
                    bus_sender.clone(),
                    states.clone(),
                    client,
                ));
            }
            //wait for the first task to "finish" aka crash
            let res = set.join_next().await.unwrap();
            eprintln!("Task ended: {res:?}");
        })
        .await;
    // we could figure out who finished early/paniced
    // spawn it anew and continue
    // .. or just die and let systemd restart the whole thing
}

#[cfg(any(feature = "mqtt", feature = "socket"))]
async fn move_to_pos(
    id: Blind,
    target_p: Pos,
    target_a: Angle,
    k: &GroupWriter,
    state: &Arc<Mutex<StateStore>>,
) -> Option<()> {
    //short circuit
    match (target_p, target_a) {
        (Pos::BOTTOM, Angle::BOTTOM) => {
            k.send((id.to_bus_addr(false), Direction::Down))
                .await
                .ok()?;
            return Some(());
        }
        (Pos::TOP, Angle::TOP) => {
            k.send((id.to_bus_addr(false), Direction::Up)).await.ok()?;
            return Some(());
        }
        _ => {}
    }

    //get curr pos
    let current = state.lock().await.get(&id).map(|(_, _, p, a)| (*p, *a));
    //match on `current` in oder to avoid blocking the mutex in None case
    let (mut current_position, mut current_angle) = match current {
        Some(a) => a,
        None => {
            //we don't know where it is, so drive it into the closest side
            let (dir, _) = Pos::from_num(50).delta(target_p);
            k.send((id.to_bus_addr(false), dir)).await.ok()?;
            sleep(FULL_TRAVEL_TIME).await;
            match dir {
                Direction::Up => (Pos::TOP, Angle::TOP),
                Direction::Down => (Pos::BOTTOM, Angle::BOTTOM),
            }
        }
    };
    //println!("Abs move from {:?} / {:?} to {} / {}", current_position, current_angle, target_p, target_a);

    let (dir, div) = current_position.delta(target_p);
    if div > 0 {
        let ttm = Pos::step_time(div);
        k.send((id.to_bus_addr(false), dir)).await.ok()?;
        sleep(ttm).await;
        //keep a local track of current_angle - the state store is updated directly form the bus messages
        tracking::shortened_move(id, ttm, dir, &mut current_position, &mut current_angle);
    }
    if current_angle == target_a {
        if div > 0 {
            // just stop the move to pos
            k.send((id.to_bus_addr(true), dir)).await.ok()?;
        }
        return Some(());
    }
    let (dir, div) = current_angle.delta(target_a);
    
    k.send((id.to_bus_addr(false), dir)).await.ok()?;
    let ttm = Angle::step_time(div);
    sleep(ttm).await;
    k.send((id.to_bus_addr(true), dir)).await.ok()?;
    Some(())
}
