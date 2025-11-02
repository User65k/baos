use crate::types::{Angle, Blind, Direction, GroupWriter, Pos, StateStore};
use rumqttc::v5::{
    mqttbytes::{
        v5::{Filter, LastWill, Packet},
        QoS,
    },
    AsyncClient, Event, EventLoop, MqttOptions,
};
use std::{env, sync::Arc, time::Duration};
use tokio::sync::Mutex;

pub async fn setup() -> (AsyncClient, EventLoop) {
    let mut mqttoptions = MqttOptions::new("blinds", "192.168.178.25", 1883);
    mqttoptions.set_credentials(
        env::var("HA_MQTT_USER").expect("no mqtt user"),
        env::var("HA_MQTT_PASS").expect("no mqtt password"),
    );
    mqttoptions.set_last_will(LastWill {
        topic: "cover/availability".into(),
        message: "offline".into(),
        qos: QoS::AtLeastOnce,
        retain: true,
        properties: None,
    });
    mqttoptions.set_keep_alive(Duration::from_secs(5));

    let (client, connection) = AsyncClient::new(mqttoptions, 32);
    (client, connection)
}
pub async fn drive(
    mut connection: EventLoop,
    bus_sender: GroupWriter,
    state: Arc<Mutex<StateStore>>,
    client: AsyncClient,
) {
    loop {
        match connection.poll().await {
            Ok(notification) => {
                //println!("Notification = {:?}", notification);
                match notification {
                    Event::Incoming(Packet::Publish(publish)) => {
                        if let Some(s) = publish.topic.strip_prefix(b"cover/") {
                            if let Some(id) = s.strip_suffix(b"/set") {
                                println!("move {} to {:?}", id.escape_ascii(), publish.payload); //payload is STOP CLOSE ...
                                if let Some(b) = blind_from_str(id) {
                                    mqtt_set(b, &publish.payload, &bus_sender).await
                                }
                            }
                            if let Some(id) = s.strip_suffix(b"/tilt") {
                                println!("tilt {} to {:?}", id.escape_ascii(), publish.payload); //payload is 0-7 STOP
                                if let Some(b) = blind_from_str(id) {
                                    mqtt_tilt(b, &publish.payload, &bus_sender, &state).await
                                }
                            }
                        }
                    }
                    Event::Incoming(Packet::ConnAck(_)) => {
                        println!("MQTT connected");
                        client
                            .subscribe_many((b'a'..=b'h').flat_map(|c| {
                                [
                                    Filter::new(
                                        format!("cover/{}/set", c as char),
                                        QoS::AtMostOnce,
                                    ),
                                    Filter::new(
                                        format!("cover/{}/tilt", c as char),
                                        QoS::AtMostOnce,
                                    ),
                                ]
                            }))
                            .await
                            .expect("sub");
                        client
                            .publish("cover/availability", QoS::AtLeastOnce, true, "online")
                            .await
                            .expect("avail");
                    }
                    _ => {}
                }
            }
            Err(e) => {
                //already done on poll: connection.clean();

                //MQTT Error: Mqtt state: Connection closed by peer abruptly
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
        let state = if t.is_some() {
            //its currently moving
            match up {
                Direction::Up => "opening",
                Direction::Down => "closing",
            }
        } else {
            match *position {
                Pos::TOP => "open",
                Pos::BOTTOM => "closed",
                _ => "stopped",
            }
        };
        let pos: u8 = (*position).into();
        let tilt: u8 = (*tilt).into();
        drop(s);
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
        drop(s);
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

fn blind_from_str(id: &[u8]) -> Option<Blind> {
    let id = *id.first()?;
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
    if a > Angle::TOP.into() {
        return;
    }
    let Some(p) = state.lock().await.get(&b).map(|(_, _, p, _)| *p) else {
        return;
    };

    super::move_to_pos(b, p, Angle::from_num(a), sender, state)
        .await
        .expect("cant send")
}
