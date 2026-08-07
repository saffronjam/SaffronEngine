//! The starter scene every fresh project begins with: a framed camera and a sun.

use glam::{Mat3, Quat, Vec3};

use crate::component::{Camera, DirectionalLight, Transform};
use crate::scene::{Entity, Scene};

/// Seeds a fresh scene with its starter content — a "Camera" framed on the origin and a
/// "Sun" [`DirectionalLight`] — and returns the camera entity.
///
/// Both are ordinary, editable, deletable entities. This is the single definition of "what
/// a new scene contains", shared by the editor's boot scene and every freshly created
/// project, so the two never disagree. Deleting the Sun leaves the scene with no direct sun.
pub fn seed_starter_scene(scene: &mut Scene) -> Entity {
    let camera = scene.create_entity("Camera");
    let _ = scene.add_component(camera, Camera::default());
    let translation = Vec3::new(3.0, 2.5, 4.0);
    let rotation = euler_angles(quat_look_at(-translation.normalize(), Vec3::Y));
    let _ = scene.with_component_mut::<Transform, _>(camera, |t| {
        t.translation = translation;
        t.rotation = rotation;
    });

    let sun = scene.create_entity("Sun");
    let _ = scene.add_component(sun, DirectionalLight::default());

    camera
}

/// A quaternion looking in `direction` with the given `up`, in the right-handed
/// convention where the camera's forward maps to `-Z`.
fn quat_look_at(direction: Vec3, up: Vec3) -> Quat {
    let z = -direction;
    let x = up.cross(z).normalize();
    let y = z.cross(x);
    Quat::from_mat3(&Mat3::from_cols(x, y, z))
}

/// The Euler-XYZ angles of `q`.
///
/// The inverse of [`crate::quat_from_euler_xyz`] up to the gimbal pole, so feeding the
/// result back through the engine's `Transform` rotation rebuilds `q`.
fn euler_angles(q: Quat) -> Vec3 {
    let (x, y, z, w) = (q.x, q.y, q.z, q.w);
    let pitch = (2.0 * (y * z + w * x)).atan2(w * w - x * x - y * y + z * z);
    let yaw = f32::asin((-2.0 * (x * z - w * y)).clamp(-1.0, 1.0));
    let roll = (2.0 * (x * y + w * z)).atan2(w * w + x * x - y * y - z * z);
    Vec3::new(pitch, yaw, roll)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::quat_from_euler_xyz;

    #[test]
    fn seed_creates_a_framed_camera_and_a_sun() {
        let mut scene = Scene::new();
        let camera = seed_starter_scene(&mut scene);

        let mut cameras = Vec::new();
        scene.for_each::<&Camera, _>(|e, _| cameras.push(e));
        let mut suns = Vec::new();
        scene.for_each::<&DirectionalLight, _>(|e, _| suns.push(e));
        assert_eq!(cameras, vec![camera]);
        assert_eq!(suns.len(), 1);
        assert_ne!(suns[0], camera);

        let transform = scene.component::<Transform>(camera).unwrap();
        assert_eq!(transform.translation, Vec3::new(3.0, 2.5, 4.0));
        let forward = (quat_from_euler_xyz(transform.rotation) * Vec3::NEG_Z).normalize();
        let to_origin = (-transform.translation).normalize();
        assert!(forward.dot(to_origin) > 0.999, "camera faces the origin");
    }
}
