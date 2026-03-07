use glam::{Mat4, Quat, Vec3};

/// Maximum number of joints supported per skeleton.
pub const MAX_JOINTS: usize = 128;

/// A single joint in a skeleton hierarchy.
#[derive(Debug, Clone)]
pub struct Joint {
    pub name: String,
    pub parent: Option<usize>,
    pub inverse_bind_matrix: Mat4,
    pub local_transform: JointTransform,
}

/// Decomposed transform for a joint (for interpolation).
#[derive(Debug, Clone, Copy)]
pub struct JointTransform {
    pub translation: Vec3,
    pub rotation: Quat,
    pub scale: Vec3,
}

impl Default for JointTransform {
    fn default() -> Self {
        Self {
            translation: Vec3::ZERO,
            rotation: Quat::IDENTITY,
            scale: Vec3::ONE,
        }
    }
}

impl JointTransform {
    pub fn to_mat4(&self) -> Mat4 {
        Mat4::from_scale_rotation_translation(self.scale, self.rotation, self.translation)
    }

    /// Linearly interpolate between two transforms.
    pub fn lerp(&self, other: &Self, t: f32) -> Self {
        Self {
            translation: self.translation.lerp(other.translation, t),
            rotation: self.rotation.slerp(other.rotation, t),
            scale: self.scale.lerp(other.scale, t),
        }
    }
}

/// A skeleton: a hierarchy of joints with inverse bind matrices.
#[derive(Debug, Clone)]
pub struct Skeleton {
    pub joints: Vec<Joint>,
    /// Maps joint name to index for quick lookup.
    pub name_to_index: std::collections::HashMap<String, usize>,
}

impl Skeleton {
    pub fn new(joints: Vec<Joint>) -> Self {
        let name_to_index = joints
            .iter()
            .enumerate()
            .map(|(i, j)| (j.name.clone(), i))
            .collect();
        Self {
            joints,
            name_to_index,
        }
    }

    /// Compute world-space joint matrices from local transforms.
    /// Returns the final skinning matrices (world * inverse_bind).
    pub fn compute_skin_matrices(&self, local_transforms: &[JointTransform]) -> Vec<Mat4> {
        let count = self.joints.len().min(MAX_JOINTS);
        let mut world_transforms = vec![Mat4::IDENTITY; count];
        let mut skin_matrices = vec![Mat4::IDENTITY; count];

        for i in 0..count {
            let local = if i < local_transforms.len() {
                local_transforms[i].to_mat4()
            } else {
                self.joints[i].local_transform.to_mat4()
            };

            world_transforms[i] = match self.joints[i].parent {
                Some(parent) if parent < i => world_transforms[parent] * local,
                _ => local,
            };

            skin_matrices[i] = world_transforms[i] * self.joints[i].inverse_bind_matrix;
        }

        skin_matrices
    }
}

/// Interpolation method for animation keyframes.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum Interpolation {
    Step,
    Linear,
    CubicSpline,
}

/// Which property a channel animates.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum ChannelProperty {
    Translation,
    Rotation,
    Scale,
}

/// A single animation channel targeting one joint's property.
#[derive(Debug, Clone)]
pub struct AnimationChannel {
    pub joint_index: usize,
    pub property: ChannelProperty,
    pub interpolation: Interpolation,
    /// Keyframe timestamps in seconds.
    pub timestamps: Vec<f32>,
    /// Keyframe values: Vec3 for translation/scale, Vec4 (as Quat) for rotation.
    pub values: ChannelValues,
}

/// Typed keyframe values.
#[derive(Debug, Clone)]
pub enum ChannelValues {
    Vec3(Vec<Vec3>),
    Quat(Vec<Quat>),
}

impl AnimationChannel {
    /// Sample this channel at a given time, writing into the joint transform.
    pub fn sample(&self, time: f32, transform: &mut JointTransform) {
        if self.timestamps.is_empty() {
            return;
        }

        let duration = *self.timestamps.last().unwrap();
        let t = if duration > 0.0 {
            time % duration
        } else {
            0.0
        };

        // Find the two keyframes to interpolate between
        let (i0, i1, factor) = self.find_keyframes(t);

        match (&self.values, self.property) {
            (ChannelValues::Vec3(vals), ChannelProperty::Translation) => {
                transform.translation = self.interpolate_vec3(vals, i0, i1, factor);
            }
            (ChannelValues::Vec3(vals), ChannelProperty::Scale) => {
                transform.scale = self.interpolate_vec3(vals, i0, i1, factor);
            }
            (ChannelValues::Quat(vals), ChannelProperty::Rotation) => {
                transform.rotation = self.interpolate_quat(vals, i0, i1, factor);
            }
            _ => {}
        }
    }

    fn find_keyframes(&self, t: f32) -> (usize, usize, f32) {
        if self.timestamps.len() == 1 {
            return (0, 0, 0.0);
        }

        for i in 0..self.timestamps.len() - 1 {
            if t < self.timestamps[i + 1] {
                let seg_duration = self.timestamps[i + 1] - self.timestamps[i];
                let factor = if seg_duration > 0.0 {
                    (t - self.timestamps[i]) / seg_duration
                } else {
                    0.0
                };
                return (i, i + 1, factor);
            }
        }

        let last = self.timestamps.len() - 1;
        (last, last, 0.0)
    }

    fn interpolate_vec3(&self, vals: &[Vec3], i0: usize, i1: usize, factor: f32) -> Vec3 {
        match self.interpolation {
            Interpolation::Step => vals[i0],
            Interpolation::Linear | Interpolation::CubicSpline => vals[i0].lerp(vals[i1], factor),
        }
    }

    fn interpolate_quat(&self, vals: &[Quat], i0: usize, i1: usize, factor: f32) -> Quat {
        match self.interpolation {
            Interpolation::Step => vals[i0],
            Interpolation::Linear | Interpolation::CubicSpline => {
                vals[i0].slerp(vals[i1], factor)
            }
        }
    }
}

/// A named animation clip containing multiple channels.
#[derive(Debug, Clone)]
pub struct AnimationClip {
    pub name: String,
    pub duration: f32,
    pub channels: Vec<AnimationChannel>,
}

impl AnimationClip {
    /// Sample all channels at the given time into a set of joint transforms.
    pub fn sample(&self, time: f32, transforms: &mut [JointTransform]) {
        for channel in &self.channels {
            if channel.joint_index < transforms.len() {
                channel.sample(time, &mut transforms[channel.joint_index]);
            }
        }
    }
}

/// Animation state machine states.
#[derive(Debug, Clone, PartialEq)]
pub enum AnimState {
    Idle,
    Walk,
    Run,
    Attack,
    Custom(String),
}

impl AnimState {
    pub fn from_str(s: &str) -> Self {
        match s {
            "idle" => Self::Idle,
            "walk" => Self::Walk,
            "run" => Self::Run,
            "attack" => Self::Attack,
            other => Self::Custom(other.to_string()),
        }
    }

    pub fn clip_name(&self) -> &str {
        match self {
            Self::Idle => "idle",
            Self::Walk => "walk",
            Self::Run => "run",
            Self::Attack => "attack",
            Self::Custom(name) => name,
        }
    }
}

/// Runtime animation controller for an entity.
#[derive(Debug, Clone)]
pub struct AnimationController {
    pub current_state: AnimState,
    pub current_time: f32,
    pub speed: f32,
    pub looping: bool,
    /// Index into the skeleton's clip list (resolved at runtime).
    pub active_clip_index: Option<usize>,
}

impl Default for AnimationController {
    fn default() -> Self {
        Self {
            current_state: AnimState::Idle,
            current_time: 0.0,
            speed: 1.0,
            looping: true,
            active_clip_index: None,
        }
    }
}

impl AnimationController {
    pub fn play(&mut self, state: AnimState) {
        if self.current_state != state {
            self.current_state = state;
            self.current_time = 0.0;
            self.active_clip_index = None; // Will be resolved next tick
        }
    }

    pub fn stop(&mut self) {
        self.current_time = 0.0;
        self.active_clip_index = None;
    }

    /// Advance the animation timer. Returns the current playback time.
    pub fn tick(&mut self, dt: f32, clip_duration: f32) -> f32 {
        self.current_time += dt * self.speed;
        if self.looping && clip_duration > 0.0 {
            self.current_time %= clip_duration;
        } else if self.current_time > clip_duration {
            self.current_time = clip_duration;
        }
        self.current_time
    }
}
