//! `fusion-bot`: Azeroth Warfare's load test. Logs in N headless characters, each holding a
//! stance and firing the starter rifle at the nearest unit it can see, so a server can be driven
//! toward the 100-player fights the design aims for. Prints shots sent and the server's verdicts.
//!
//! Accounts `<prefix>1..<prefix>N` must exist with one password; `--print-accounts` prints the
//! mangosd console lines that make them.
//!
//! Example: `cargo run --release -p benilla-protocol --bin fusion-bot -- localhost --bots 50`

use std::collections::HashMap;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;
use std::thread;
use std::time::{Duration, Instant};

use anyhow::{bail, Context, Result};
use benilla_protocol::fusion::{EventKind, Gait, Shot, Stance, State, STATE_FFA_ON};
use benilla_protocol::messages::{CharCreateReq, CHAR_CREATE_NAME_IN_USE, CHAR_CREATE_SUCCESS};
use benilla_protocol::{decode, EntityKind, SessionEvent, WorldSession, WORLD_PORT};
use clap::Parser;

#[derive(Parser, Clone)]
#[command(name = "fusion-bot", about = "Azeroth Warfare load test")]
struct Cli {
    /// Auth (realmd) host.
    host: String,
    /// How many bots.
    #[arg(long, default_value_t = 10)]
    bots: u32,
    /// Account name prefix: bots log in as <prefix>1, <prefix>2, ...
    #[arg(long, default_value = "awbot")]
    prefix: String,
    #[arg(long, default_value = "awbot")]
    password: String,
    /// Seconds to run.
    #[arg(long, default_value_t = 60)]
    seconds: u64,
    /// The weapon id to fire (the server's weapons.json; 1 is the seed's FAMAS).
    #[arg(long, default_value_t = 1)]
    weapon: u16,
    /// Seconds between trigger pulls.
    #[arg(long, default_value_t = 0.1)]
    fire_interval: f32,
    /// Print the console lines that create the accounts, and exit.
    #[arg(long)]
    print_accounts: bool,
}

#[derive(Default)]
struct Totals {
    online: AtomicU64,
    shots: AtomicU64,
    hits: AtomicU64,
    headshots: AtomicU64,
    kills: AtomicU64,
    hurt: AtomicU64,
    errors: AtomicU64,
}

/// A character name from the bot's index: letters only, as the server requires.
fn bot_name(i: u32) -> String {
    let mut n = i;
    let mut s = String::new();
    loop {
        s.insert(0, (b'a' + (n % 26) as u8) as char);
        n /= 26;
        if n == 0 {
            break;
        }
    }
    format!("Awbot{s}")
}

fn run_bot(cli: &Cli, index: u32, totals: &Totals) -> Result<()> {
    let account = format!("{}{}", cli.prefix, index);
    let logon = benilla_protocol::logon(&cli.host, &account, &cli.password)
        .with_context(|| format!("logon {account}"))?;
    let world_addr = logon
        .realms
        .first()
        .map(|r| r.address.clone())
        .unwrap_or_else(|| format!("{}:{}", cli.host, WORLD_PORT));
    let mut session = WorldSession::connect(&world_addr, &account, logon.session_key)?;
    let mut characters = session.char_enum()?;
    if characters.is_empty() {
        // Alternate the factions, so the bots have someone to fight.
        let (race, class) = if index.is_multiple_of(2) {
            (1, 1)
        } else {
            (2, 1)
        };
        let req = CharCreateReq {
            name: bot_name(index),
            race,
            class,
            gender: 0,
            skin: 0,
            face: 0,
            hair_style: 0,
            hair_color: 0,
            facial_hair: 0,
        };
        match session.create_character(&req)? {
            CHAR_CREATE_SUCCESS | CHAR_CREATE_NAME_IN_USE => {}
            other => bail!("{account}: character creation failed ({other:#x})"),
        }
        characters = session.char_enum()?;
    }
    let me = characters.first().context("no character")?.clone();
    session.player_login(me.guid)?;
    session.set_active_mover(me.guid)?;
    let (mut reader, mut writer) = session.into_split()?;
    totals.online.fetch_add(1, Ordering::Relaxed);

    // The read side: track every unit's position and count the server's verdicts.
    let (tx, rx) = std::sync::mpsc::channel::<SessionEvent>();
    thread::spawn(move || {
        while let Ok(packet) = reader.recv() {
            for ev in decode(packet) {
                if tx.send(ev).is_err() {
                    return;
                }
            }
        }
    });

    let start = Instant::now();
    let deadline = start + Duration::from_secs(cli.seconds);
    let mut units: HashMap<u64, [f32; 3]> = HashMap::new();
    let mut my_pos = [me.position.x, me.position.y, me.position.z];
    let stance = [Stance::Stand, Stance::Crouch, Stance::Prone][index as usize % 3];
    writer.fusion_state(&State {
        stance,
        gait: Gait::Still,
        flags: STATE_FFA_ON,
        held_weapon_id: cli.weapon,
        loaded_ammo: 0,
    })?;
    let mut next_shot = Instant::now();
    let mut mag = 0u32;
    while Instant::now() < deadline {
        while let Ok(ev) = rx.try_recv() {
            match ev {
                SessionEvent::ObjectCreate {
                    guid,
                    kind: EntityKind::Unit | EntityKind::Player,
                    position,
                    ..
                } => {
                    if guid == me.guid {
                        my_pos = position;
                    } else {
                        units.insert(guid, position);
                    }
                }
                SessionEvent::ObjectMove { guid, position, .. }
                | SessionEvent::UnitMove { guid, position, .. } => {
                    if guid == me.guid {
                        my_pos = position;
                    } else if let Some(p) = units.get_mut(&guid) {
                        *p = position;
                    }
                }
                SessionEvent::ObjectDestroyed(guid) => {
                    units.remove(&guid);
                }
                SessionEvent::FusionEvents { events } => {
                    for e in events {
                        let counter = match e.kind {
                            EventKind::Hit => &totals.hits,
                            EventKind::Kill => &totals.kills,
                            EventKind::Hurt => &totals.hurt,
                            EventKind::Ammo => {
                                mag = e.amount;
                                continue;
                            }
                            _ => continue,
                        };
                        counter.fetch_add(1, Ordering::Relaxed);
                        if e.kind == EventKind::Hit
                            && e.flags & benilla_protocol::fusion::EVENT_HEADSHOT != 0
                        {
                            totals.headshots.fetch_add(1, Ordering::Relaxed);
                        }
                    }
                }
                SessionEvent::Disconnected { reason, .. } => {
                    bail!("{account}: disconnected: {reason}")
                }
                _ => {}
            }
        }

        if Instant::now() >= next_shot {
            next_shot += Duration::from_secs_f32(cli.fire_interval);
            let eye = [
                my_pos[0],
                my_pos[1],
                my_pos[2] + 1.6 * [1.0, 0.65, 0.3][index as usize % 3],
            ];
            let nearest = units
                .iter()
                .map(|(g, p)| {
                    let d = [p[0] - eye[0], p[1] - eye[1], p[2] + 1.0 - eye[2]];
                    (*g, d, (d[0] * d[0] + d[1] * d[1] + d[2] * d[2]).sqrt())
                })
                .filter(|(_, _, dist)| *dist > 0.5 && *dist < 60.0)
                .min_by(|a, b| a.2.total_cmp(&b.2));
            if let Some((guid, d, dist)) = nearest {
                let shot = Shot {
                    time_ms: start.elapsed().as_millis() as u32,
                    weapon_id: cli.weapon,
                    flags: 0,
                    stance,
                    target_guid: guid,
                    origin: eye,
                    direction: [d[0] / dist, d[1] / dist, d[2] / dist],
                };
                writer.fusion_shots(&[shot])?;
                totals.shots.fetch_add(1, Ordering::Relaxed);
                mag = mag.saturating_sub(1);
            }
            // An empty magazine (the server's count) asks for a reload.
            if mag == 0 {
                writer.fusion_state(&State {
                    stance,
                    gait: Gait::Still,
                    flags: STATE_FFA_ON | benilla_protocol::fusion::STATE_RELOADING,
                    held_weapon_id: cli.weapon,
                    loaded_ammo: 0,
                })?;
                mag = 30;
            }
        }
        thread::sleep(Duration::from_millis(5));
    }
    totals.online.fetch_sub(1, Ordering::Relaxed);
    Ok(())
}

fn main() -> Result<()> {
    let cli = Cli::parse();
    if cli.print_accounts {
        for i in 1..=cli.bots {
            println!("account create {}{} {}", cli.prefix, i, cli.password);
        }
        return Ok(());
    }
    let totals = Arc::new(Totals::default());
    let mut handles = Vec::new();
    for i in 1..=cli.bots {
        let (cli, totals) = (cli.clone(), totals.clone());
        handles.push(thread::spawn(move || {
            if let Err(e) = run_bot(&cli, i, &totals) {
                eprintln!("bot {i}: {e:#}");
                totals.errors.fetch_add(1, Ordering::Relaxed);
            }
        }));
        // Stagger the logins, as realmd does not love a stampede.
        thread::sleep(Duration::from_millis(100));
    }
    let start = Instant::now();
    while handles.iter().any(|h| !h.is_finished()) {
        thread::sleep(Duration::from_secs(5));
        let t = &totals;
        let secs = start.elapsed().as_secs_f32().max(1.0);
        println!(
            "[{:>4.0}s] online {:>3}  shots {:>6} ({:.0}/s)  hits {:>5}  heads {:>4}  kills {:>4}  hurt {:>5}  errors {}",
            secs,
            t.online.load(Ordering::Relaxed),
            t.shots.load(Ordering::Relaxed),
            t.shots.load(Ordering::Relaxed) as f32 / secs,
            t.hits.load(Ordering::Relaxed),
            t.headshots.load(Ordering::Relaxed),
            t.kills.load(Ordering::Relaxed),
            t.hurt.load(Ordering::Relaxed),
            t.errors.load(Ordering::Relaxed),
        );
    }
    for h in handles {
        let _ = h.join();
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::bot_name;

    #[test]
    fn names_are_letters() {
        assert_eq!(bot_name(0), "Awbota");
        assert_eq!(bot_name(27), "Awbotbb");
        assert!(bot_name(1000).chars().all(|c| c.is_ascii_alphabetic()));
    }
}
