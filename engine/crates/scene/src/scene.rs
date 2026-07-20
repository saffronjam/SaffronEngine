//! The wrapped ECS world, the `Entity` handle, and the `Scene` access surface.
//!
//! The ECS crate behind the world (`hecs` by default) is an *internal* detail: every
//! downstream crate goes through [`Scene`] and [`Entity`], never through `hecs::`
//! directly. That is what keeps the fallback to `bevy_ecs` a one-crate change. `Entity`
//! is a bare handle, but every consumer goes through the `Scene` methods.

use std::any::TypeId;
use std::sync::Arc;

use saffron_core::Uuid;

use crate::component::{
    ComponentOrder, IdComponent, Name, Relationship, Transform, VegetationField,
};
use crate::environment::{AssetCatalog, SceneEnvironment};

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
}

/// The scene: the ECS world, the environment, and a borrowed asset catalog.
///
/// `catalog` is an `Option<Arc<AssetCatalog>>` (a read-shared handle the asset layer
/// hands the scene). It is never serialized.
#[derive(Default)]
pub struct Scene {
    world: hecs::World,
    /// Scene-wide environment state (sky, ambient, atmosphere).
    pub environment: SceneEnvironment,
    /// The borrowed, read-shared asset catalog; never serialized.
    pub catalog: Option<Arc<AssetCatalog>>,
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
    /// [`Error::InvalidEntity`](crate::Error::InvalidEntity) if `entity` is stale.
    pub fn add_component<C: Component>(&mut self, entity: Entity, c: C) -> crate::Result<()> {
        if TypeId::of::<C>() == TypeId::of::<VegetationField>()
            && self
                .world
                .query::<(hecs::Entity, &VegetationField)>()
                .iter()
                .any(|(owner, _)| owner != entity.0)
        {
            return Err(crate::Error::SingletonComponent("VegetationField"));
        }
        self.world
            .insert_one(entity.0, c)
            .map_err(|_| crate::Error::InvalidEntity)
    }

    /// Whether `entity` carries a component of type `C`. A stale handle reports `false`.
    #[must_use]
    pub fn has_component<C: Component>(&self, entity: Entity) -> bool {
        self.world.satisfies::<&C>(entity.0)
    }

    /// Removes the component of type `C` from `entity` if present. A missing component or
    /// a stale handle is a no-op.
    pub fn remove_component<C: Component>(&mut self, entity: Entity) {
        let _ = self.world.remove_one::<C>(entity.0);
    }

    /// Runs `f` with a shared reference to `entity`'s component of type `C`, returning
    /// its result. Scoped to a borrow rather than handing out a long-lived reference into
    /// the ECS storage.
    ///
    /// # Errors
    ///
    /// [`Error::MissingComponent`](crate::Error::MissingComponent) if `entity` does
    /// not carry a `C` (also covers a stale handle).
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
        let mut guard = self
            .world
            .get::<&mut C>(entity.0)
            .map_err(|_| crate::Error::MissingComponent)?;
        Ok(f(&mut guard))
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

    /// Iterates every entity carrying the query components, invoking `f` with the
    /// entity handle and its component references.
    ///
    /// `Q` is a tuple of component references — `(&Transform,)`, `(&Transform, &mut
    /// Camera)` — so the callback receives `(Entity, &C…)` or `(Entity, &mut C…)`
    /// exactly as the query tuple spells. Iteration order is unspecified; roots-first
    /// ordering comes from the hierarchy walk, not the view.
    pub fn for_each<Q, F>(&mut self, mut f: F)
    where
        Q: Query,
        F: for<'a> FnMut(Entity, <Q as Query>::Item<'a>),
    {
        for (handle, item) in self.world.query_mut::<(hecs::Entity, Q)>() {
            f(Entity(handle), item);
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
        Entity(handle)
    }

    /// Creates a bare entity carrying only an [`IdComponent`] for the given uuid.
    ///
    /// Unlike [`Scene::create_entity`], the id is *preserved*, not minted, and none of the
    /// authored seed components (`Name` / `Transform` / `Relationship` / `ComponentOrder`)
    /// are added — the scene loader fills them from the document. Used only by
    /// [`Scene::scene_from_json`](crate::Scene); the relink pass defaults a root
    /// [`Relationship`] onto any entity the document left without one.
    pub fn spawn_with_id(&mut self, id: Uuid) -> Entity {
        Entity(self.world.spawn((IdComponent::new(id),)))
    }

    /// Removes every entity from the scene.
    ///
    /// Leaves the environment and the catalog handle untouched; only the ECS world is
    /// emptied. Used by the scene loader before repopulating from a document.
    pub fn clear(&mut self) {
        self.world.clear();
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

        for handle in doomed {
            let _ = self.world.despawn(handle.0);
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
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn create_valid_destroy_and_count() {
        let mut scene = Scene::new();
        let entities: Vec<Entity> = (0..5)
            .map(|i| scene.create_entity(format!("e{i}")))
            .collect();

        // Every freshly created handle is valid.
        for &e in &entities {
            assert!(scene.valid(e));
        }
        assert_eq!(scene.len(), 5);
        assert!(!scene.is_empty());

        // for_each over the seeded IdComponent counts exactly the created entities.
        let mut seen = 0;
        scene.for_each::<&IdComponent, _>(|e, _id| {
            assert!(scene_contains(&entities, e));
            seen += 1;
        });
        assert_eq!(seen, 5);

        // Destroying one removes exactly that entity; the rest stay valid.
        let doomed = entities[2];
        scene.destroy_entity(doomed);
        assert!(!scene.valid(doomed));
        assert_eq!(scene.len(), 4);
        for &e in &entities {
            assert_eq!(scene.valid(e), e != doomed);
        }

        // Destroying a stale handle is a harmless no-op.
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

        // An id no live entity carries resolves to None, not a dangling handle.
        let absent = Uuid(7);
        assert!(scene.find_entity_by_uuid(absent).is_none());
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
        // Reading an absent component is a typed miss, not a panic.
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
}
