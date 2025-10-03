use crate::types::{Blind, Direction, GroupWriter, StateStore};
use rumqttc::{AsyncClient, EventLoop, MqttOptions, QoS};
use std::{env, sync::Arc, time::Duration};
use tokio::sync::Mutex;

pub async fn setup() -> (AsyncClient, EventLoop) {
    let mut mqttoptions = MqttOptions::new("rumqtt-sync", "192.168.178.25", 1883);
    mqttoptions.set_credentials(
        env::var("HA_MQTT_USER").expect("no mqtt user"),
        env::var("HA_MQTT_PASS").expect("no mqtt password"),
    );
    mqttoptions.set_keep_alive(Duration::from_secs(5));

    let (client, mut connection) = AsyncClient::new(mqttoptions, 32);
    for c in b'a'..=b'h' {
        client
            .subscribe(format!("cover/{}/set", c as char), QoS::AtMostOnce)
            .await
            .expect("sub set");
        client
            .subscribe(format!("cover/{}/tilt", c as char), QoS::AtMostOnce)
            .await
            .expect("sub tilt");
    }
    client
        .publish("cover/availability", QoS::AtLeastOnce, true, "online")
        .await
        .expect("avail");
    (client, connection)
}
pub async fn drive(
    mut connection: EventLoop,
    bus_sender: GroupWriter,
    state: Arc<Mutex<StateStore>>,
) {
    loop {
        match connection.poll().await {
            Ok(notification) => {
                //println!("Notification = {:?}", notification);
                if let rumqttc::Event::Incoming(rumqttc::Packet::Publish(publish)) = notification {
                    if let Some(s) = publish.topic.strip_prefix("cover/") {
                        if let Some(id) = s.strip_suffix("/set") {
                            println!("move {} to {:?}", id, publish.payload); //payload is STOP CLOSE ...
                            if let Some(b) = blind_from_str(id) {
                                mqtt_set(b, &publish.payload, &bus_sender).await
                            }
                        }
                        if let Some(id) = s.strip_suffix("/tilt") {
                            println!("tilt {} to {:?}", id, publish.payload); //payload is 0-7 STOP
                            if let Some(b) = blind_from_str(id) {
                                mqtt_tilt(b, &publish.payload, &bus_sender, &state).await
                            }
                        }
                    }
                }
            }
            Err(e) => {
                connection.clean();
                eprintln!("MQTT Error: {}", e);
            }
        }
    }
}
pub async fn report(id: Blind, state: &Mutex<StateStore>, client: &AsyncClient) {
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
        } else {
            match pos {
                100u8 => "open",
                0u8 => "closed",
                _ => "stopped",
            }
        };
        let tilt: u8 = (*tilt).into();
        client
            .publish(
                format!("cover/{}/position", id.letter()),
                QoS::AtLeastOnce,
                true,
                format!("{}", pos),
            )
            .await
            .unwrap();
        client
            .publish(
                format!("cover/{}/tilt-state", id.letter()),
                QoS::AtLeastOnce,
                true,
                format!("{}", tilt),
            )
            .await
            .unwrap();
        client
            .publish(
                format!("cover/{}/state", id.letter()),
                QoS::AtLeastOnce,
                true,
                state,
            )
            .await
            .unwrap();
    } else {
        client
            .publish(
                format!("cover/{}/state", id.letter()),
                QoS::AtLeastOnce,
                true,
                "None",
            )
            .await
            .unwrap();
    }
}

fn blind_from_str(id: &str) -> Option<Blind> {
    let id = *id.as_bytes().first()?;
    if id > b'h' {
        return None;
    }
    Some(Blind::from_port(id))
}

async fn mqtt_set(b: Blind, cmd: &[u8], sender: &GroupWriter) {
    match cmd {
        b"OPEN" => sender
            .send((b.to_bus_addr(false), Direction::Up))
            .await
            .expect("msg"),
        b"CLOSE" => sender
            .send((b.to_bus_addr(false), Direction::Down))
            .await
            .expect("msg"),
        b"STOP" => sender
            .send((b.to_bus_addr(true), Direction::Up))
            .await
            .expect("msg"),
        _ => return,
    }
}

async fn mqtt_tilt(b: Blind, cmd: &[u8], sender: &GroupWriter, state: &Arc<Mutex<StateStore>>) {
    if cmd == b"STOP" {
        return;
    }
    if cmd.len() != 1 {
        return;
    }
    let a = cmd[0] - b'0';
    if a > 7 {
        return;
    }
    let Some(p) = state.lock().await.get(&b).map(|(_, _, p, _)| *p) else {
        return;
    };

    super::move_to_pos(b, p.into(), a, sender, state)
        .await
        .expect("nope")
}
