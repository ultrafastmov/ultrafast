//! The anchored crop camera: holds still while the subject stays inside a
//! dead zone, and when it leaves, eases toward it with a speed limit and a
//! bounded acceleration that can always brake without overshooting. Shot
//! cuts and speaker changes snap instead of panning; a stitch join (removed
//! time inside one camera shot) keeps the established framing.

/// -1: chasing left, 0: holding, 1: chasing right.
pub type Mode = i8;

#[derive(Debug, Clone, PartialEq)]
pub struct AnchorState {
    pub face_id: Option<i64>,
    pub mode: Mode,
    pub offset_px: f64,
    pub shot_key: Option<(i64, i64)>,
    pub velocity_px: f64,
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct AnchorProfile {
    pub enter_safe_zone_px: f64,
    pub max_acceleration_px: f64,
    pub max_step_px: f64,
    pub release_safe_zone_px: f64,
}

/// Wider shots get a larger dead zone and a gentler, slower camera.
pub fn anchor_profile(face_height: Option<f64>, visible_faces: usize) -> AnchorProfile {
    let h = face_height.filter(|h| h.is_finite()).map(|h| h.max(0.0)).unwrap_or(0.0);
    let wide = h <= 0.18 || (visible_faces >= 2 && h <= 0.28);
    if wide {
        AnchorProfile {
            enter_safe_zone_px: 180.0,
            release_safe_zone_px: 110.0,
            max_acceleration_px: 0.25,
            max_step_px: 2.0,
        }
    } else if h <= 0.32 {
        AnchorProfile {
            enter_safe_zone_px: 120.0,
            release_safe_zone_px: 72.0,
            max_acceleration_px: 0.5,
            max_step_px: 4.0,
        }
    } else {
        AnchorProfile {
            enter_safe_zone_px: 72.0,
            release_safe_zone_px: 44.0,
            max_acceleration_px: 0.75,
            max_step_px: 6.0,
        }
    }
}

fn js_sign(v: f64) -> f64 {
    if v > 0.0 {
        1.0
    } else if v < 0.0 {
        -1.0
    } else {
        v
    }
}

fn eased(current: f64, velocity: f64, max_acc: f64, max_step: f64, target: f64) -> (f64, f64) {
    let distance = target - current;
    if distance.abs() < 0.01 {
        return (target, 0.0);
    }
    // The fastest speed from which the camera can still brake at max_acc
    // without overshooting.
    let braking = (2.0 * max_acc * distance.abs()).sqrt();
    let target_velocity = js_sign(distance) * max_step.min(braking);
    let v = velocity + (target_velocity - velocity).clamp(-max_acc, max_acc);
    let offset = current + v;
    if js_sign(target - offset) != js_sign(distance) || (target - offset).abs() < 0.01 {
        return (target, 0.0);
    }
    (offset, v)
}

fn decelerate(current: f64, velocity: f64, max_acc: f64) -> (f64, f64) {
    let v = velocity + (-velocity).clamp(-max_acc, max_acc);
    (current + v, v)
}

/// The camera after one frame.
pub fn advance_anchor(
    desired_offset_px: f64,
    face_height: Option<f64>,
    face_id: Option<i64>,
    force_reset: bool,
    previous: Option<&AnchorState>,
    shot_key: Option<(i64, i64)>,
    visible_faces: usize,
    start_boundary_kind: Option<&str>,
) -> AnchorState {
    if !desired_offset_px.is_finite() {
        return previous.cloned().unwrap_or(AnchorState {
            face_id,
            mode: 0,
            offset_px: 0.0,
            shot_key,
            velocity_px: 0.0,
        });
    }
    let face_changed = matches!(previous, Some(p) if face_id.is_some() && p.face_id.is_some() && face_id != p.face_id);
    let shot_changed = previous.is_none_or(|p| p.shot_key != shot_key);
    // A stitch join is a tracking boundary, not necessarily a camera
    // change: keep the incoming shot's framing if the subject is inside its
    // dead zone. Never carry velocity through removed time.
    if start_boundary_kind == Some("stitch_join")
        && (force_reset || shot_changed)
        && let Some(p) = previous
        && p.offset_px.is_finite()
        && p.face_id.is_some()
        && face_id.is_some()
        && let Some(h) = face_height.filter(|h| h.is_finite() && *h > 0.0)
        && (desired_offset_px - p.offset_px).abs() <= anchor_profile(Some(h), visible_faces).enter_safe_zone_px
    {
        return AnchorState {
            face_id,
            mode: 0,
            offset_px: p.offset_px,
            shot_key,
            velocity_px: 0.0,
        };
    }
    if force_reset || shot_changed {
        return AnchorState {
            face_id,
            mode: 0,
            offset_px: desired_offset_px,
            shot_key,
            velocity_px: 0.0,
        };
    }
    let prev = previous.expect("previous is set when the shot did not change");
    if face_changed {
        // A confirmed speaker change is an editorial cut, not camera motion.
        return AnchorState {
            face_id,
            mode: 0,
            offset_px: desired_offset_px,
            shot_key,
            velocity_px: 0.0,
        };
    }
    // Missing detections keep the established composition.
    let Some(h) = face_height.filter(|h| h.is_finite()) else {
        return AnchorState {
            face_id: face_id.or(prev.face_id),
            mode: 0,
            shot_key,
            velocity_px: 0.0,
            ..prev.clone()
        };
    };
    let profile = anchor_profile(Some(h), visible_faces);
    let error = desired_offset_px - prev.offset_px;
    let mut mode = prev.mode;
    if mode == 0 {
        if error > profile.enter_safe_zone_px {
            mode = 1;
        } else if error < -profile.enter_safe_zone_px {
            mode = -1;
        }
    } else if (mode == 1 && error <= profile.release_safe_zone_px)
        || (mode == -1 && error >= -profile.release_safe_zone_px)
    {
        mode = 0;
    }
    if mode == 0 {
        let (offset_px, velocity_px) = decelerate(prev.offset_px, prev.velocity_px, profile.max_acceleration_px);
        return AnchorState {
            face_id: face_id.or(prev.face_id),
            mode,
            offset_px,
            shot_key,
            velocity_px,
        };
    }
    let target = desired_offset_px - mode as f64 * profile.release_safe_zone_px;
    let (offset_px, velocity_px) = eased(
        prev.offset_px,
        prev.velocity_px.clamp(-profile.max_step_px, profile.max_step_px),
        profile.max_acceleration_px,
        profile.max_step_px,
        target,
    );
    AnchorState {
        face_id: face_id.or(prev.face_id),
        mode,
        offset_px,
        shot_key,
        velocity_px,
    }
}
