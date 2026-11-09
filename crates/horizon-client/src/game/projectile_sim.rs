//! Parallel and batched projectile simulation and raycasting.

use crate::game::{
    Game, creatures::MobWorld, deflect, dragon::SPECIAL, lan_pvp, native_projectiles::Impact,
    remote_projectiles, wind,
};
use horizon_client::{
    Result,
    arrows::{EntityHit, OwnerLookup},
    collisions::SceneCollisions,
    mobs::{Species, Who},
    powder_snow,
    projectiles::{Hit, Projectile, ProjectileKind, ProjectileWorld, clip_including_border},
};
use horizon_core::{
    geometry::{Aabb, Vec3, traverse_blocks},
    position::BlockPos,
    random::RandomSource,
};
use rayon::prelude::*;

#[derive(Clone, Copy)]
pub(crate) struct ProjWorldBase<'a> {
    pub world: &'a horizon_client::world::World,
    pub registry: &'a horizon_world::registry::BlockRegistry,
    pub facts: &'a horizon_items::blocks::BlockFacts,
    pub collision: &'a horizon_world::block_collision::BlockCollisionRegistry<'a>,
}

impl ProjWorldBase<'_> {
    pub fn in_water(&self, bounds: Aabb) -> bool {
        let b = bounds.deflate(Vec3::new(0.001, 0.001, 0.001));
        let (lo, hi) = (b.min_position(), b.max_position());
        let (ex, ez) = (
            ((lo.x + hi.x) / 2.0).floor() as i32,
            ((lo.z + hi.z) / 2.0).floor() as i32,
        );
        let cells = (
            [
                (lo.x.floor() as i32).min(ex),
                lo.y.floor() as i32,
                (lo.z.floor() as i32).min(ez),
            ],
            [
                (hi.x.ceil() as i32 - 1).max(ex),
                hi.y.ceil() as i32 - 1,
                (hi.z.ceil() as i32 - 1).max(ez),
            ],
        );
        if !self.world.may_hold(cells.0, cells.1, |s| {
            self.facts.get(s).fluid != horizon_client::blocks::Fluid::None
        }) {
            return false;
        }
        for x in lo.x.floor() as i32..=(hi.x.ceil() as i32 - 1) {
            for y in lo.y.floor() as i32..=(hi.y.ceil() as i32 - 1) {
                for z in lo.z.floor() as i32..=(hi.z.ceil() as i32 - 1) {
                    let f = self.facts.get(self.world.block(x, y, z));
                    if f.fluid != horizon_client::blocks::Fluid::Water {
                        continue;
                    }
                    let top = f64::from(y)
                        + if self.facts.get(self.world.block(x, y + 1, z)).fluid == f.fluid {
                            1.0
                        } else {
                            f64::from(f.fluid_level) / 9.0
                        };
                    if top >= lo.y {
                        return true;
                    }
                }
            }
        }
        false
    }
}

pub(crate) struct ProjWorld<'a> {
    pub base: ProjWorldBase<'a>,
    pub targets: Vec<(Who, Aabb)>,
    /// The world border's [minX, minZ, maxX, maxZ], which stops the ray as a
    /// block would (`clipIncludingBorder`); None: no border.
    pub border: Option<[f64; 4]>,
}

impl<'a> ProjWorld<'a> {
    pub fn new(w: &MobWorld<'a>, targets: Vec<(Who, Aabb)>, border: Option<[f64; 4]>) -> Self {
        Self {
            base: ProjWorldBase {
                world: w.world,
                registry: w.registry,
                facts: w.facts,
                collision: w.collision,
            },
            targets,
            border,
        }
    }
}

impl ProjectileWorld for ProjWorld<'_> {
    fn block_contacts(&self, entity: i32, kind: &str, from: Vec3, to: Vec3, bounds: Aabb) {
        horizon_client::block_contact::swept_inside(entity, kind, from, to, bounds, |p| {
            let state = self.base.world.block(p.x, p.y, p.z);
            (state, &self.base.facts.get(state).name)
        });
    }
    fn clip(&self, from: Vec3, to: Vec3) -> Option<(Vec3, [i32; 3], [i32; 3])> {
        let mut scene = SceneCollisions {
            world: self.base.world,
            binding: self.base.collision,
            subject: None,
        };
        let context = horizon_world::collision_context::CollisionContext::position(from.y);
        let r = traverse_blocks(from, to, 256, |p: BlockPos| {
            let state = self.base.world.block(p.x, p.y, p.z);
            if self.base.facts.is_air(state) {
                return None;
            }
            // ClipContext hands CollisionContext.of(projectile): scaffolding
            // is open for a projectile below it.
            let shape = self
                .base
                .collision
                .collision_shape_with_context(&mut scene, state, p, &context)
                .ok()?;
            shape.clip(from, to, p)
        });
        let hit = r.ok().flatten().map(|hit| {
            (
                hit.location,
                [hit.block.x, hit.block.y, hit.block.z],
                hit.direction.normal(),
            )
        });
        match self.border {
            Some(bounds) => clip_including_border(hit, from, to, bounds),
            None => hit,
        }
    }
    /// Entity.updateFluidInteraction over the projectile's box, not the
    /// block at its feet (#3072).
    fn in_water(&self, bounds: Aabb) -> bool {
        self.base.in_water(bounds)
    }
    fn targets(&self) -> &[(Who, Aabb)] {
        &self.targets
    }
    /// `BaseFireBlock.entityInside` for a thrown projectile: fire and soul
    /// fire set it alight for 160 ticks.
    fn ignites(&self, p: [i32; 3]) -> bool {
        let state = self.base.world.block(p[0], p[1], p[2]);
        matches!(
            &*self.base.facts.get(state).name,
            "minecraft:fire" | "minecraft:soul_fire"
        )
    }
    fn empty(&self, p: [i32; 3]) -> bool {
        self.base
            .facts
            .is_air(self.base.world.block(p[0], p[1], p[2]))
    }
    /// `WebBlock.entityInside` and `PowderSnowBlock.entityInside`.
    fn stuck_speed(&self, p: [i32; 3]) -> Option<Vec3> {
        horizon_client::projectiles::stuck_speed_of(
            &self
                .base
                .facts
                .get(self.base.world.block(p[0], p[1], p[2]))
                .name,
        )
    }
    fn bubble_column(&self, p: [i32; 3]) -> Option<(bool, bool)> {
        horizon_client::motion::bubble_column_at(
            self.base.world,
            self.base.registry,
            self.base.facts,
            self.base.collision,
            BlockPos::new(p[0], p[1], p[2]),
        )
        .map(|c| (c.drag, c.surface))
    }
    fn can_stand_on(&self, p: [i32; 3], entity_y: f64) -> bool {
        horizon_client::collisions::projectile_can_stand_on(
            self.base.world,
            self.base.collision,
            BlockPos::new(p[0], p[1], p[2]),
            entity_y,
        )
    }
}

pub(crate) fn border_crossing(p: &Projectile, bounds: Option<[f64; 4]>) -> bool {
    let Some(bounds) = bounds else { return false };
    let within =
        |x: f64, z: f64| x >= bounds[0] && x <= bounds[2] && z >= bounds[1] && z <= bounds[3];
    let (at, v) = (p.position, p.velocity);
    within(at.x, at.z) && !within(at.x + v.x, at.z + v.z)
}

/// `Projectile.isPickable`: a projectile in `#minecraft:redirectable_projectile`
/// (a ghast's fireball and both wind charges) is pickable, so another
/// projectile's `ProjectileUtil.getEntityHitResult` can strike it
/// (`Projectile.onHit` then redirects it). `PICKABLE_PROJECTILE_IDS + j` is
/// `projectiles[j]` during `tick_projectiles`, below the dragon's reserved ids.
pub(crate) const PICKABLE_PROJECTILE_IDS: u32 = SPECIAL - 0x10000;

/// The index of the projectile a `pickable_projectile_hitboxes` id stands for.
pub(crate) fn pickable_projectile_hit(id: u32) -> Option<usize> {
    (PICKABLE_PROJECTILE_IDS..SPECIAL)
        .contains(&id)
        .then(|| (id - PICKABLE_PROJECTILE_IDS) as usize)
}

pub(crate) fn pickable_projectile_hitboxes_into(
    projectiles: &[Projectile],
    alive: &[bool],
    skip: usize,
    out: &mut Vec<(Who, Aabb)>,
) {
    out.clear();
    out.extend(
        projectiles
            .iter()
            .enumerate()
            .filter(|(j, q)| *j != skip && alive[*j] && deflect::redirectable(q))
            .map(|(j, q)| (Who::Mob(PICKABLE_PROJECTILE_IDS + j as u32), q.aabb())),
    );
}

#[cfg(test)]
pub(crate) fn pickable_projectile_hitboxes(
    projectiles: &[Projectile],
    alive: &[bool],
    skip: usize,
) -> Vec<(Who, Aabb)> {
    let mut out = Vec::new();
    pickable_projectile_hitboxes_into(projectiles, alive, skip, &mut out);
    out
}

pub(super) struct PrecomputedOwnerLookup {
    pub non_players: Vec<[i32; 4]>,
}

impl OwnerLookup for PrecomputedOwnerLookup {
    fn non_player_entity(&self, uuid: [i32; 4]) -> bool {
        self.non_players.contains(&uuid)
    }
}

struct ParallelStep {
    keep: bool,
    hit: Hit,
    before: Option<Projectile>,
    snow_contacts: Vec<(BlockPos, Vec3, Vec3)>,
    shoot_event: Option<[f64; 3]>,
    has_trail: bool,
    bubble_tops: Vec<BlockPos>,
    arrow_events: Vec<(Vec3, horizon_client::arrows::ArrowEvent)>,
    vanished: bool,
    burst_top: bool,
}

impl Game<'_> {
    pub(crate) fn non_player_entity(&self, uuid: [i32; 4]) -> bool {
        let found = self.target_by_uuid(horizon_client::lan::playerdata::ints_uuid(uuid));
        !matches!(
            found,
            None | Some(
                crate::game::commands::Target::Player | crate::game::commands::Target::Guest(_)
            )
        )
    }

    pub(crate) fn arrow_rain(&self) -> impl Fn(BlockPos) -> bool + Sync + '_ {
        let rain_active = self.weather_levels(1.0).0 > 0.2;
        let world = &self.world;
        let properties = &self.pack.assets.properties;
        let biomes = self.res.biomes();
        move |p: BlockPos| {
            rain_active
                && world.light(p.x, p.y, p.z) >> 4 == 15
                && crate::game::render_tick::motion_blocking_height(world, properties, p.x, p.z)
                    <= p.y
                && match world.chunks.get(&(p.x >> 4, p.z >> 4)) {
                    Some(chunk) => {
                        biomes
                            .get(chunk.biome(p.x, p.y, p.z))
                            .snows_at(p.x, p.y, p.z)
                            == Some(false)
                    }
                    None => false,
                }
        }
    }

    /// The level as arrows see it.
    pub(crate) fn arrow_scene<'s>(
        &'s self,
        rain: &'s (dyn Fn(BlockPos) -> bool + Sync),
    ) -> horizon_client::arrows::ArrowScene<'s, 's> {
        self.arrow_scene_with_owners(rain, &())
    }

    pub(crate) fn arrow_scene_with_owners<'s>(
        &'s self,
        rain: &'s (dyn Fn(BlockPos) -> bool + Sync),
        owners: &'s (dyn horizon_client::arrows::OwnerLookup + Sync),
    ) -> horizon_client::arrows::ArrowScene<'s, 's> {
        horizon_client::arrows::ArrowScene {
            world: &self.world,
            registry: self.pack.assets.registry,
            facts: self.res.facts(),
            collision: self.collision,
            fluids: &self.motion.fluids,
            raining: rain,
            mob_griefing: self.ui.rules.bool("mob_griefing"),
            tnt_explodes: self.ui.rules.bool("tnt_explodes"),
            owners,
        }
    }

    pub(crate) fn tick_projectiles(&mut self) -> Result<()> {
        if self.creatures.projectiles.is_empty() {
            self.flush_lan_explosions();
            return self.flush_wind_bursts();
        }
        let mut projectiles = std::mem::take(&mut self.creatures.projectiles);
        let ticking = self.entity_ticking_area();
        let mut hits = Vec::new();
        let mut deflections: Vec<u32> = Vec::new();
        let mut arrow_events = Vec::new();
        let mut shoot_events = Vec::new();
        let mut trails = Vec::new();
        let mut bubble_tops = Vec::new();
        let mut snow_contacts = Vec::new();
        let mut keep_flags = vec![true; projectiles.len()];
        let mut winds = std::mem::take(&mut self.creatures.pickable_projectile_targets);
        // The impacts the mods may cancel (`native_projectile_impact`), on a server.
        let hear = self.native_hears_impacts();
        let mut impacts: Vec<Impact> = Vec::new();
        {
            let targets = self.projectile_targets();
            let breezes: Vec<u32> = self
                .mobs
                .iter()
                .filter(|m| m.alive() && m.species() == Species::Breeze)
                .map(|m| m.id)
                .collect();
            // Drawn only when a breeze could deflect a projectile: an empty
            // level must not move the level random every tick.
            let jitter = if breezes.is_empty() {
                0.0
            } else {
                self.world.random.next_float()
            };
            let mut non_players = Vec::new();
            for p in &projectiles {
                if p.owner.is_none()
                    && let Some(u) = p.owner_uuid
                    && self.non_player_entity(u)
                    && !non_players.contains(&u)
                {
                    non_players.push(u);
                }
            }
            let owner_lookup = PrecomputedOwnerLookup { non_players };
            let rain = self.arrow_rain();
            let scene = self.arrow_scene_with_owners(&rain, &owner_lookup);
            // Mobs' projectiles also hit the guests (game/lan_mobs.rs).
            let guests = self.lan_projectile_targets();
            let has_guests = !guests.is_empty();
            let targets_guests = if has_guests {
                let mut targets_guests = targets.clone();
                targets_guests.extend(guests);
                targets_guests
            } else {
                Vec::new()
            };
            let border = Some(self.border.bounds());
            let dimension = self.dim.id();
            let base = ProjWorldBase {
                world: &self.world,
                registry: self.pack.assets.registry,
                facts: self.res.facts(),
                collision: self.collision,
            };
            let pw = ProjWorld {
                base,
                targets,
                border,
            };
            let pw_guests = ProjWorld {
                base,
                targets: targets_guests,
                border,
            };
            // Every guest, not only the ones a projectile can hit: a dead or
            // spectating guest's pearl or arrow still resolves to them.
            let guest_entities = self.lan_guest_entities();
            // ThrownEnderpearl.tick: a dead player's pearl is discarded in
            // place of its tick while ender_pearls_vanish_on_death is on.
            let vanish = self.ui.rules.bool("ender_pearls_vanish_on_death");
            let host_dead = self.player_view().is_some_and(|p| !p.alive);
            let dead_guests: Vec<u32> = self.lan.as_ref().map_or_else(Vec::new, |h| {
                h.hub
                    .guests
                    .iter()
                    .filter(|g| g.dead)
                    .map(|g| g.conn)
                    .collect()
            });
            // `EntityTickList.forEach`: a projectile that joined during this
            // tick's entity pass (a mob's shot) first ticks on the next one; a
            // player's or a dispenser's, which join before the pass, tick now.
            let through = self.ticked_entities_through;
            if projectiles.len() >= 32 {
                let pickable_projectiles: Vec<(usize, Who, Aabb)> = projectiles
                    .iter()
                    .enumerate()
                    .filter(|(_, q)| deflect::redirectable(q))
                    .map(|(j, q)| (j, Who::Mob(PICKABLE_PROJECTILE_IDS + j as u32), q.aabb()))
                    .collect();

                let results: Vec<Option<ParallelStep>> = projectiles
                    .par_iter_mut()
                    .enumerate()
                    .map(|(i, p)| {
                        if p.id > through {
                            return None;
                        }
                        let at = p.position;
                        if !ticking(at.x.floor() as i32, at.z.floor() as i32) {
                            p.previous = at;
                            return None;
                        }
                        if vanish
                            && matches!(p.kind, ProjectileKind::EnderPearl)
                            && p.owner == Some(Who::Player)
                            && p.guest.map_or(host_dead, |c| dead_guests.contains(&c))
                        {
                            return Some(ParallelStep {
                                keep: false,
                                hit: Hit::None,
                                before: None,
                                snow_contacts: Vec::new(),
                                shoot_event: None,
                                has_trail: false,
                                bubble_tops: Vec::new(),
                                arrow_events: Vec::new(),
                                vanished: true,
                                burst_top: false,
                            });
                        }
                        let border_through =
                            p.arrow.is_none() && !p.stuck && border_crossing(p, border) && {
                                let (at, v) = (p.position, p.velocity);
                                let (mut pos, mut velocity) = ([at.x, at.y, at.z], [v.x, v.y, v.z]);
                                let mut collide = true;
                                let kind = p.kind.id();
                                let (vel, c) = (&mut velocity, &mut collide);
                                Self::native_projectile_world_border(
                                    dimension, p.id, kind, &mut pos, vel, c,
                                );
                                if pos.iter().chain(&velocity).all(|v| v.is_finite())
                                    && (pos != [at.x, at.y, at.z] || velocity != [v.x, v.y, v.z])
                                {
                                    p.position = Vec3::new(pos[0], pos[1], pos[2]);
                                    p.velocity = Vec3::new(velocity[0], velocity[1], velocity[2]);
                                    p.needs_sync = true;
                                }
                                !collide
                            };
                        let before = (hear && !p.stuck).then(|| p.clone());
                        let mob_shot = !matches!(p.owner, Some(Who::Player));
                        let pvp = !mob_shot && lan_pvp::meets_players(&p.kind);
                        let shooter = p
                            .guest
                            .filter(|_| pvp)
                            .and_then(|c| guest_entities.iter().find(|(g, _)| *g == c))
                            .map(|&(_, id)| Who::Mob(id));
                        if shooter.is_some() {
                            p.owner = shooter;
                        }
                        let use_guests = (mob_shot || pvp) && has_guests;
                        let has_winds = (matches!(p.kind, ProjectileKind::EnderPearl)
                            || p.arrow.is_some())
                            && pickable_projectiles.iter().any(|&(j, ..)| j != i);
                        let (keep, hit) = match (p.arrow.is_some(), use_guests, has_winds) {
                            (true, false, false) => {
                                horizon_client::arrows::tick(p, &scene, &pw.targets)
                            }
                            (true, true, false) => {
                                horizon_client::arrows::tick(p, &scene, &pw_guests.targets)
                            }
                            (true, with_guests, true) => {
                                let mut targets = if with_guests {
                                    pw_guests.targets.clone()
                                } else {
                                    pw.targets.clone()
                                };
                                targets.extend(
                                    pickable_projectiles
                                        .iter()
                                        .filter(|&&(j, ..)| j != i)
                                        .map(|&(_, who, aabb)| (who, aabb)),
                                );
                                horizon_client::arrows::tick(p, &scene, &targets)
                            }
                            (false, with_guests, false) if border_through => p.tick(&ProjWorld {
                                base,
                                targets: if with_guests {
                                    pw_guests.targets.clone()
                                } else {
                                    pw.targets.clone()
                                },
                                border: None,
                            }),
                            (false, false, false) => p.tick(&pw),
                            (false, true, false) => p.tick(&pw_guests),
                            (false, pvp_shot, true) => {
                                let mut targets = if pvp_shot {
                                    pw_guests.targets.clone()
                                } else {
                                    pw.targets.clone()
                                };
                                targets.extend(
                                    pickable_projectiles
                                        .iter()
                                        .filter(|&&(j, ..)| j != i)
                                        .map(|&(_, who, aabb)| (who, aabb)),
                                );
                                p.tick(&ProjWorld {
                                    base,
                                    targets,
                                    border: border.filter(|_| !border_through),
                                })
                            }
                        };
                        let mut snow_contacts = Vec::new();
                        if (p.is_thrown() && !matches!(p.kind, ProjectileKind::LlamaSpit))
                            || p.is_arrow() && p.arrow.as_ref().is_none_or(|a| !a.no_physics)
                        {
                            for block in powder_snow::contacts(
                                p.previous,
                                p.position,
                                p.bounds(),
                                false,
                                |at| {
                                    &*base.facts.get(base.world.block(at.x, at.y, at.z)).name
                                        == "minecraft:powder_snow"
                                },
                            ) {
                                snow_contacts.push((block, p.position, p.velocity));
                            }
                        }
                        if shooter.is_some() {
                            p.owner = Some(Who::Player);
                        }
                        let shoot_event = p.take_shoot_event().map(|at| [at.x, at.y, at.z]);
                        let has_trail = remote_projectiles::has_smoke_trail(&p.kind);
                        let mut bubble_tops = Vec::new();
                        if keep && hit == Hit::None && matches!(p.kind, ProjectileKind::EnderPearl)
                        {
                            let (at, h) = (p.position, 0.125);
                            horizon_client::motion::bubble_column_push_over(
                                base.world,
                                base.registry,
                                base.facts,
                                base.collision,
                                Aabb::new(
                                    Vec3::new(at.x - h, at.y, at.z - h),
                                    Vec3::new(at.x + h, at.y + 2.0 * h, at.z + h),
                                ),
                                &mut p.velocity,
                                |pos| bubble_tops.push(pos),
                            );
                        }
                        let mut arrow_events = Vec::new();
                        if let Some(a) = p.arrow.as_mut() {
                            arrow_events.extend(a.events.drain(..).map(|e| (p.position, e)));
                        }
                        let wind = matches!(
                            p.kind,
                            ProjectileKind::WindCharge | ProjectileKind::BreezeWindCharge
                        );
                        let burst_top = wind && keep && p.position.y > 350.0;
                        Some(ParallelStep {
                            keep,
                            hit,
                            before,
                            snow_contacts,
                            shoot_event,
                            has_trail,
                            bubble_tops,
                            arrow_events,
                            vanished: false,
                            burst_top,
                        })
                    })
                    .collect();

                // Sequential pass: apply results in original projectile order.
                for i in 0..projectiles.len() {
                    let Some(res) = &results[i] else {
                        continue;
                    };
                    if res.vanished {
                        keep_flags[i] = false;
                        continue;
                    }
                    let p = &mut projectiles[i];
                    for contact in &res.snow_contacts {
                        snow_contacts.push(*contact);
                    }
                    if let Some(at) = res.shoot_event {
                        shoot_events.push((p.owner, p.guest, at));
                    }
                    if res.has_trail {
                        trails.push(p.position);
                    }
                    for top in &res.bubble_tops {
                        bubble_tops.push(*top);
                    }
                    let events_from = arrow_events.len();
                    let ordinary_arrow = p.is_arrow();
                    for (pos, e) in &res.arrow_events {
                        if ordinary_arrow && *e == horizon_client::arrows::ArrowEvent::ShootEvent {
                            shoot_events.push((p.owner, p.guest, pos.components()));
                        }
                        arrow_events.push((*pos, *e));
                    }
                    let wind = matches!(
                        p.kind,
                        ProjectileKind::WindCharge | ProjectileKind::BreezeWindCharge
                    );
                    if let Hit::Entity {
                        who: Who::Mob(id), ..
                    } = res.hit
                        && !wind
                        && breezes.contains(&id)
                    {
                        wind::reverse(p, jitter);
                        if p.arrow.is_some() {
                            horizon_client::arrows::finish_entity_hit(
                                p,
                                &scene,
                                EntityHit::Deflected,
                            );
                            let a = p.arrow.as_mut().expect("an arrow");
                            arrow_events.extend(a.events.drain(..).map(|e| {
                                if ordinary_arrow
                                    && e == horizon_client::arrows::ArrowEvent::ShootEvent
                                {
                                    shoot_events.push((p.owner, p.guest, p.position.components()));
                                }
                                (p.position, e)
                            }));
                        }
                        deflections.push(id);
                        continue;
                    }
                    if res.burst_top {
                        hits.push((
                            p.clone(),
                            Hit::Block {
                                pos: [0; 3],
                                at: p.position,
                                face: [0; 3],
                            },
                        ));
                        keep_flags[i] = false;
                        continue;
                    }
                    if res.hit != Hit::None {
                        if let Some(before) = &res.before {
                            impacts.push(Impact {
                                index: i,
                                before: before.clone(),
                                hit: res.hit,
                                hit_slot: hits.len(),
                                events: events_from..arrow_events.len(),
                            });
                        }
                        hits.push((p.clone(), res.hit));
                    }
                    keep_flags[i] = res.keep;
                    if let Hit::Entity {
                        who: Who::Mob(id), ..
                    } = res.hit
                        && let Some(j) = pickable_projectile_hit(id)
                    {
                        let (owner, owner_uuid, guest) = (p.owner, p.owner_uuid, p.guest);
                        let look = self.owner_look(owner, guest);
                        let wind = &mut projectiles[j];
                        deflect::aim_deflect(wind, look, owner, owner_uuid, guest);
                    }
                }
            } else {
                for i in 0..projectiles.len() {
                    if projectiles[i].id > through {
                        continue;
                    }
                    // ServerLevel.tick: losing an entity-ticking ticket must not
                    // advance projectile flags or motion while its chunk unloads.
                    let at = projectiles[i].position;
                    if !ticking(at.x.floor() as i32, at.z.floor() as i32) {
                        projectiles[i].previous = at;
                        continue;
                    }
                    // A ghast's fireball or a wind charge is pickable, so a pearl, an
                    // arrow or a trident can strike it (`Projectile.isPickable`,
                    // `Projectile.onHit`).
                    winds.clear();
                    if matches!(projectiles[i].kind, ProjectileKind::EnderPearl)
                        || projectiles[i].arrow.is_some()
                    {
                        pickable_projectile_hitboxes_into(&projectiles, &keep_flags, i, &mut winds);
                    }
                    let p = &mut projectiles[i];
                    if vanish
                        && matches!(p.kind, ProjectileKind::EnderPearl)
                        && p.owner == Some(Who::Player)
                        && p.guest.map_or(host_dead, |c| dead_guests.contains(&c))
                    {
                        keep_flags[i] = false;
                        continue;
                    }
                    // MC-310476: the mods may let a thrown projectile through the world border.
                    let through = p.arrow.is_none() && !p.stuck && border_crossing(p, border) && {
                        let (at, v) = (p.position, p.velocity);
                        let (mut pos, mut velocity) = ([at.x, at.y, at.z], [v.x, v.y, v.z]);
                        let mut collide = true;
                        let kind = p.kind.id();
                        let (vel, c) = (&mut velocity, &mut collide);
                        Self::native_projectile_world_border(
                            dimension, p.id, kind, &mut pos, vel, c,
                        );
                        if pos.iter().chain(&velocity).all(|v| v.is_finite())
                            && (pos != [at.x, at.y, at.z] || velocity != [v.x, v.y, v.z])
                        {
                            p.position = Vec3::new(pos[0], pos[1], pos[2]);
                            p.velocity = Vec3::new(velocity[0], velocity[1], velocity[2]);
                            p.needs_sync = true;
                        }
                        !collide
                    };
                    // The projectile as it was, to fly on if a mod cancels its impact.
                    let before = (hear && !p.stuck).then(|| p.clone());
                    let mob_shot = !matches!(p.owner, Some(Who::Player));
                    // A player's arrows, snowballs and eggs meet every player; a
                    // guest's flies from that guest (Projectile.getOwner), not
                    // from the host, until it has left them.
                    let pvp = !mob_shot && lan_pvp::meets_players(&p.kind);
                    let shooter = p
                        .guest
                        .filter(|_| pvp)
                        .and_then(|c| guest_entities.iter().find(|(g, _)| *g == c))
                        .map(|&(_, id)| Who::Mob(id));
                    if shooter.is_some() {
                        p.owner = shooter;
                    }
                    let use_guests = (mob_shot || pvp) && has_guests;
                    let (keep, hit) = match (p.arrow.is_some(), use_guests, winds.is_empty()) {
                        (true, false, true) => horizon_client::arrows::tick(p, &scene, &pw.targets),
                        (true, true, true) => {
                            horizon_client::arrows::tick(p, &scene, &pw_guests.targets)
                        }
                        (true, with_guests, false) => {
                            let mut targets = if with_guests {
                                pw_guests.targets.clone()
                            } else {
                                pw.targets.clone()
                            };
                            targets.extend_from_slice(&winds);
                            horizon_client::arrows::tick(p, &scene, &targets)
                        }
                        // Through the border: this tick's ray does not stop on it.
                        (false, with_guests, true) if through => p.tick(&ProjWorld {
                            base,
                            targets: if with_guests {
                                pw_guests.targets.clone()
                            } else {
                                pw.targets.clone()
                            },
                            border: None,
                        }),
                        (false, false, true) => p.tick(&pw),
                        (false, true, true) => p.tick(&pw_guests),
                        (false, pvp_shot, false) => {
                            let mut targets = if pvp_shot {
                                pw_guests.targets.clone()
                            } else {
                                pw.targets.clone()
                            };
                            targets.extend_from_slice(&winds);
                            p.tick(&ProjWorld {
                                base,
                                targets,
                                border: border.filter(|_| !through),
                            })
                        }
                    };
                    if (p.is_thrown() && !matches!(p.kind, ProjectileKind::LlamaSpit))
                        || p.is_arrow() && p.arrow.as_ref().is_none_or(|a| !a.no_physics)
                    {
                        for block in
                            powder_snow::contacts(p.previous, p.position, p.bounds(), false, |at| {
                                &*base.facts.get(base.world.block(at.x, at.y, at.z)).name
                                    == "minecraft:powder_snow"
                            })
                        {
                            snow_contacts.push((block, p.position, p.velocity));
                        }
                    }
                    if shooter.is_some() {
                        p.owner = Some(Who::Player);
                    }
                    if let Some(at) = p.take_shoot_event() {
                        shoot_events.push((p.owner, p.guest, [at.x, at.y, at.z]));
                    }
                    // AbstractHurtingProjectile.tick ends with createParticleTrail.
                    if remote_projectiles::has_smoke_trail(&p.kind) {
                        trails.push(p.position);
                    }
                    // ThrownEnderpearl.onAboveBubbleColumn / onInsideBubbleColumn.
                    if keep && hit == Hit::None && matches!(p.kind, ProjectileKind::EnderPearl) {
                        let (at, h) = (p.position, 0.125);
                        horizon_client::motion::bubble_column_push_over(
                            base.world,
                            base.registry,
                            base.facts,
                            base.collision,
                            Aabb::new(
                                Vec3::new(at.x - h, at.y, at.z - h),
                                Vec3::new(at.x + h, at.y + 2.0 * h, at.z + h),
                            ),
                            &mut p.velocity,
                            |pos| bubble_tops.push(pos),
                        );
                    }
                    let events_from = arrow_events.len();
                    let ordinary_arrow = p.is_arrow();
                    if let Some(a) = p.arrow.as_mut() {
                        arrow_events.extend(a.events.drain(..).map(|e| {
                            if ordinary_arrow && e == horizon_client::arrows::ArrowEvent::ShootEvent
                            {
                                shoot_events.push((p.owner, p.guest, p.position.components()));
                            }
                            // Retain even the no-op marker: native impact ranges index this list.
                            (p.position, e)
                        }));
                    }
                    let wind = matches!(
                        p.kind,
                        ProjectileKind::WindCharge | ProjectileKind::BreezeWindCharge
                    );
                    // Entity.deflection: a breeze turns every projectile but wind charges away.
                    if let Hit::Entity {
                        who: Who::Mob(id), ..
                    } = hit
                        && !wind
                        && breezes.contains(&id)
                    {
                        wind::reverse(p, jitter);
                        if p.arrow.is_some() {
                            horizon_client::arrows::finish_entity_hit(
                                p,
                                &scene,
                                EntityHit::Deflected,
                            );
                            let a = p.arrow.as_mut().expect("an arrow");
                            arrow_events.extend(a.events.drain(..).map(|e| {
                                if ordinary_arrow
                                    && e == horizon_client::arrows::ArrowEvent::ShootEvent
                                {
                                    shoot_events.push((p.owner, p.guest, p.position.components()));
                                }
                                (p.position, e)
                            }));
                        }
                        deflections.push(id);
                        continue;
                    }
                    // AbstractWindCharge.tick: out of the world's top it bursts.
                    if wind && keep && p.position.y > 350.0 {
                        hits.push((
                            p.clone(),
                            Hit::Block {
                                pos: [0; 3],
                                at: p.position,
                                face: [0; 3],
                            },
                        ));
                        keep_flags[i] = false;
                        continue;
                    }
                    if hit != Hit::None {
                        if let Some(before) = before {
                            impacts.push(Impact {
                                index: i,
                                before,
                                hit,
                                hit_slot: hits.len(),
                                events: events_from..arrow_events.len(),
                            });
                        }
                        hits.push((p.clone(), hit));
                    }
                    keep_flags[i] = keep;
                    // Projectile.onHit: the redirectable projectile it hit (a ghast's
                    // fireball or a wind charge) turns before its own tick.
                    if let Hit::Entity {
                        who: Who::Mob(id), ..
                    } = hit
                        && let Some(j) = pickable_projectile_hit(id)
                    {
                        let (owner, owner_uuid, guest) = (p.owner, p.owner_uuid, p.guest);
                        let look = self.owner_look(owner, guest);
                        let wind = &mut projectiles[j];
                        deflect::aim_deflect(wind, look, owner, owner_uuid, guest);
                    }
                }
            }
        }
        self.creatures.pickable_projectile_targets = winds;
        for (block, position, movement) in snow_contacts {
            self.powder_snow_particle(block, position, movement);
        }
        let listed = keep_flags.clone();
        let mut idx = 0;
        projectiles.retain(|_| {
            let k = keep_flags[idx];
            idx += 1;
            k
        });
        if !impacts.is_empty() {
            self.native_settle_impacts(
                &mut projectiles,
                &listed,
                impacts,
                &mut hits,
                &mut arrow_events,
            );
        }
        for (owner, guest, at) in shoot_events {
            self.projectile_shoot_event(owner, guest, at);
        }
        self.smoke_trails(trails);
        self.dragon_breath_trails();
        for pos in bubble_tops {
            self.bubble_column_particles(pos);
        }
        for id in deflections {
            if let Some(index) = self.mobs.iter().position(|m| m.id == id) {
                self.play_mob_bound_sound(
                    index,
                    "minecraft:entity.breeze.deflect",
                    5,
                    1.0,
                    1.0,
                    false,
                );
            }
        }
        for (at, event) in arrow_events {
            self.arrow_event(at, event);
        }
        self.creatures.projectiles.append(&mut projectiles);
        for (p, hit) in hits {
            // A player's arrow on another player lands as the victim's hurt.
            if let Some(victim) = self.projectile_pvp_victim(&p, hit) {
                self.projectile_hits_player(&p, hit, victim)?;
                continue;
            }
            // A guest's projectile lands as theirs: a pearl takes them, what
            // it hurts answers to them. One hitting the host (`Who::Player`
            // here) stays the host's hit.
            match p.guest {
                Some(conn)
                    if !matches!(
                        hit,
                        Hit::Entity {
                            who: Who::Player,
                            ..
                        }
                    ) =>
                {
                    self.guest_projectile_hit(conn, &p, hit)?;
                }
                _ => self.projectile_hit(&p, hit)?,
            }
        }
        self.flush_lan_explosions();
        self.flush_wind_bursts()
    }

    /// `projectile_hit` for a projectile the guest `conn` shot, run as them;
    /// as the host's when they have left.
    fn guest_projectile_hit(&mut self, conn: u32, p: &Projectile, hit: Hit) -> Result<()> {
        let Some(mut host) = self.lan.take() else {
            return self.projectile_hit(p, hit);
        };
        let result = self.as_guest(&mut host, conn, |g| g.projectile_hit(p, hit));
        // The mob hit now answers to the guest (LivingEntity.lastHurtByPlayer).
        if result.is_some()
            && let Hit::Entity {
                who: Who::Mob(id), ..
            } = hit
            && self.mobs.iter().any(|m| m.id == id)
        {
            host.mob_players.insert(id, conn);
        }
        self.lan = Some(host);
        match result {
            Some(r) => r,
            None => self.projectile_hit(p, hit),
        }
    }
}
