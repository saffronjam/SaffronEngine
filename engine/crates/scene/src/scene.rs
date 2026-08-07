//! The wrapped ECS world, the `Entity` handle, and the `Scene` access surface.
//!
//! The ECS crate behind the world is an internal detail: every downstream crate goes through
//! [`Scene`] and [`Entity`], never through `hecs::` directly.

use std::any::TypeId;
use std::collections::{HashMap, HashSet, VecDeque};
use std::sync::Arc;

use glam::Mat4;
use saffron_core::Uuid;

use crate::component::{
    ComponentOrder, IdComponent, Name, Relationship, Transform, VegetationField,
};
use crate::environment::{AssetCatalog, SceneEnvironment};
use crate::journal::{
    SceneEntityRevisions, SceneJournalCursor, SceneJournalRead, SceneMutation, SceneMutationKind,
    SceneRevision, SceneWorldTransformState,
};

const DEFAULT_JOURNAL_CAPACITY: usize = 65_536;

#[derive(Clone, Copy, Debug, Default)]
struct EntityRevisionState {
    published: SceneEntityRevisions,
    resolved_local: SceneRevision,
    resolved_hierarchy: SceneRevision,
    resolved_parent_world: SceneRevision,
    current_world: Option<Mat4>,
    previous_world: Option<Mat4>,
}

/// The component trait every stored type satisfies.
///
/// Re-exported from the internal ECS so callers bound generics on `crate::Component`
/// and never name the ECS crate. (`hecs::Component` is a blanket trait over
/// `'static + Send + Sync` types, so plain component structs satisfy it for free.)
pub use hecs::Component;

/// The query trait `for_each` is generic over: a tuple of component references such
/// as `(&Transform, &mut Camera)`.
///
/// Re-exported from the internal ECS so callers write `scene.for_each::<(&C,), _>(…)`
/// and never name the ECS crate.
pub use hecs::Query;

/// A lightweight, copyable handle to an entity.
///
/// Wraps the internal ECS's generational handle so the ECS type never leaks. An
/// `Entity` is a plain index plus generation, so it never dangles against a relocated
/// `Scene`; a handle that outlives its entity is caught by [`Scene::valid`]. Cross-`Scene`
/// lookups must go by [`Uuid`] ([`Scene::find_entity_by_uuid`]) — handles can coincide
/// between worlds and alias silently.
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
pub struct Entity(hecs::Entity);

impl Entity {
    /// The sentinel non-entity.
    ///
    /// Used inside the runtime hierarchy caches that store a flat `Vec<Entity>` (the
    /// skinned-mesh `bone_handles`) where an unresolved slot needs a value rather than an
    /// `Option`. [`Scene::valid`] reports `false` for it, so it never resolves a
    /// component or a world matrix.
    pub const NULL: Entity = Entity(hecs::Entity::DANGLING);

    /// The handle's raw index+generation bits: a total order that is a pure function of
    /// scene construction order, for deterministic processing of unordered entity sets
    /// (a uuid order would differ run to run for freshly minted ids).
    pub(crate) fn allocation_bits(self) -> u64 {
        self.0.to_bits().get()
    }
}

/// One resolved local wind source and the entity that placed it.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct PlacedWindSource {
    /// The stable id of the entity carrying the [`WindSource`](crate::WindSource).
    pub entity: Uuid,
    /// The source as the shared field composes it.
    pub source: saffron_wind::LocalWindSource,
}

/// The scene: the ECS world, the environment, and a borrowed asset catalog.
///
/// `catalog` is an `Option<Arc<AssetCatalog>>` (a read-shared handle the asset layer
/// hands the scene). It is never serialized.
pub struct Scene {
    world: hecs::World,
    /// Scene-wide environment state (sky, ambient, atmosphere).
    pub environment: SceneEnvironment,
    /// The borrowed, read-shared asset catalog; never serialized.
    pub catalog: Option<Arc<AssetCatalog>>,
    instance: Uuid,
    revision: SceneRevision,
    journal_base: SceneRevision,
    journal_capacity: usize,
    journal: VecDeque<SceneMutation>,
    entity_revisions: HashMap<Entity, EntityRevisionState>,
    dirty_world: HashSet<Entity>,
}

impl Default for Scene {
    fn default() -> Self {
        Self {
            world: hecs::World::new(),
            environment: SceneEnvironment::default(),
            catalog: None,
            instance: Uuid::new(),
            revision: SceneRevision::ZERO,
            journal_base: SceneRevision::ZERO,
            journal_capacity: DEFAULT_JOURNAL_CAPACITY,
            journal: VecDeque::new(),
            entity_revisions: HashMap::new(),
            dirty_world: HashSet::new(),
        }
    }
}

impl Scene {
    /// Constructs an empty scene with a default environment and no catalog.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Whether `entity` is a live handle in this scene.
    #[must_use]
    pub fn valid(&self, entity: Entity) -> bool {
        self.world.contains(entity.0)
    }

    /// Adds component `c` to `entity`, replacing any existing one of the same type.
    ///
    /// # Errors
    ///
    /// [`Error::InvalidEntity`](crate::Error::InvalidEntity) if `entity` is stale, or
    /// [`Error::ImmutableIdentity`](crate::Error::ImmutableIdentity) for [`IdComponent`].
    pub fn add_component<C: Component>(&mut self, entity: Entity, c: C) -> crate::Result<()> {
        if !self.valid(entity) {
            return Err(crate::Error::InvalidEntity);
        }
        if TypeId::of::<C>() == TypeId::of::<IdComponent>()
            || (TypeId::of::<C>() == TypeId::of::<crate::PlantOrigin>()
                && self.has_component::<crate::PlantOrigin>(entity))
        {
            return Err(crate::Error::ImmutableIdentity);
        }
        if TypeId::of::<C>() == TypeId::of::<VegetationField>()
            && self
                .world
                .query::<(hecs::Entity, &VegetationField)>()
                .iter()
                .any(|(owner, _)| owner != entity.0)
        {
            return Err(crate::Error::SingletonComponent("VegetationField"));
        }
        let existed = self.has_component::<C>(entity);
        self.world
            .insert_one(entity.0, c)
            .map_err(|_| crate::Error::InvalidEntity)?;
        self.record_component_mutation(
            entity,
            TypeId::of::<C>(),
            if existed {
                SceneMutationKind::ComponentUpdated(TypeId::of::<C>())
            } else {
                SceneMutationKind::ComponentAdded(TypeId::of::<C>())
            },
        );
        Ok(())
    }

    /// Whether `entity` carries a component of type `C`. A stale handle reports `false`.
    #[must_use]
    pub fn has_component<C: Component>(&self, entity: Entity) -> bool {
        self.world.satisfies::<&C>(entity.0)
    }

    /// Removes the component of type `C` from `entity` if present. A missing component or
    /// a stale handle is a no-op.
    ///
    /// # Panics
    ///
    /// Panics when asked to remove [`IdComponent`], whose value and presence define stable identity.
    pub fn remove_component<C: Component>(&mut self, entity: Entity) {
        assert_ne!(
            TypeId::of::<C>(),
            TypeId::of::<IdComponent>(),
            "IdComponent is immutable; destroy the entity instead"
        );
        assert_ne!(
            TypeId::of::<C>(),
            TypeId::of::<crate::PlantOrigin>(),
            "PlantOrigin is immutable; demote the plant instead"
        );
        if self.world.remove_one::<C>(entity.0).is_ok() {
            self.record_component_mutation(
                entity,
                TypeId::of::<C>(),
                SceneMutationKind::ComponentRemoved(TypeId::of::<C>()),
            );
        }
    }

    /// Runs `f` with a shared reference to `entity`'s component of type `C`, returning
    /// its result. Scoped to a borrow rather than handing out a long-lived reference into
    /// the ECS storage.
    ///
    /// # Errors
    ///
    /// [`Error::MissingComponent`](crate::Error::MissingComponent) if `entity` does
    /// not carry a `C` (also covers a stale handle), or
    /// [`Error::ImmutableIdentity`](crate::Error::ImmutableIdentity) for [`IdComponent`].
    pub fn with_component<C: Component, R>(
        &self,
        entity: Entity,
        f: impl FnOnce(&C) -> R,
    ) -> crate::Result<R> {
        let guard = self
            .world
            .get::<&C>(entity.0)
            .map_err(|_| crate::Error::MissingComponent)?;
        Ok(f(&guard))
    }

    /// Runs `f` with a mutable reference to `entity`'s component of type `C`, returning
    /// its result.
    ///
    /// # Errors
    ///
    /// [`Error::MissingComponent`](crate::Error::MissingComponent) if `entity` does
    /// not carry a `C` (also covers a stale handle).
    pub fn with_component_mut<C: Component, R>(
        &mut self,
        entity: Entity,
        f: impl FnOnce(&mut C) -> R,
    ) -> crate::Result<R> {
        if TypeId::of::<C>() == TypeId::of::<IdComponent>()
            || TypeId::of::<C>() == TypeId::of::<crate::PlantOrigin>()
        {
            return Err(crate::Error::ImmutableIdentity);
        }
        let mut guard = self
            .world
            .get::<&mut C>(entity.0)
            .map_err(|_| crate::Error::MissingComponent)?;
        let result = f(&mut guard);
        drop(guard);
        self.record_component_mutation(
            entity,
            TypeId::of::<C>(),
            SceneMutationKind::ComponentUpdated(TypeId::of::<C>()),
        );
        Ok(result)
    }

    /// Returns a copy of `entity`'s component of type `C`, for the common read of a
    /// small `Copy` component (a convenience over [`Scene::with_component`]).
    ///
    /// # Errors
    ///
    /// [`Error::MissingComponent`](crate::Error::MissingComponent) if `entity` does
    /// not carry a `C`.
    pub fn component<C: Component + Copy>(&self, entity: Entity) -> crate::Result<C> {
        self.with_component::<C, _>(entity, |c| *c)
    }

    /// The resolved local wind sources: every enabled [`WindSource`](crate::WindSource) entity,
    /// anchored at its world position with its world +Z as the forward axis, each paired with
    /// the entity that placed it so an inspector can name what it is reading.
    #[must_use]
    pub fn local_wind_sources(&mut self) -> Vec<PlacedWindSource> {
        let mut sources = Vec::new();
        self.for_each::<(
            &crate::WindSource,
            &crate::WorldTransform,
            &crate::IdComponent,
        ), _>(|_, (source, world, id)| {
            if !source.enabled {
                return;
            }
            let position = world.matrix.w_axis.truncate();
            let forward = world.matrix.z_axis.truncate().normalize_or_zero();
            sources.push(PlacedWindSource {
                entity: id.id,
                source: saffron_wind::LocalWindSource {
                    kind: source.kind,
                    position: glam::DVec3::new(
                        f64::from(position.x),
                        f64::from(position.y),
                        f64::from(position.z),
                    ),
                    direction: forward,
                    strength: source.strength,
                    radius: source.radius,
                    falloff: source.falloff,
                },
            });
        });
        sources
    }

    /// The composed field's local sources alone, in the same order, for a consumer that only
    /// samples them.
    #[must_use]
    pub fn local_wind_source_field(&mut self) -> Vec<saffron_wind::LocalWindSource> {
        self.local_wind_sources()
            .into_iter()
            .map(|placed| placed.source)
            .collect()
    }

    /// Iterates every entity carrying the query components, invoking `f` with the entity handle and
    /// its component references.
    ///
    /// `Q` is a tuple of component references — `(&Transform,)`, `(&Transform, &mut Camera)` — so the
    /// callback receives `(Entity, &C…)` exactly as the query tuple spells. Iteration order is
    /// unspecified; roots-first ordering comes from the hierarchy walk, not the view.
    pub fn for_each<Q, F>(&mut self, mut f: F)
    where
        Q: Query,
        F: for<'a> FnMut(Entity, <Q as Query>::Item<'a>),
    {
        let mut mutated_types = Vec::new();
        <Q::Fetch as hecs::Fetch>::for_each_borrow(|component, unique| {
            if unique && !mutated_types.contains(&component) {
                mutated_types.push(component);
            }
        });
        assert!(
            !mutated_types.contains(&TypeId::of::<IdComponent>()),
            "mutable scene queries cannot borrow IdComponent"
        );
        let mut mutated_entities = Vec::new();
        {
            let query = self.world.query_mut::<(hecs::Entity, Q)>();
            for (handle, item) in query {
                let entity = Entity(handle);
                f(entity, item);
                if !mutated_types.is_empty() {
                    mutated_entities.push(entity);
                }
            }
        }
        for entity in mutated_entities {
            for &component in &mutated_types {
                self.record_component_mutation(
                    entity,
                    component,
                    SceneMutationKind::ComponentUpdated(component),
                );
            }
        }
    }

    /// Creates an entity seeded with the standard authored component set: a freshly minted
    /// [`IdComponent`], a [`Name`], a default [`Transform`], a root [`Relationship`], and a
    /// [`ComponentOrder`] of `["Name", "Transform"]`.
    pub fn create_entity(&mut self, name: impl Into<String>) -> Entity {
        let handle = self.world.spawn((
            IdComponent::new(Uuid::new()),
            Name { name: name.into() },
            Transform::default(),
            Relationship::default(),
            ComponentOrder {
                names: vec!["Name".to_string(), "Transform".to_string()],
            },
        ));
        let entity = Entity(handle);
        self.record_entity_created(entity);
        entity
    }

    /// Creates a bare entity carrying only an [`IdComponent`] for the given uuid.
    ///
    /// Unlike [`Scene::create_entity`], the id is *preserved*, not minted, and none of the
    /// authored seed components (`Name` / `Transform` / `Relationship` / `ComponentOrder`)
    /// are added — the scene loader fills them from the document. Used only by
    /// [`Scene::scene_from_json`](crate::Scene); the relink pass defaults a root
    /// [`Relationship`] onto any entity the document left without one.
    pub fn spawn_with_id(&mut self, id: Uuid) -> Entity {
        let entity = Entity(self.world.spawn((IdComponent::new(id),)));
        self.record_entity_created(entity);
        entity
    }

    /// Removes every entity from the scene.
    ///
    /// Leaves the environment and the catalog handle untouched; only the ECS world is
    /// emptied. Used by the scene loader before repopulating from a document.
    pub fn clear(&mut self) {
        let destroyed = self
            .world
            .query::<(hecs::Entity, &IdComponent)>()
            .iter()
            .map(|(entity, id)| (Entity(entity), id.id))
            .collect::<Vec<_>>();
        self.world.clear();
        for (entity, entity_id) in destroyed {
            self.record_mutation(entity, entity_id, SceneMutationKind::EntityDestroyed);
            self.entity_revisions.remove(&entity);
            self.dirty_world.remove(&entity);
        }
    }

    /// Destroys `entity` and its whole subtree.
    ///
    /// Descendants are gathered through the children caches *before* any destroy, since
    /// despawning invalidates handles. The entity is also detached from its parent's
    /// children cache so no live entity holds a dead handle. A stale handle is a no-op.
    pub fn destroy_entity(&mut self, entity: Entity) {
        let mut doomed: Vec<Entity> = Vec::new();
        self.gather_subtree(entity, &mut doomed);

        // Detach from the parent's children cache so it holds no dead handle.
        let parent = self
            .with_component::<Relationship, _>(entity, |rel| rel.parent_handle)
            .unwrap_or(None);
        if let Some(parent) = parent {
            let _ = self.with_component_mut::<Relationship, _>(parent, |rel| {
                rel.children.retain(|&c| c != entity);
            });
        }

        let doomed = doomed
            .into_iter()
            .filter_map(|handle| {
                self.with_component::<IdComponent, _>(handle, |id| id.id)
                    .ok()
                    .map(|id| (handle, id))
            })
            .collect::<Vec<_>>();
        for (handle, entity_id) in doomed {
            if self.world.despawn(handle.0).is_ok() {
                self.record_mutation(handle, entity_id, SceneMutationKind::EntityDestroyed);
                self.entity_revisions.remove(&handle);
                self.dirty_world.remove(&handle);
            }
        }
    }

    /// All entities in `entity`'s subtree (pre-order), including `entity` itself.
    #[must_use]
    pub fn subtree_entities(&self, entity: Entity) -> Vec<Entity> {
        let mut out = Vec::new();
        self.gather_subtree(entity, &mut out);
        out
    }

    /// Appends `entity` and every descendant (via the children caches) to `doomed`,
    /// pre-order, for [`Scene::destroy_entity`].
    fn gather_subtree(&self, entity: Entity, doomed: &mut Vec<Entity>) {
        doomed.push(entity);
        let children = self
            .with_component::<Relationship, _>(entity, |rel| rel.children.clone())
            .unwrap_or_default();
        for child in children {
            self.gather_subtree(child, doomed);
        }
    }

    /// The entity carrying `uuid`, or `None`.
    ///
    /// Cross-scene lookups must go by uuid — ECS handles can coincide between worlds
    /// and alias silently, so the id is the only stable cross-entity reference.
    #[must_use]
    pub fn find_entity_by_uuid(&self, uuid: Uuid) -> Option<Entity> {
        self.world
            .query::<(hecs::Entity, &IdComponent)>()
            .iter()
            .find(|(_, id)| id.id == uuid)
            .map(|(handle, _)| Entity(handle))
    }

    /// The number of live entities in the scene.
    #[must_use]
    pub fn len(&self) -> usize {
        self.world.len() as usize
    }

    /// Whether the scene holds no entities.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }

    /// Identity of this in-memory scene instance.
    ///
    /// Distinguishes journal streams across distinct `Scene` values (the authored scene,
    /// a play duplicate, a preview scene): a derived mirror whose retained cursor came
    /// from a different instance must rebuild from a snapshot instead of reading deltas.
    #[must_use]
    pub const fn instance_id(&self) -> Uuid {
        self.instance
    }

    /// Current mutation cursor for an atomically read scene snapshot.
    #[must_use]
    pub fn journal_cursor(&self) -> SceneJournalCursor {
        SceneJournalCursor::at(self.revision)
    }

    /// Reads every retained mutation after `cursor`, or requests a complete snapshot rebuild.
    #[must_use]
    pub fn read_journal(&self, cursor: SceneJournalCursor) -> SceneJournalRead {
        let revision = cursor.revision();
        let next = self.journal_cursor();
        if revision < self.journal_base || revision > self.revision {
            return SceneJournalRead::SnapshotRequired { next };
        }
        SceneJournalRead::Delta {
            mutations: self
                .journal
                .iter()
                .copied()
                .filter(|mutation| mutation.revision > revision)
                .collect(),
            next,
        }
    }

    /// Latest tracked revisions for a live entity.
    #[must_use]
    pub fn entity_revisions(&self, entity: Entity) -> Option<SceneEntityRevisions> {
        self.entity_revisions
            .get(&entity)
            .map(|state| state.published)
    }

    /// Current and previous composed transforms for a live transformable entity.
    #[must_use]
    pub fn world_transform_state(&self, entity: Entity) -> Option<SceneWorldTransformState> {
        let state = self.entity_revisions.get(&entity)?;
        Some(SceneWorldTransformState {
            current: state.current_world?,
            previous: state.previous_world.unwrap_or(state.current_world?),
            current_revision: state.published.world_transform,
            previous_revision: state.published.previous_world_transform,
        })
    }

    pub(crate) fn world_transform_needs_update(
        &self,
        entity: Entity,
        parent_world_revision: SceneRevision,
    ) -> bool {
        let Some(state) = self.entity_revisions.get(&entity) else {
            return true;
        };
        state.current_world.is_none()
            || !self.has_component::<crate::WorldTransform>(entity)
            || state.resolved_local != state.published.local_transform
            || state.resolved_hierarchy != state.published.hierarchy
            || state.resolved_parent_world != parent_world_revision
    }

    pub(crate) fn publish_world_transform(
        &mut self,
        entity: Entity,
        parent_world_revision: SceneRevision,
        world: Mat4,
    ) -> SceneRevision {
        let changed = self
            .entity_revisions
            .get(&entity)
            .and_then(|state| state.current_world)
            != Some(world);
        if changed {
            if self.has_component::<crate::WorldTransform>(entity) {
                if let Ok(mut value) = self.world.get::<&mut crate::WorldTransform>(entity.0) {
                    value.matrix = world;
                }
            } else {
                let _ = self
                    .world
                    .insert_one(entity.0, crate::WorldTransform { matrix: world });
            }
            let revision = self.record_component_mutation(
                entity,
                TypeId::of::<crate::WorldTransform>(),
                SceneMutationKind::ComponentUpdated(TypeId::of::<crate::WorldTransform>()),
            );
            debug_assert_eq!(
                self.entity_revisions
                    .get(&entity)
                    .map(|state| state.published.world_transform),
                Some(revision)
            );
        }
        let state = self
            .entity_revisions
            .get_mut(&entity)
            .expect("live transformable entity must have revision state");
        state.resolved_local = state.published.local_transform;
        state.resolved_hierarchy = state.published.hierarchy;
        state.resolved_parent_world = parent_world_revision;
        state.published.world_transform
    }

    pub(crate) fn take_world_dirty_entities(&mut self) -> HashSet<Entity> {
        std::mem::take(&mut self.dirty_world)
    }

    fn record_entity_created(&mut self, entity: Entity) {
        let entity_id = self
            .with_component::<IdComponent, _>(entity, |id| id.id)
            .expect("a created scene entity always has an IdComponent");
        let revision = self.record_mutation(entity, entity_id, SceneMutationKind::EntityCreated);
        self.entity_revisions.insert(
            entity,
            EntityRevisionState {
                published: SceneEntityRevisions {
                    created: revision,
                    content: revision,
                    local_transform: revision,
                    hierarchy: revision,
                    ..SceneEntityRevisions::default()
                },
                ..EntityRevisionState::default()
            },
        );
        self.dirty_world.insert(entity);
    }

    fn record_component_mutation(
        &mut self,
        entity: Entity,
        component: TypeId,
        kind: SceneMutationKind,
    ) -> SceneRevision {
        let Some(entity_id) = self
            .with_component::<IdComponent, _>(entity, |id| id.id)
            .ok()
        else {
            return self.revision;
        };
        let world_transform = (component == TypeId::of::<crate::WorldTransform>())
            .then(|| self.component::<crate::WorldTransform>(entity).ok())
            .flatten();
        let revision = self.record_mutation(entity, entity_id, kind);
        let state = self.entity_revisions.entry(entity).or_default();
        state.published.content = revision;
        if component == TypeId::of::<Transform>()
            || component == TypeId::of::<crate::PoseOverride>()
        {
            state.published.local_transform = revision;
            self.dirty_world.insert(entity);
        }
        if component == TypeId::of::<Relationship>() {
            state.published.hierarchy = revision;
            self.dirty_world.insert(entity);
        }
        if component == TypeId::of::<crate::WorldTransform>() {
            if matches!(kind, SceneMutationKind::ComponentRemoved(_)) {
                state.current_world = None;
                state.previous_world = None;
                state.published.world_transform = SceneRevision::ZERO;
                state.published.previous_world_transform = SceneRevision::ZERO;
                self.dirty_world.insert(entity);
            } else if let Some(world) = world_transform {
                state.previous_world = state.current_world;
                state.published.previous_world_transform = state.published.world_transform;
                state.current_world = Some(world.matrix);
                state.published.world_transform = revision;
            }
        }
        revision
    }

    fn record_mutation(
        &mut self,
        entity: Entity,
        entity_id: Uuid,
        kind: SceneMutationKind,
    ) -> SceneRevision {
        self.revision = self.revision.next();
        if self.journal_capacity == 0 {
            self.journal_base = self.revision;
            return self.revision;
        }
        self.journal.push_back(SceneMutation {
            revision: self.revision,
            entity,
            entity_id,
            kind,
        });
        while self.journal.len() > self.journal_capacity {
            if let Some(dropped) = self.journal.pop_front() {
                self.journal_base = dropped.revision;
            }
        }
        self.revision
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{SceneJournalRead, SceneMutationKind};

    #[test]
    fn create_valid_destroy_and_count() {
        let mut scene = Scene::new();
        let entities: Vec<Entity> = (0..5)
            .map(|i| scene.create_entity(format!("e{i}")))
            .collect();

        for &e in &entities {
            assert!(scene.valid(e));
        }
        assert_eq!(scene.len(), 5);
        assert!(!scene.is_empty());

        let mut seen = 0;
        scene.for_each::<&IdComponent, _>(|e, _id| {
            assert!(scene_contains(&entities, e));
            seen += 1;
        });
        assert_eq!(seen, 5);

        let doomed = entities[2];
        scene.destroy_entity(doomed);
        assert!(!scene.valid(doomed));
        assert_eq!(scene.len(), 4);
        for &e in &entities {
            assert_eq!(scene.valid(e), e != doomed);
        }

        scene.destroy_entity(doomed);
        assert_eq!(scene.len(), 4);
    }

    fn scene_contains(entities: &[Entity], e: Entity) -> bool {
        entities.contains(&e)
    }

    #[test]
    fn find_entity_by_uuid_resolves_known_and_misses_absent() {
        let mut scene = Scene::new();
        let a = scene.create_entity("a");
        let b = scene.create_entity("b");

        let id_a = scene.component::<IdComponent>(a).unwrap().id;
        let id_b = scene.component::<IdComponent>(b).unwrap().id;
        assert_ne!(id_a, id_b);

        assert_eq!(scene.find_entity_by_uuid(id_a), Some(a));
        assert_eq!(scene.find_entity_by_uuid(id_b), Some(b));

        let absent = Uuid(7);
        assert!(scene.find_entity_by_uuid(absent).is_none());
    }

    #[test]
    fn stable_identity_rejects_replacement_and_mutable_access() {
        let mut scene = Scene::new();
        let entity = scene.create_entity("stable");
        let original = scene.component::<IdComponent>(entity).unwrap();

        assert!(matches!(
            scene.add_component(entity, IdComponent::new(Uuid(99))),
            Err(crate::Error::ImmutableIdentity)
        ));
        assert!(matches!(
            scene.with_component_mut::<IdComponent, _>(entity, |id| id.id = Uuid(99)),
            Err(crate::Error::ImmutableIdentity)
        ));
        assert_eq!(scene.component::<IdComponent>(entity).unwrap(), original);
    }

    #[test]
    fn plant_origin_is_immutable_once_set() {
        use crate::PlantOrigin;
        let mut scene = Scene::new();
        let entity = scene.create_entity("promoted plant");
        let plant = saffron_spatial::PlantId::explicit([5; 16]).expect("plant id");
        scene
            .add_component(
                entity,
                PlantOrigin {
                    plant,
                    source_generation: 3,
                },
            )
            .unwrap();

        // A second add would re-point the view at another plant.
        assert!(matches!(
            scene.add_component(
                entity,
                PlantOrigin {
                    plant: saffron_spatial::PlantId::explicit([6; 16]).expect("plant id"),
                    source_generation: 3,
                },
            ),
            Err(crate::Error::ImmutableIdentity)
        ));
        assert!(matches!(
            scene.with_component_mut::<PlantOrigin, _>(entity, |origin| origin.plant =
                saffron_spatial::PlantId::explicit([6; 16]).expect("plant id")),
            Err(crate::Error::ImmutableIdentity)
        ));
        assert_eq!(scene.component::<PlantOrigin>(entity).unwrap().plant, plant);
    }

    #[test]
    #[should_panic(expected = "PlantOrigin is immutable")]
    fn plant_origin_cannot_be_removed() {
        let mut scene = Scene::new();
        let entity = scene.create_entity("promoted plant");
        scene.remove_component::<crate::PlantOrigin>(entity);
    }

    #[test]
    #[should_panic(expected = "IdComponent is immutable")]
    fn stable_identity_cannot_be_removed() {
        let mut scene = Scene::new();
        let entity = scene.create_entity("stable");
        scene.remove_component::<IdComponent>(entity);
    }

    #[test]
    #[should_panic(expected = "mutable scene queries cannot borrow IdComponent")]
    fn stable_identity_cannot_enter_a_mutable_query() {
        let mut scene = Scene::new();
        scene.create_entity("stable");
        scene.for_each::<&mut IdComponent, _>(|_, _| {});
    }

    #[test]
    fn component_access_add_has_read_remove() {
        #[derive(Clone, Copy, PartialEq, Debug)]
        struct Health(i32);

        let mut scene = Scene::new();
        let e = scene.create_entity("e");

        assert!(!scene.has_component::<Health>(e));
        scene.add_component(e, Health(42)).unwrap();
        assert!(scene.has_component::<Health>(e));

        assert_eq!(scene.component::<Health>(e).unwrap(), Health(42));
        scene
            .with_component_mut::<Health, _>(e, |h| h.0 += 8)
            .unwrap();
        assert_eq!(scene.component::<Health>(e).unwrap(), Health(50));

        scene.remove_component::<Health>(e);
        assert!(!scene.has_component::<Health>(e));
        assert!(matches!(
            scene.component::<Health>(e),
            Err(crate::Error::MissingComponent)
        ));
    }

    #[test]
    fn add_component_to_stale_handle_errors() {
        let mut scene = Scene::new();
        let e = scene.create_entity("e");
        scene.destroy_entity(e);
        assert!(matches!(
            scene.add_component(e, 7u32),
            Err(crate::Error::InvalidEntity)
        ));
    }

    #[test]
    fn vegetation_field_is_a_scene_singleton() {
        let mut scene = Scene::new();
        let first = scene.create_entity("Vegetation");
        let second = scene.create_entity("Competing vegetation");
        let field = VegetationField {
            map: Uuid(42),
            enabled: true,
        };

        scene.add_component(first, field).unwrap();
        scene.add_component(first, field).unwrap();
        assert!(matches!(
            scene.add_component(second, field),
            Err(crate::Error::SingletonComponent("VegetationField"))
        ));
        assert_eq!(scene.component::<VegetationField>(first).unwrap(), field);
        assert!(!scene.has_component::<VegetationField>(second));
    }

    #[test]
    fn for_each_mutates_through_query() {
        #[derive(Clone, Copy, PartialEq, Debug)]
        struct Counter(u32);

        let mut scene = Scene::new();
        let entities: Vec<Entity> = (0..3).map(|_| scene.create_entity("c")).collect();
        for &e in &entities {
            scene.add_component(e, Counter(0)).unwrap();
        }

        scene.for_each::<&mut Counter, _>(|_, c| c.0 += 1);

        for &e in &entities {
            assert_eq!(scene.component::<Counter>(e).unwrap(), Counter(1));
        }
    }

    #[test]
    fn journal_orders_structural_and_component_mutations() {
        #[derive(Clone, Copy)]
        struct Counter(u32);

        let mut scene = Scene::new();
        let cursor = scene.journal_cursor();
        let entity = scene.create_entity("tracked");
        let entity_id = scene.component::<IdComponent>(entity).unwrap().id;
        scene.add_component(entity, Counter(1)).unwrap();
        scene
            .with_component_mut::<Counter, _>(entity, |counter| counter.0 += 1)
            .unwrap();
        scene.remove_component::<Counter>(entity);
        scene.destroy_entity(entity);

        let SceneJournalRead::Delta { mutations, next } = scene.read_journal(cursor) else {
            panic!("fresh cursor must retain its delta");
        };
        assert_eq!(next.revision().get(), 5);
        assert_eq!(mutations.len(), 5);
        assert!(
            mutations
                .windows(2)
                .all(|pair| pair[0].revision < pair[1].revision)
        );
        assert!(mutations.iter().all(|mutation| mutation.entity == entity));
        assert!(
            mutations
                .iter()
                .all(|mutation| mutation.entity_id == entity_id)
        );
        assert_eq!(mutations[0].kind, SceneMutationKind::EntityCreated);
        assert_eq!(
            mutations[1].kind,
            SceneMutationKind::ComponentAdded(TypeId::of::<Counter>())
        );
        assert_eq!(
            mutations[2].kind,
            SceneMutationKind::ComponentUpdated(TypeId::of::<Counter>())
        );
        assert_eq!(
            mutations[3].kind,
            SceneMutationKind::ComponentRemoved(TypeId::of::<Counter>())
        );
        assert_eq!(mutations[4].kind, SceneMutationKind::EntityDestroyed);
    }

    #[test]
    fn mutable_query_reports_each_entity_and_unique_component_type() {
        #[derive(Clone, Copy)]
        struct Counter(u32);

        let mut scene = Scene::new();
        let first = scene.create_entity("first");
        let second = scene.create_entity("second");
        scene.add_component(first, Counter(0)).unwrap();
        scene.add_component(second, Counter(0)).unwrap();
        let cursor = scene.journal_cursor();

        scene.for_each::<(&mut Counter, &Name), _>(|_, (counter, _)| counter.0 += 1);

        let SceneJournalRead::Delta { mutations, .. } = scene.read_journal(cursor) else {
            panic!("fresh cursor must retain its delta");
        };
        assert_eq!(mutations.len(), 2);
        assert!(mutations.iter().all(|mutation| {
            mutation.kind == SceneMutationKind::ComponentUpdated(TypeId::of::<Counter>())
        }));
    }

    #[test]
    fn bounded_journal_requires_snapshot_after_overflow() {
        let mut scene = Scene::new();
        scene.journal_capacity = 2;
        let stale = scene.journal_cursor();
        scene.create_entity("a");
        scene.create_entity("b");
        scene.create_entity("c");

        assert!(matches!(
            scene.read_journal(stale),
            SceneJournalRead::SnapshotRequired { .. }
        ));
        let current = scene.journal_cursor();
        assert!(matches!(
            scene.read_journal(current),
            SceneJournalRead::Delta { mutations, .. } if mutations.is_empty()
        ));
    }

    #[test]
    fn world_transform_revisions_change_only_for_dirty_results() {
        let mut scene = Scene::new();
        let entity = scene.create_entity("moving");
        scene.update_world_transforms();
        let initial = scene.world_transform_state(entity).unwrap();

        scene.update_world_transforms();
        assert_eq!(scene.world_transform_state(entity).unwrap(), initial);

        scene
            .with_component_mut::<Transform, _>(entity, |transform| {
                transform.translation.x = 4.0;
            })
            .unwrap();
        scene.update_world_transforms();
        let moved = scene.world_transform_state(entity).unwrap();
        assert_eq!(moved.previous, initial.current);
        assert_eq!(moved.previous_revision, initial.current_revision);
        assert_eq!(moved.current.w_axis.x, 4.0);
        assert!(moved.current_revision > initial.current_revision);
    }

    #[test]
    fn parent_world_revision_propagates_once_to_descendants() {
        let mut scene = Scene::new();
        let parent = scene.create_entity("parent");
        let child = scene.create_entity("child");
        scene.set_parent(child, Some(parent), false).unwrap();
        scene.update_world_transforms();
        let initial_child = scene.world_transform_state(child).unwrap();

        scene
            .with_component_mut::<Transform, _>(parent, |transform| {
                transform.translation.y = 3.0;
            })
            .unwrap();
        scene.update_world_transforms();
        let moved_child = scene.world_transform_state(child).unwrap();
        assert_eq!(moved_child.previous, initial_child.current);
        assert_eq!(moved_child.current.w_axis.y, 3.0);

        let cursor = scene.journal_cursor();
        scene.update_world_transforms();
        assert!(matches!(
            scene.read_journal(cursor),
            SceneJournalRead::Delta { mutations, .. } if mutations.is_empty()
        ));
    }
}
