//! The `register_component!` declarative macro: the one-line component registration
//! surface.
//!
//! This macro makes **one line per component the entire registration**: it
//! expands `register_component!(reg, C, "Name", to_json, from_json [, removable])` into the
//! [`ComponentRegistry::register`](crate::ComponentRegistry::register) call that builds the
//! fn-pointer [`ComponentTraits`](crate::ComponentTraits) row, with the serde supplied at
//! the call site.
//!
//! A **declarative macro over an explicit ordered list** — not `inventory`. Registration
//! order is load-bearing twice over (it is the `componentOrder` canonical order and the
//! OpenRPC/manifest emit order), and `inventory`'s collection order is link-order-defined
//! and not stable across builds. So
//! [`register_builtin_components`](crate::register_builtin_components) stays an explicit
//! ordered sequence of `register_component!` calls in one function — one place, deterministic
//! order. The macro removes the closure boilerplate; it does not hide the order.

/// Registers a component type into a [`ComponentRegistry`](crate::ComponentRegistry) in one
/// line, building the serialize/deserialize trampolines from the supplied serde paths.
///
/// The full form is `register_component!(reg, Type, "Name", to_json, from_json [, removable])`:
///
/// - `reg` — the `&mut ComponentRegistry` to register into.
/// - `Type` — the component struct (must be `Component + Default + Clone`).
/// - `"Name"` — the stable JSON key and UI header (a `&'static str`).
/// - `to_json` — a path to `fn(&Type) -> serde_json::Value` (e.g.
///   `<Type as SceneSerialize>::to_json`).
/// - `from_json` — a path to `fn(&mut Type, &Value) -> crate::Result<()>` (e.g.
///   `<Type as SceneSerialize>::load_json`).
/// - `removable` — optional `bool` (defaults to `true`); the durable `Name` / `Transform` /
///   `Relationship` rows pass `false`.
///
/// When the serde paths are omitted — `register_component!(reg, Type, "Name" [, removable])` —
/// they default to the type's [`SceneSerialize`](crate::SceneSerialize) impl
/// (`<Type as SceneSerialize>::to_json` / `::load_json`), which is the byte-compatible body
/// every built-in component carries. This is the form
/// [`register_builtin_components`](crate::register_builtin_components) uses; the explicit-serde
/// form exists for a type that supplies a one-off `to_json` / `from_json` (e.g. a test stub).
/// Both forms expand to the same single
/// [`ComponentRegistry::register`](crate::ComponentRegistry::register) call — there is one
/// registration mechanism, the serde paths are just defaulted like `removable`.
///
/// The serialize/deserialize closures reference only the serde *paths* (they capture
/// nothing), so they coerce to the bare `fn` pointers [`ComponentTraits`](crate::ComponentTraits)
/// holds — the row stays `Copy`. The deserialize trampoline default-constructs the component
/// when absent, then fills it in place.
#[macro_export]
macro_rules! register_component {
    // Serde defaulted to the SceneSerialize impl, removable defaulted to true.
    ($reg:expr, $ty:ty, $name:literal $(,)?) => {
        $crate::register_component!(
            $reg,
            $ty,
            $name,
            <$ty as $crate::SceneSerialize>::to_json,
            <$ty as $crate::SceneSerialize>::load_json,
            true
        )
    };
    // Serde defaulted to the SceneSerialize impl, removable explicit.
    ($reg:expr, $ty:ty, $name:literal, $removable:literal $(,)?) => {
        $crate::register_component!(
            $reg,
            $ty,
            $name,
            <$ty as $crate::SceneSerialize>::to_json,
            <$ty as $crate::SceneSerialize>::load_json,
            $removable
        )
    };
    // Explicit serde, removable defaulted to true.
    ($reg:expr, $ty:ty, $name:literal, $to_json:expr, $from_json:expr $(,)?) => {
        $crate::register_component!($reg, $ty, $name, $to_json, $from_json, true)
    };
    // Explicit serde, explicit removable — the canonical expansion.
    ($reg:expr, $ty:ty, $name:literal, $to_json:expr, $from_json:expr, $removable:expr $(,)?) => {
        $reg.register::<$ty>(
            $name,
            $removable,
            |scene: &$crate::Scene, entity: $crate::Entity| -> ::serde_json::Value {
                let to_json: fn(&$ty) -> ::serde_json::Value = $to_json;
                scene
                    .with_component::<$ty, _>(entity, to_json)
                    .unwrap_or(::serde_json::Value::Null)
            },
            |scene: &mut $crate::Scene,
             entity: $crate::Entity,
             value: &::serde_json::Value|
             -> $crate::Result<()> {
                let from_json: fn(&mut $ty, &::serde_json::Value) -> $crate::Result<()> =
                    $from_json;
                if !scene.has_component::<$ty>(entity) {
                    scene.add_component(entity, <$ty as ::core::default::Default>::default())?;
                }
                scene.with_component_mut::<$ty, _>(entity, |c| from_json(c, value))?
            },
        );
    };
}
