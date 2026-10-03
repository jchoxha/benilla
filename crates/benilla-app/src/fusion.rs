//! Azeroth Warfare's fusion packets, opened to crates built on top (benilla-mw2): send this
//! tick's shots and the stance/gait state as [`Message`]s, and read the server's hits, kills and
//! streaks back as [`FusionEventsReceived`]. The wire format is `benilla_protocol::fusion`.

use bevy::prelude::*;

pub use benilla_protocol::fusion::{
    Event, EventKind, Gait, Shot, Stance, State, EVENT_ASSIST, EVENT_FATAL, EVENT_HEADSHOT,
    MAX_SHOTS_PER_BATCH, SHOT_ADS, SHOT_AIRBORNE, SHOT_CLAIMS_HEAD, STATE_AIMING, STATE_FFA_OFF,
    STATE_FFA_ON, STATE_RELOADING,
};
use benilla_protocol::{SessionEvent, SessionEventKind};

use crate::net::handlers::NetHandlerApp;
use crate::net::{ClientCommand, NetCommands};

/// Shots fired this frame; batched into `CMSG_FUSION_SHOTS` of at most [`MAX_SHOTS_PER_BATCH`].
#[derive(Message, Clone, Debug)]
pub struct SendFusionShots(pub Vec<Shot>);

/// The stance, gait, held gun and FFA requests, sent when they change.
#[derive(Message, Clone, Copy, Debug)]
pub struct SendFusionState(pub State);

/// `SMSG_FUSION_EVENTS`: what the server's referee decided about our shots and others' at us.
#[derive(Message, Clone, Debug)]
pub struct FusionEventsReceived(pub Vec<Event>);

pub(crate) struct FusionPlugin;

impl Plugin for FusionPlugin {
    fn build(&self, app: &mut App) {
        app.add_message::<SendFusionShots>()
            .add_message::<SendFusionState>()
            .add_message::<FusionEventsReceived>()
            .add_systems(PostUpdate, forward_sends)
            .net_handler(SessionEventKind::FusionEvents, on_fusion_events);
    }
}

fn forward_sends(
    mut shots: MessageReader<SendFusionShots>,
    mut states: MessageReader<SendFusionState>,
    net: Option<Res<NetCommands>>,
) {
    let Some(net) = net else {
        shots.clear();
        states.clear();
        return;
    };
    let mut pending: Vec<Shot> = shots.read().flat_map(|s| s.0.iter().copied()).collect();
    while !pending.is_empty() {
        let rest = pending.split_off(pending.len().min(MAX_SHOTS_PER_BATCH));
        let _ = net.0.send(ClientCommand::FusionShots { shots: pending });
        pending = rest;
    }
    if let Some(state) = states.read().last() {
        let _ = net.0.send(ClientCommand::FusionState { state: state.0 });
    }
}

fn on_fusion_events(In(ev): In<SessionEvent>, mut out: MessageWriter<FusionEventsReceived>) {
    if let SessionEvent::FusionEvents { events } = ev {
        out.write(FusionEventsReceived(events));
    }
}
