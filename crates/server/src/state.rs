//! Shared server state: sessions, rooms, per-session buckets and delivery.

use crate::auth::AuthGate;
use crate::config::ServerConfig;
use crate::rooms::{self, Room};
use rust_chat_crypto::PasswordVerifier;
use rust_chat_protocol::payloads::{MemberLeft, RoomFlags};
use rust_chat_protocol::{Envelope, ErrorCode, Member, RoomState, RoomSummary, kind};
use serde::Serialize;
use std::collections::HashMap;
use std::net::IpAddr;
use std::path::PathBuf;
use std::sync::{Arc, Mutex, MutexGuard};
use std::time::{Instant, SystemTime, UNIX_EPOCH};
use tokio::sync::{Notify, mpsc};

pub const SESSION_QUEUE: usize = 512;

pub type RoomError = (ErrorCode, String);

#[derive(Clone, Debug)]
pub struct Bucket {
    tokens: f64,
    capacity: f64,
    refill_per_second: f64,
    last: Instant,
}

impl Bucket {
    pub fn new(capacity: f64, refill_per_second: f64) -> Self {
        Self {
            tokens: capacity,
            capacity,
            refill_per_second,
            last: Instant::now(),
        }
    }

    pub fn allow(&mut self, now: Instant) -> bool {
        let elapsed = now.saturating_duration_since(self.last).as_secs_f64();
        self.tokens = (self.tokens + elapsed * self.refill_per_second).min(self.capacity);
        self.last = now;
        if self.tokens >= 1.0 {
            self.tokens -= 1.0;
            true
        } else {
            false
        }
    }

    pub fn drain(&mut self) {
        self.tokens = 0.0;
    }
}

pub struct Session {
    pub id: String,
    pub name: String,
    pub pubkey_fp: String,
    pub features: Vec<String>,
    pub joined_at: i64,
    pub room: Option<String>,
    pub self_mute: bool,
    pub self_deaf: bool,
    pub sharing: bool,
    pub voice: bool,
    pub tx: mpsc::Sender<Envelope>,
    pub close: Arc<Notify>,
    pub chat: Bucket,
    pub signal: Bucket,
    pub offers: Bucket,
}

impl Session {
    pub fn new(
        id: String,
        name: String,
        pubkey_fp: String,
        features: Vec<String>,
        tx: mpsc::Sender<Envelope>,
        close: Arc<Notify>,
    ) -> Self {
        Self {
            id,
            name,
            pubkey_fp,
            features,
            joined_at: now_ms(),
            room: None,
            self_mute: false,
            self_deaf: false,
            sharing: false,
            voice: false,
            tx,
            close,
            chat: Bucket::new(10.0, 5.0),
            signal: Bucket::new(300.0, 50.0),
            offers: Bucket::new(20.0, 20.0 / 60.0),
        }
    }

    pub fn member(&self) -> Member {
        Member {
            session_id: self.id.clone(),
            name: self.name.clone(),
            pubkey_fp: self.pubkey_fp.clone(),
            joined_at: self.joined_at,
            self_mute: self.self_mute,
            self_deaf: self.self_deaf,
            sharing: self.sharing,
            voice: self.voice,
            features: self.features.clone(),
        }
    }
}

pub struct State {
    pub sessions: HashMap<String, Session>,
    pub rooms: Vec<Room>,
    message_seq: u64,
}

impl State {
    pub fn new(config: &ServerConfig, client_rooms: Vec<Room>) -> Self {
        let mut rooms: Vec<Room> = config
            .rooms
            .iter()
            .map(|room| Room {
                id: room.id.clone(),
                name: room.name.clone(),
                topic: room.topic.clone(),
                voice: room.voice,
                from_config: true,
            })
            .collect();
        for room in client_rooms {
            if rooms.iter().any(|known| {
                known.id.eq_ignore_ascii_case(&room.id)
                    || known.name.eq_ignore_ascii_case(&room.name)
            }) {
                continue;
            }
            rooms.push(room);
        }
        if !rooms.iter().any(|room| room.id == "lobby") {
            rooms.insert(
                0,
                Room {
                    id: "lobby".into(),
                    name: "Lobby".into(),
                    topic: String::new(),
                    voice: false,
                    from_config: true,
                },
            );
        }
        Self {
            sessions: HashMap::new(),
            rooms,
            message_seq: 0,
        }
    }

    pub fn default_room_id(&self) -> String {
        if self.rooms.iter().any(|room| room.id == "lobby") {
            "lobby".to_owned()
        } else {
            self.rooms
                .first()
                .map(|room| room.id.clone())
                .unwrap_or_else(|| "lobby".to_owned())
        }
    }

    pub fn room(&self, id: &str) -> Option<&Room> {
        self.rooms.iter().find(|room| room.id == id)
    }

    pub fn member_count(&self, room_id: &str) -> usize {
        self.sessions
            .values()
            .filter(|session| session.room.as_deref() == Some(room_id))
            .count()
    }

    pub fn room_summaries(&self, config: &ServerConfig) -> Vec<RoomSummary> {
        self.rooms
            .iter()
            .map(|room| room.summary(self.member_count(&room.id), config.max_users_per_room))
            .collect()
    }

    pub fn send(&self, session_id: &str, envelope: Envelope) -> bool {
        let Some(session) = self.sessions.get(session_id) else {
            return false;
        };
        match session.tx.try_send(envelope) {
            Ok(()) => true,
            Err(mpsc::error::TrySendError::Full(_)) | Err(mpsc::error::TrySendError::Closed(_)) => {
                session.close.notify_one();
                false
            }
        }
    }

    pub fn broadcast_all(&self, envelope: Envelope) {
        for session in self.sessions.values() {
            let _ = self.send(&session.id, envelope.clone());
        }
    }

    pub fn broadcast_room(&self, room_id: &str, envelope: Envelope, except: Option<&str>) {
        let ids: Vec<String> = self
            .sessions
            .values()
            .filter(|session| session.room.as_deref() == Some(room_id))
            .filter(|session| Some(session.id.as_str()) != except)
            .map(|session| session.id.clone())
            .collect();
        for id in ids {
            let _ = self.send(&id, envelope.clone());
        }
    }

    pub fn snapshot(&self, room_id: &str, self_id: &str) -> Option<RoomState> {
        let room = self.room(room_id)?;
        let mut members: Vec<Member> = self
            .sessions
            .values()
            .filter(|session| session.room.as_deref() == Some(room_id))
            .map(Session::member)
            .collect();
        members.sort_by_key(|member| (member.joined_at, member.session_id.clone()));
        let self_member = members
            .iter()
            .find(|member| member.session_id == self_id)?
            .clone();
        Some(RoomState {
            room: rust_chat_protocol::RoomInfo {
                id: room.id.clone(),
                name: room.name.clone(),
                topic: room.topic.clone(),
                voice: room.voice,
            },
            self_member,
            members,
        })
    }

    pub fn broadcast_room_state(&self, room_id: &str) {
        let ids: Vec<String> = self
            .sessions
            .values()
            .filter(|session| session.room.as_deref() == Some(room_id))
            .map(|session| session.id.clone())
            .collect();
        for id in ids {
            if let Some(snapshot) = self.snapshot(room_id, &id) {
                let _ = self.send(&id, payload_envelope(kind::ROOM_STATE, &snapshot));
            }
        }
    }

    pub fn broadcast_room_list(&self, config: &ServerConfig) {
        let rooms = self.room_summaries(config);
        self.broadcast_all(payload_envelope(
            kind::ROOM_LIST_RESULT,
            &serde_json::json!({ "rooms": rooms }),
        ));
    }

    pub fn broadcast_room_flags(&self) {
        let deletable: Vec<String> = self
            .rooms
            .iter()
            .filter(|room| !room.from_config && room.id != "lobby")
            .filter(|room| self.member_count(&room.id) == 0)
            .map(|room| room.id.clone())
            .collect();
        self.broadcast_all(payload_envelope(kind::ROOM_FLAGS, &RoomFlags { deletable }));
    }

    pub fn join_room(
        &mut self,
        config: &ServerConfig,
        session_id: &str,
        room_id: &str,
    ) -> Result<(), RoomError> {
        let (voice, exists) = match self.room(room_id) {
            Some(room) => (room.voice, true),
            None => (false, false),
        };
        if !exists {
            return Err((
                ErrorCode::RoomNotFound,
                format!("room `{room_id}` does not exist"),
            ));
        }
        let current = self
            .sessions
            .get(session_id)
            .ok_or_else(|| (ErrorCode::AuthRequired, "session is gone".to_owned()))?
            .room
            .clone();
        if current.as_deref() == Some(room_id) {
            self.broadcast_room_state(room_id);
            return Ok(());
        }
        if voice && self.member_count(room_id) >= config.max_users_per_room {
            return Err((ErrorCode::RoomFull, format!("room `{room_id}` is full")));
        }

        if let Some(old) = current {
            if let Some(session) = self.sessions.get_mut(session_id) {
                session.room = None;
            }
            let left = payload_envelope(
                kind::MEMBER_LEFT,
                &MemberLeft {
                    session_id: session_id.to_owned(),
                    reason: "leave".into(),
                },
            );
            self.broadcast_room(&old, left, None);
            self.broadcast_room_state(&old);
        }

        if let Some(session) = self.sessions.get_mut(session_id) {
            session.room = Some(room_id.to_owned());
        }
        self.broadcast_room_state(room_id);
        if let Some(member) = self.sessions.get(session_id).map(Session::member) {
            let joined = payload_envelope(
                kind::MEMBER_JOINED,
                &serde_json::json!({ "member": member }),
            );
            self.broadcast_room(room_id, joined, Some(session_id));
        }
        self.broadcast_room_list(config);
        self.broadcast_room_flags();
        Ok(())
    }

    pub fn leave_room(&mut self, config: &ServerConfig, session_id: &str) {
        let old = self
            .sessions
            .get_mut(session_id)
            .and_then(|session| session.room.take());
        if let Some(old) = old {
            let left = payload_envelope(
                kind::MEMBER_LEFT,
                &MemberLeft {
                    session_id: session_id.to_owned(),
                    reason: "leave".into(),
                },
            );
            self.broadcast_room(&old, left, None);
            self.broadcast_room_state(&old);
        }
        let rooms = self.room_summaries(config);
        let _ = self.send(
            session_id,
            payload_envelope(kind::ROOM_LEFT, &serde_json::json!({ "rooms": rooms })),
        );
        self.broadcast_room_list(config);
        self.broadcast_room_flags();
    }

    pub fn disconnect(&mut self, config: &ServerConfig, session_id: &str, reason: &str) {
        let Some(session) = self.sessions.remove(session_id) else {
            return;
        };
        if let Some(room_id) = session.room {
            let left = payload_envelope(
                kind::MEMBER_LEFT,
                &MemberLeft {
                    session_id: session_id.to_owned(),
                    reason: reason.to_owned(),
                },
            );
            self.broadcast_room(&room_id, left, None);
            self.broadcast_room_state(&room_id);
        }
        self.broadcast_room_list(config);
        self.broadcast_room_flags();
    }

    pub fn create_room(
        &mut self,
        config: &ServerConfig,
        session_id: &str,
        name: &str,
        topic: &str,
        voice: bool,
    ) -> Result<Vec<Room>, RoomError> {
        if !config.allow_client_rooms {
            return Err((
                ErrorCode::BadPayload,
                "this server does not allow client-created rooms".into(),
            ));
        }
        if self.rooms.len() >= config.max_rooms {
            return Err((
                ErrorCode::BadPayload,
                format!("the server already has {} rooms", config.max_rooms),
            ));
        }
        let name = name.trim();
        let name_len = name.chars().count();
        if name_len == 0 || name_len > rust_chat_protocol::MAX_NAME_CHARS {
            return Err((
                ErrorCode::BadPayload,
                "room name must be 1-32 characters".into(),
            ));
        }
        let topic = topic.trim();
        if topic.chars().count() > rust_chat_protocol::MAX_TOPIC_CHARS {
            return Err((ErrorCode::BadPayload, "room topic is too long".into()));
        }
        if self
            .rooms
            .iter()
            .any(|room| room.name.eq_ignore_ascii_case(name))
        {
            return Err((
                ErrorCode::BadPayload,
                format!("a room named `{name}` already exists"),
            ));
        }
        let mut id = rooms::slugify(name);
        for _ in 0..16 {
            if !self.rooms.iter().any(|room| room.id == id) {
                break;
            }
            id = rooms::slugify(name);
        }
        self.rooms.push(Room {
            id: id.clone(),
            name: name.to_owned(),
            topic: topic.to_owned(),
            voice,
            from_config: false,
        });
        if let Err(error) = self.join_room(config, session_id, &id) {
            self.rooms.retain(|room| room.id != id);
            return Err(error);
        }
        self.broadcast_room_list(config);
        self.broadcast_room_flags();
        Ok(self.rooms.clone())
    }

    pub fn delete_room(
        &mut self,
        config: &ServerConfig,
        room_id: &str,
    ) -> Result<Vec<Room>, RoomError> {
        let Some(room) = self.room(room_id) else {
            return Err((
                ErrorCode::RoomNotFound,
                format!("room `{room_id}` does not exist"),
            ));
        };
        if room.from_config || room.id == "lobby" {
            return Err((
                ErrorCode::RoomDeleteDenied,
                "rooms from the server config cannot be removed".into(),
            ));
        }
        if self.member_count(room_id) > 0 {
            return Err((
                ErrorCode::RoomNotEmpty,
                "a room with members cannot be removed".into(),
            ));
        }
        self.rooms.retain(|room| room.id != room_id);
        self.broadcast_all(payload_envelope(
            kind::ROOM_DELETED,
            &serde_json::json!({ "roomId": room_id }),
        ));
        self.broadcast_room_list(config);
        self.broadcast_room_flags();
        Ok(self.rooms.clone())
    }

    pub fn next_message_id(&mut self) -> String {
        self.message_seq = self.message_seq.wrapping_add(1);
        format!("m_{:x}{:x}", now_ms(), self.message_seq)
    }
}

pub struct Shared {
    pub config: ServerConfig,
    pub verifier: PasswordVerifier,
    pub state: Mutex<State>,
    auth_gate: Mutex<HashMap<IpAddr, AuthGate>>,
    pub client_rooms_path: PathBuf,
}

impl Shared {
    pub fn new(config: ServerConfig, verifier: PasswordVerifier) -> anyhow::Result<Arc<Self>> {
        let path = PathBuf::from(&config.client_rooms_file);
        let client_rooms = rooms::load_client_rooms(&path)?;
        let state = State::new(&config, client_rooms);
        Ok(Arc::new(Self {
            config,
            verifier,
            state: Mutex::new(state),
            auth_gate: Mutex::new(HashMap::new()),
            client_rooms_path: path,
        }))
    }

    pub fn lock_state(&self) -> MutexGuard<'_, State> {
        self.state.lock().unwrap_or_else(|error| error.into_inner())
    }

    pub fn auth_allow(&self, ip: IpAddr) -> bool {
        let now = Instant::now();
        let mut gates = self
            .auth_gate
            .lock()
            .unwrap_or_else(|error| error.into_inner());
        gates.entry(ip).or_default().allow(now)
    }

    pub fn auth_fail(&self, ip: IpAddr) {
        let now = Instant::now();
        let mut gates = self
            .auth_gate
            .lock()
            .unwrap_or_else(|error| error.into_inner());
        gates.entry(ip).or_default().fail(now);
    }

    pub fn auth_success(&self, ip: IpAddr) {
        let mut gates = self
            .auth_gate
            .lock()
            .unwrap_or_else(|error| error.into_inner());
        if let Some(gate) = gates.get_mut(&ip) {
            gate.success();
        }
    }

    pub fn save_client_rooms(&self, rooms: &[Room]) -> anyhow::Result<()> {
        rooms::save_client_rooms(&self.client_rooms_path, rooms)
    }
}

pub fn payload_envelope<T: Serialize>(kind: &str, payload: &T) -> Envelope {
    Envelope::new(
        kind,
        serde_json::to_value(payload).expect("payloads are plain data"),
    )
}

pub fn now_ms() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|duration| duration.as_millis() as i64)
        .unwrap_or(0)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::ServerConfig;
    use std::time::Duration;

    fn config() -> ServerConfig {
        ServerConfig::default()
    }

    fn state() -> (Arc<Shared>, ServerConfig) {
        let config = config();
        let verifier = PasswordVerifier::create("test", 8, 1, 1).unwrap();
        let shared = Shared::new(config.clone(), verifier).unwrap();
        (shared, config)
    }

    fn insert_session(shared: &Shared, id: &str, name: &str) {
        let (tx, _rx) = mpsc::channel(4);
        let close = Arc::new(Notify::new());
        shared.lock_state().sessions.insert(
            id.to_owned(),
            Session::new(
                id.to_owned(),
                name.to_owned(),
                "SHA256:00".into(),
                Vec::new(),
                tx,
                close,
            ),
        );
    }

    #[test]
    fn buckets_refill_and_deny() {
        let mut bucket = Bucket::new(2.0, 1.0);
        let now = Instant::now();
        assert!(bucket.allow(now));
        assert!(bucket.allow(now));
        assert!(!bucket.allow(now));
        assert!(bucket.allow(now + Duration::from_secs(1)));
    }

    #[test]
    fn joining_switches_rooms_and_enforces_caps() {
        let (shared, config) = state();
        insert_session(&shared, "s1", "alice");
        insert_session(&shared, "s2", "bob");
        {
            let mut state = shared.lock_state();
            state.sessions.get_mut("s1").unwrap().room = Some("lobby".into());
            state.join_room(&config, "s2", "lobby").unwrap();
            let snapshot = state.snapshot("lobby", "s2").unwrap();
            assert_eq!(snapshot.members.len(), 2);
            assert_eq!(snapshot.self_member.session_id, "s2");
        }
    }

    #[test]
    fn text_room_and_voice_cap_rules() {
        let (shared, mut config) = state();
        config.max_users_per_room = 1;
        insert_session(&shared, "s1", "alice");
        insert_session(&shared, "s2", "bob");
        let mut state = shared.lock_state();
        state.sessions.get_mut("s1").unwrap().room = Some("lobby".into());
        let error = state.join_room(&config, "s2", "lobby").unwrap_err();
        assert_eq!(error.0, ErrorCode::RoomFull);
        state.leave_room(&config, "s1");
        assert!(state.join_room(&config, "s2", "lobby").is_ok());
    }

    #[test]
    fn room_create_and_delete_rules() {
        let (shared, config) = state();
        insert_session(&shared, "s1", "alice");
        let mut state = shared.lock_state();
        let rooms = state
            .create_room(&config, "s1", "Game Night", "fun", true)
            .unwrap();
        assert!(rooms.iter().any(|room| room.name == "Game Night"));
        let created = rooms
            .iter()
            .find(|room| room.name == "Game Night")
            .unwrap()
            .id
            .clone();
        assert!(state.delete_room(&config, &created).is_err()); // creator is inside
        state.leave_room(&config, "s1");
        assert!(state.delete_room(&config, &created).is_ok());
        assert!(state.delete_room(&config, "lobby").is_err());
    }

    #[test]
    fn names_are_unique_case_insensitively() {
        let (shared, config) = state();
        insert_session(&shared, "s1", "alice");
        let mut state = shared.lock_state();
        state
            .create_room(&config, "s1", "Game Night", "", true)
            .unwrap();
        let error = state
            .create_room(&config, "s1", "game night", "", true)
            .unwrap_err();
        assert_eq!(error.0, ErrorCode::BadPayload);
    }
}
