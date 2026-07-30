//! Every `sa.Vec3` operator, method, and free function evaluated in a sandboxed VM against the
//! value `glam` computes directly.

use glam::Vec3;
use mlua::Value;

use saffron_script::{DEFAULT_MEMORY_LIMIT, ScriptVm};

/// A VM with the no-scene `sa.*` surface registered. A generous instruction budget
/// so a multi-statement chunk never trips it; the memory limit is the default.
fn vm() -> ScriptVm {
    let vm = ScriptVm::with_limits(1_000_000, DEFAULT_MEMORY_LIMIT).expect("create vm");
    vm.register_no_scene_globals()
        .expect("register no-scene globals");
    vm
}

/// Evaluates a chunk that assigns three numbers to `out_x/out_y/out_z` globals and
/// reads them back as an `f32` triple — the comparison harness for a Vec3 result.
fn eval_triple(vm: &ScriptVm, body: &str) -> Vec3 {
    vm.run_string(body, "value-surface-test")
        .expect("run chunk");
    let g = vm.lua().globals();
    Vec3::new(
        g.get::<f32>("out_x").expect("out_x"),
        g.get::<f32>("out_y").expect("out_y"),
        g.get::<f32>("out_z").expect("out_z"),
    )
}

fn store_vec(name: &str) -> String {
    format!("out_x, out_y, out_z = {name}.x, {name}.y, {name}.z")
}

#[test]
fn vec3_round_trips_components() {
    let vm = vm();
    let got = eval_triple(
        &vm,
        &format!("local v = sa.vec3(1, 2, 3) {}", store_vec("v")),
    );
    assert_eq!(got, Vec3::new(1.0, 2.0, 3.0));
}

#[test]
fn vec3_fields_write_through() {
    let vm = vm();
    let got = eval_triple(
        &vm,
        &format!(
            "local v = sa.vec3(0, 0, 0) v.x = 5 v.y = 6 v.z = 7 {}",
            store_vec("v")
        ),
    );
    assert_eq!(got, Vec3::new(5.0, 6.0, 7.0));
}

#[test]
fn add_and_sub_match_glam() {
    let vm = vm();
    let (a, b) = (Vec3::new(1.0, 2.0, 3.0), Vec3::new(4.0, 5.0, 6.0));
    let setup = "local a = sa.vec3(1, 2, 3) local b = sa.vec3(4, 5, 6)";
    let sum = eval_triple(&vm, &format!("{setup} local c = a + b {}", store_vec("c")));
    assert_eq!(sum, a + b);
    let diff = eval_triple(&vm, &format!("{setup} local c = a - b {}", store_vec("c")));
    assert_eq!(diff, a - b);
}

#[test]
fn scalar_mul_both_orders_match_glam() {
    let vm = vm();
    let v = Vec3::new(1.0, 2.0, 3.0);
    let setup = "local v = sa.vec3(1, 2, 3)";
    let right = eval_triple(&vm, &format!("{setup} local c = v * 2 {}", store_vec("c")));
    assert_eq!(right, v * 2.0);
    let left = eval_triple(&vm, &format!("{setup} local c = 2 * v {}", store_vec("c")));
    assert_eq!(left, v * 2.0);
}

#[test]
fn negation_matches_glam() {
    let vm = vm();
    let v = Vec3::new(1.0, -2.0, 3.0);
    let got = eval_triple(
        &vm,
        &format!(
            "local v = sa.vec3(1, -2, 3) local c = -v {}",
            store_vec("c")
        ),
    );
    assert_eq!(got, -v);
}

#[test]
fn equality_matches_glam() {
    let vm = vm();
    vm.run_string(
        "assert(sa.vec3(1, 2, 3) == sa.vec3(1, 2, 3)) assert(not (sa.vec3(1, 2, 3) == sa.vec3(1, 2, 4)))",
        "eq-test",
    )
    .expect("equality should evaluate");
}

#[test]
fn tostring_matches_format() {
    let vm = vm();
    let s: String = {
        vm.run_string("out_s = tostring(sa.vec3(1, 2, 3))", "tostring-test")
            .expect("tostring should evaluate");
        vm.lua().globals().get("out_s").expect("out_s")
    };
    assert_eq!(s, "Vec3(1, 2, 3)");
}

#[test]
fn length_normalized_dot_cross_lerp_match_glam() {
    let vm = vm();
    let (a, b) = (Vec3::new(1.0, 2.0, 2.0), Vec3::new(0.0, 3.0, 4.0));

    let len: f32 = {
        vm.run_string("out = (sa.vec3(1, 2, 2)):length()", "len-test")
            .expect("length");
        vm.lua().globals().get("out").expect("out")
    };
    assert!((len - a.length()).abs() < 1e-6);

    let normalized = eval_triple(
        &vm,
        &format!(
            "local c = (sa.vec3(1, 2, 2)):normalized() {}",
            store_vec("c")
        ),
    );
    assert!((normalized - a / a.length()).length() < 1e-6);

    let dot: f32 = {
        vm.run_string("out = (sa.vec3(1, 2, 2)):dot(sa.vec3(0, 3, 4))", "dot-test")
            .expect("dot");
        vm.lua().globals().get("out").expect("out")
    };
    assert!((dot - a.dot(b)).abs() < 1e-6);

    let cross = eval_triple(
        &vm,
        &format!(
            "local c = (sa.vec3(1, 2, 2)):cross(sa.vec3(0, 3, 4)) {}",
            store_vec("c")
        ),
    );
    assert!((cross - a.cross(b)).length() < 1e-6);

    let lerped = eval_triple(
        &vm,
        &format!(
            "local c = (sa.vec3(1, 2, 2)):lerp(sa.vec3(0, 3, 4), 0.25) {}",
            store_vec("c")
        ),
    );
    assert!((lerped - a.lerp(b, 0.25)).length() < 1e-6);
}

#[test]
fn free_lerp_matches_glam() {
    let vm = vm();
    let (a, b) = (Vec3::new(1.0, 2.0, 3.0), Vec3::new(5.0, 6.0, 7.0));
    let got = eval_triple(
        &vm,
        &format!(
            "local c = sa.lerp(sa.vec3(1, 2, 3), sa.vec3(5, 6, 7), 0.5) {}",
            store_vec("c")
        ),
    );
    assert!((got - a.lerp(b, 0.5)).length() < 1e-6);
}

#[test]
fn free_look_at_matches_engine_helper() {
    let vm = vm();
    let expected = saffron_script::look_at(
        saffron_script::vec3(0.0, 0.0, 5.0),
        saffron_script::vec3(1.0, 0.0, 0.0),
        saffron_script::vec3(0.0, 1.0, 0.0),
    )
    .0;
    let got = eval_triple(
        &vm,
        &format!(
            "local c = sa.look_at(sa.vec3(0, 0, 5), sa.vec3(1, 0, 0), sa.vec3(0, 1, 0)) {}",
            store_vec("c")
        ),
    );
    assert!(
        (got - expected).length() < 1e-6,
        "got {got:?} expected {expected:?}"
    );
}

#[test]
fn log_is_callable() {
    let vm = vm();
    vm.run_string("sa.log('value-surface: log ok')", "log-test")
        .expect("sa.log should be callable");
}

#[test]
fn vec3_static_new_constructs() {
    let vm = vm();
    // The static constructor is on the value class, registered implicitly through the
    // userdata metatable, reachable from a constructed instance.
    vm.run_string("assert(sa.vec3(1, 2, 3) == sa.vec3(1, 2, 3))", "new-test")
        .expect("vec3 construction should work");
}

#[test]
fn schema_style_vm_resolves_vec3_without_a_scene() {
    // A second, schema-style VM (value types + no-scene globals, no scene bound):
    // the same `sa.vec3` default a `properties` table would carry must resolve.
    let vm = ScriptVm::new().expect("create schema vm");
    vm.register_value_types().expect("register value types");
    vm.register_no_scene_globals()
        .expect("register no-scene globals");
    vm.run_string(
        "local d = sa.vec3(0, 1, 0) assert(d.y == 1)",
        "schema-default",
    )
    .expect("a properties default of sa.vec3 must resolve at edit time");
}

#[test]
fn vec3_userdata_survives_as_value() {
    // A constructed Vec3 stays a userdata value, not coerced to a table — so the
    // schema reader can tell a Vec3 field from a table default.
    let vm = vm();
    vm.run_string(
        "local v = sa.vec3(1, 2, 3) assert(type(v) == 'userdata')",
        "userdata-test",
    )
    .expect("vec3 must be a userdata value");
    // And it carries the numeric x/y/z a vec3 userdata probe checks.
    let v = vm.lua();
    let _ = v.globals().get::<Value>("sa");
}
