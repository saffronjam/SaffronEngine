//! The `register_component!` declarative macro: one line per component is the whole registration.
//!
//! Registration order is load-bearing twice over — it is the `componentOrder` canonical order and
//! the OpenRPC/manifest emit order — so
//! [`register_builtin_components`](crate::register_builtin_components) stays an explicit ordered
//! sequence of calls rather than a link-order-defined collection like `inventory`.

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
/// Omitting the serde paths — `register_component!(reg, Type, "Name" [, removable])` — defaults them
/// to the type's [`SceneSerialize`](crate::SceneSerialize) impl, which is the byte-compatible body
/// every built-in carries. The explicit-serde form exists for a type supplying a one-off `to_json` /
/// `from_json`, such as a test stub.
///
/// The closures reference only the serde *paths* and capture nothing, so they coerce to the bare `fn`
/// pointers [`ComponentTraits`](crate::ComponentTraits) holds and the row stays `Copy`. The
/// deserialize trampoline default-constructs the component when absent, then fills it in place.
#[macro_export]
macro_rules! register_component {
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
    ($reg:expr, $ty:ty, $name:literal, $to_json:expr, $from_json:expr $(,)?) => {
        $crate::register_component!($reg, $ty, $name, $to_json, $from_json, true)
    };
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
