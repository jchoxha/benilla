//! Azeroth Warfare's own world packets, beside 1.12's (whose table ends at 827): the client's shot
//! batches and stance/state, and the server's hit, kill and streak events. The server side is
//! `src/fusion` in azeroth-warfare-server; both sides test against the same frozen byte vectors
//! (`tests/data/fusion_packets.txt`, a copy of the server's `src/fusion/vectors/packets.txt`).
//! Little-endian throughout.

use anyhow::{bail, Result};

use crate::world::WorldWriter;

pub const CMSG_FUSION_SHOTS: u16 = 828;
pub const SMSG_FUSION_EVENTS: u16 = 829;
pub const CMSG_FUSION_STATE: u16 = 830;

pub const VERSION: u8 = 1;
/// One batch carries at most this many shots (the client sends 20 batches a second).
pub const MAX_SHOTS_PER_BATCH: usize = 16;
pub const MAX_EVENTS_PER_PACKET: usize = 32;

pub const SHOT_ADS: u8 = 0x01;
pub const SHOT_CLAIMS_HEAD: u8 = 0x02;
pub const SHOT_AIRBORNE: u8 = 0x04;

pub const STATE_AIMING: u8 = 0x01;
/// Ask for the free-for-all flag on.
pub const STATE_FFA_ON: u8 = 0x02;
/// Ask for it off (it drops after 5 minutes out of combat).
pub const STATE_FFA_OFF: u8 = 0x04;
pub const STATE_RELOADING: u8 = 0x08;

pub const EVENT_HEADSHOT: u8 = 0x01;
pub const EVENT_FATAL: u8 = 0x02;
pub const EVENT_ASSIST: u8 = 0x04;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
#[repr(u8)]
pub enum Stance {
    #[default]
    Stand = 0,
    Crouch = 1,
    Prone = 2,
    Slide = 3,
}

impl Stance {
    fn from_u8(v: u8) -> Option<Self> {
        Some(match v {
            0 => Self::Stand,
            1 => Self::Crouch,
            2 => Self::Prone,
            3 => Self::Slide,
            _ => return None,
        })
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
#[repr(u8)]
pub enum Gait {
    Still = 0,
    Walk = 1,
    #[default]
    Run = 2,
    Sprint = 3,
    Backpedal = 4,
}

impl Gait {
    fn from_u8(v: u8) -> Option<Self> {
        Some(match v {
            0 => Self::Still,
            1 => Self::Walk,
            2 => Self::Run,
            3 => Self::Sprint,
            4 => Self::Backpedal,
            _ => return None,
        })
    }
}

/// One shot as the client saw it: the server re-casts it against its own hitboxes, rewound to
/// `time_ms`, and only then deals damage.
#[derive(Debug, Clone, Copy, PartialEq, Default)]
pub struct Shot {
    /// The shooter's view time on the server clock.
    pub time_ms: u32,
    pub weapon_id: u16,
    pub flags: u8,
    pub stance: Stance,
    /// The unit the client's ray hit, 0 for none.
    pub target_guid: u64,
    /// Raw WoW yards, z up.
    pub origin: [f32; 3],
    pub direction: [f32; 3],
}

#[derive(Debug, Clone, Copy, PartialEq, Default)]
pub struct State {
    pub stance: Stance,
    pub gait: Gait,
    pub flags: u8,
    pub held_weapon_id: u16,
    /// The loaded special round (0 basic).
    pub loaded_ammo: u8,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(u8)]
pub enum EventKind {
    /// You hit `guid` for `amount`.
    Hit = 0,
    /// You killed `guid`.
    Kill = 1,
    /// `guid` hit you for `amount`.
    Hurt = 2,
    /// `extra` is the reward id.
    StreakEarned = 3,
    /// A resync: `amount` the magazine, `extra` the reserve.
    Ammo = 4,
    Revived = 5,
}

impl EventKind {
    fn from_u8(v: u8) -> Option<Self> {
        Some(match v {
            0 => Self::Hit,
            1 => Self::Kill,
            2 => Self::Hurt,
            3 => Self::StreakEarned,
            4 => Self::Ammo,
            5 => Self::Revived,
            _ => return None,
        })
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Event {
    pub kind: EventKind,
    pub flags: u8,
    pub guid: u64,
    pub amount: u32,
    pub extra: u16,
}

/// `CMSG_FUSION_SHOTS`' body; shots past [`MAX_SHOTS_PER_BATCH`] are dropped.
pub fn encode_shots(shots: &[Shot]) -> Vec<u8> {
    let n = shots.len().min(MAX_SHOTS_PER_BATCH);
    let mut out = Vec::with_capacity(2 + n * 40);
    out.push(VERSION);
    out.push(n as u8);
    for s in &shots[..n] {
        out.extend_from_slice(&s.time_ms.to_le_bytes());
        out.extend_from_slice(&s.weapon_id.to_le_bytes());
        out.push(s.flags);
        out.push(s.stance as u8);
        out.extend_from_slice(&s.target_guid.to_le_bytes());
        for v in s.origin.iter().chain(&s.direction) {
            out.extend_from_slice(&v.to_le_bytes());
        }
    }
    out
}

/// `CMSG_FUSION_STATE`'s body.
pub fn encode_state(s: &State) -> Vec<u8> {
    let mut out = vec![VERSION, s.stance as u8, s.gait as u8, s.flags];
    out.extend_from_slice(&s.held_weapon_id.to_le_bytes());
    out.push(s.loaded_ammo);
    out
}

/// `SMSG_FUSION_EVENTS`' body (the server writes these; here for tests and the load-test bot).
pub fn encode_events(events: &[Event]) -> Vec<u8> {
    let n = events.len().min(MAX_EVENTS_PER_PACKET);
    let mut out = vec![VERSION, n as u8];
    for e in &events[..n] {
        out.push(e.kind as u8);
        out.push(e.flags);
        out.extend_from_slice(&e.guid.to_le_bytes());
        out.extend_from_slice(&e.amount.to_le_bytes());
        out.extend_from_slice(&e.extra.to_le_bytes());
    }
    out
}

struct Cursor<'a> {
    b: &'a [u8],
}

impl Cursor<'_> {
    fn take<const N: usize>(&mut self) -> Result<[u8; N]> {
        if self.b.len() < N {
            bail!("fusion packet too short");
        }
        let (head, rest) = self.b.split_at(N);
        self.b = rest;
        Ok(head.try_into().expect("split at N"))
    }
    fn u8(&mut self) -> Result<u8> {
        Ok(self.take::<1>()?[0])
    }
    fn u16(&mut self) -> Result<u16> {
        Ok(u16::from_le_bytes(self.take()?))
    }
    fn u32(&mut self) -> Result<u32> {
        Ok(u32::from_le_bytes(self.take()?))
    }
    fn u64(&mut self) -> Result<u64> {
        Ok(u64::from_le_bytes(self.take()?))
    }
    fn f32(&mut self) -> Result<f32> {
        let v = f32::from_le_bytes(self.take()?);
        if !v.is_finite() {
            bail!("fusion packet carries a non-finite float");
        }
        Ok(v)
    }
    fn version(&mut self) -> Result<()> {
        let v = self.u8()?;
        if v != VERSION {
            bail!("fusion packet version {v}, expected {VERSION}");
        }
        Ok(())
    }
    fn done(&self) -> Result<()> {
        if !self.b.is_empty() {
            bail!("fusion packet has {} trailing bytes", self.b.len());
        }
        Ok(())
    }
}

pub fn decode_shots(body: &[u8]) -> Result<Vec<Shot>> {
    let mut r = Cursor { b: body };
    r.version()?;
    let n = r.u8()? as usize;
    if n > MAX_SHOTS_PER_BATCH {
        bail!("{n} shots in one batch");
    }
    let mut shots = Vec::with_capacity(n);
    for _ in 0..n {
        let time_ms = r.u32()?;
        let weapon_id = r.u16()?;
        let flags = r.u8()?;
        let Some(stance) = Stance::from_u8(r.u8()?) else {
            bail!("bad stance");
        };
        let target_guid = r.u64()?;
        let origin = [r.f32()?, r.f32()?, r.f32()?];
        let direction = [r.f32()?, r.f32()?, r.f32()?];
        shots.push(Shot {
            time_ms,
            weapon_id,
            flags,
            stance,
            target_guid,
            origin,
            direction,
        });
    }
    r.done()?;
    Ok(shots)
}

pub fn decode_state(body: &[u8]) -> Result<State> {
    let mut r = Cursor { b: body };
    r.version()?;
    let (Some(stance), Some(gait)) = (Stance::from_u8(r.u8()?), Gait::from_u8(r.u8()?)) else {
        bail!("bad stance or gait");
    };
    let flags = r.u8()?;
    let held_weapon_id = r.u16()?;
    let loaded_ammo = r.u8()?;
    r.done()?;
    Ok(State {
        stance,
        gait,
        flags,
        held_weapon_id,
        loaded_ammo,
    })
}

pub fn decode_events(body: &[u8]) -> Result<Vec<Event>> {
    let mut r = Cursor { b: body };
    r.version()?;
    let n = r.u8()? as usize;
    if n > MAX_EVENTS_PER_PACKET {
        bail!("{n} events in one packet");
    }
    let mut events = Vec::with_capacity(n);
    for _ in 0..n {
        let Some(kind) = EventKind::from_u8(r.u8()?) else {
            bail!("bad event kind");
        };
        events.push(Event {
            kind,
            flags: r.u8()?,
            guid: r.u64()?,
            amount: r.u32()?,
            extra: r.u16()?,
        });
    }
    r.done()?;
    Ok(events)
}

impl WorldWriter {
    /// `CMSG_FUSION_SHOTS`: this tick's shots (at most [`MAX_SHOTS_PER_BATCH`]).
    pub fn fusion_shots(&mut self, shots: &[Shot]) -> Result<()> {
        self.send(CMSG_FUSION_SHOTS, &encode_shots(shots))
    }

    /// `CMSG_FUSION_STATE`: stance, gait, held gun and the FFA flag requests, sent on change.
    pub fn fusion_state(&mut self, state: &State) -> Result<()> {
        self.send(CMSG_FUSION_STATE, &encode_state(state))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn vector(name: &str) -> Vec<u8> {
        let text = include_str!("../tests/data/fusion_packets.txt");
        let hex = text
            .lines()
            .filter(|l| !l.starts_with('#'))
            .find_map(|l| l.strip_prefix(name)?.strip_prefix(' '))
            .unwrap_or_else(|| panic!("no vector {name}"));
        (0..hex.len() / 2)
            .map(|i| u8::from_str_radix(&hex[2 * i..2 * i + 2], 16).unwrap())
            .collect()
    }

    fn sample_shot() -> Shot {
        Shot {
            time_ms: 123_456,
            weapon_id: 7,
            flags: SHOT_ADS | SHOT_CLAIMS_HEAD,
            stance: Stance::Crouch,
            target_guid: 0xF1A2B3,
            origin: [10.5, -20.25, 30.0],
            direction: [0.0, 1.0, 0.0],
        }
    }

    fn sample_state() -> State {
        State {
            stance: Stance::Prone,
            gait: Gait::Walk,
            flags: STATE_AIMING | STATE_FFA_ON,
            held_weapon_id: 42,
            loaded_ammo: 5,
        }
    }

    fn sample_events() -> Vec<Event> {
        vec![
            Event {
                kind: EventKind::Hit,
                flags: EVENT_HEADSHOT,
                guid: 0x1234,
                amount: 45,
                extra: 7,
            },
            Event {
                kind: EventKind::Kill,
                flags: EVENT_FATAL,
                guid: 0x1234,
                amount: 0,
                extra: 7,
            },
        ]
    }

    #[test]
    fn encodes_the_servers_bytes() {
        assert_eq!(encode_shots(&[sample_shot()]), vector("shots_one"));
        assert_eq!(encode_shots(&[]), vector("shots_empty"));
        assert_eq!(encode_state(&sample_state()), vector("state"));
        assert_eq!(encode_events(&sample_events()), vector("events_hit_kill"));
    }

    #[test]
    fn decodes_the_servers_bytes() {
        assert_eq!(
            decode_shots(&vector("shots_one")).unwrap(),
            vec![sample_shot()]
        );
        assert_eq!(decode_state(&vector("state")).unwrap(), sample_state());
        assert_eq!(
            decode_events(&vector("events_hit_kill")).unwrap(),
            sample_events()
        );
    }

    #[test]
    fn rejects_malformed_bodies() {
        let body = vector("shots_one");
        assert!(decode_shots(&body[..body.len() - 1]).is_err());
        let mut long = body.clone();
        long.push(0);
        assert!(decode_shots(&long).is_err());
        let mut version = body.clone();
        version[0] = 2;
        assert!(decode_shots(&version).is_err());
        let mut stance = body.clone();
        stance[2 + 7] = 9;
        assert!(decode_shots(&stance).is_err());
        assert!(decode_shots(&[VERSION, MAX_SHOTS_PER_BATCH as u8 + 1]).is_err());
        let mut nan = body;
        nan[2 + 28..2 + 32].copy_from_slice(&f32::NAN.to_le_bytes());
        assert!(decode_shots(&nan).is_err());
    }

    #[test]
    fn caps_a_batch() {
        let shots = vec![sample_shot(); 20];
        assert_eq!(
            decode_shots(&encode_shots(&shots)).unwrap().len(),
            MAX_SHOTS_PER_BATCH
        );
    }
}
