//! What the server remembers about a room between its sockets: the destinations the
//! producer declared on the signaling socket, for the ingest session that follows.
//!
//! Stream keys live here, inside [`Target`] values (redacted `Debug`), for as long as the
//! producer's signaling socket is open or until `stop_stream`; they are never written
//! anywhere.

use std::{
    collections::HashMap,
    sync::{Arc, Mutex},
};

use prostor_protocol::rtmp::Target;

use crate::room::RoomId;

/// One declared destination.
#[derive(Debug, Clone)]
pub struct Destination {
    pub id: String,
    pub name: String,
    pub target: Target,
}

/// The per-room state shared by the signaling and ingest handlers.
#[derive(Debug, Clone, Default)]
pub struct Rooms {
    inner: Arc<Mutex<HashMap<RoomId, Vec<Destination>>>>,
}

impl Rooms {
    /// Replaces the room's destinations.
    pub fn set_destinations(&self, room: &RoomId, destinations: Vec<Destination>) {
        let mut rooms = self.inner.lock().expect("rooms lock");
        if destinations.is_empty() {
            rooms.remove(room);
        } else {
            rooms.insert(room.clone(), destinations);
        }
    }

    /// Forgets the room's destinations (and their keys).
    pub fn clear(&self, room: &RoomId) {
        self.inner.lock().expect("rooms lock").remove(room);
    }

    /// The room's destinations, if any were declared.
    pub fn destinations(&self, room: &RoomId) -> Vec<Destination> {
        self.inner
            .lock()
            .expect("rooms lock")
            .get(room)
            .cloned()
            .unwrap_or_default()
    }

    /// Rooms with destinations declared right now.
    pub fn armed_rooms(&self) -> usize {
        self.inner.lock().expect("rooms lock").len()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn destination(id: &str) -> Destination {
        Destination {
            id: id.to_owned(),
            name: id.to_owned(),
            target: Target::new("rtmp://example.invalid/app", "k").unwrap(),
        }
    }

    #[test]
    fn stores_clears_and_counts() {
        let rooms = Rooms::default();
        let room = RoomId::parse("r").unwrap();
        assert!(rooms.destinations(&room).is_empty());
        rooms.set_destinations(&room, vec![destination("a"), destination("b")]);
        assert_eq!(rooms.destinations(&room).len(), 2);
        assert_eq!(rooms.armed_rooms(), 1);
        rooms.set_destinations(&room, vec![destination("c")]);
        assert_eq!(rooms.destinations(&room)[0].id, "c");
        rooms.set_destinations(&room, vec![]);
        assert_eq!(rooms.armed_rooms(), 0);
        rooms.set_destinations(&room, vec![destination("a")]);
        rooms.clear(&room);
        assert!(rooms.destinations(&room).is_empty());
    }

    #[test]
    fn debug_output_redacts_keys() {
        let text = format!("{:?}", destination("a"));
        assert!(text.contains("example.invalid"));
        assert!(!text.contains("\"k\""), "{text}");
        assert!(text.contains("redacted"), "{text}");
    }
}
