//! GPU-free Gaussian splat decoding in the standard 3DGS PLY format.

use std::io::Read;
use std::path::Path;

use ply_rs::parser::Parser;
use ply_rs::ply::{
    DefaultElement, ElementDef, Encoding, Property, PropertyAccess, PropertyType, ScalarType,
};

use super::AssetError;

/// A decoded Gaussian, with scale/opacity/color activations already applied.
#[derive(Debug, Clone, PartialEq)]
pub struct SplatPoint {
    pub position: [f32; 3],
    pub opacity: f32,
    pub scale: [f32; 3],
    /// Quaternion in 3DGS order: w, x, y, z.
    pub rotation: [f32; 4],
    /// Base colour, `0.5 + C0 * f_dc`. Not clamped: the renderer adds the
    /// higher spherical-harmonics bands first, then clamps. An sRGB display
    /// value, as 3DGS trains it against photographs; the renderer converts
    /// it to linear light.
    pub sh_dc: [f32; 3],
}

#[derive(Debug, Clone, PartialEq)]
pub struct SplatData {
    pub splats: Vec<SplatPoint>,
    /// Spherical-harmonics degree of `sh_rest`, 0 to [`MAX_SH_DEGREE`].
    pub sh_degree: u32,
    /// Higher-band SH coefficients, `3 * sh_rest_coefficients(sh_degree)`
    /// per splat in splat order: coefficient by coefficient, each (r, g, b).
    pub sh_rest: Vec<f32>,
}

/// The highest spherical-harmonics degree 3DGS stores.
pub const MAX_SH_DEGREE: u32 = 3;

/// Higher-band SH coefficients per colour channel at `degree` (the bands
/// above the base colour): 0, 3, 8 or 15.
pub fn sh_rest_coefficients(degree: u32) -> usize {
    ((degree + 1) * (degree + 1) - 1) as usize
}

/// Load a confined PLY asset. Only the explicit `procedural` source selects the
/// engine galaxy; absent or invalid files never substitute generated geometry.
pub fn load_splat(project_root: &Path, splat_path: &str) -> Result<SplatData, AssetError> {
    if splat_path == "procedural" {
        return Ok(create_procedural_splats());
    }
    let full_path = crate::project_path::resolve(project_root, splat_path).map_err(AssetError)?;
    let file = std::fs::File::open(&full_path)
        .map_err(|error| AssetError(format!("Failed to read splat '{splat_path}': {error}")))?;
    let mut reader = std::io::BufReader::new(file);
    let invalid =
        |error: std::io::Error| AssetError(format!("Invalid PLY splat '{splat_path}': {error}"));
    let header = Parser::<DefaultElement>::new()
        .read_header(&mut reader)
        .map_err(invalid)?;

    let mut decoded = None;
    for (name, element) in &header.elements {
        if name == "vertex" {
            let sh_degree = vertex_layout(element, splat_path)
                .map_err(|error| AssetError(format!("PLY splat '{splat_path}': {error}")))?;
            let mut decoder = Decoder::new(splat_path, sh_degree, element.count);
            if header.encoding == Encoding::BinaryLittleEndian && all_f32(element) {
                read_f32_rows(&mut reader, element, |row| decoder.push(row)).map_err(|error| {
                    match error {
                        RowError::Io(error) => invalid(error),
                        RowError::Decode(error) => error,
                    }
                })?;
            } else {
                // Each vertex is read straight into a fixed-size row. A map
                // per vertex took about 8 GB for a million degree-3 splats.
                let rows = Parser::<RawVertex>::new()
                    .read_payload_for_element(&mut reader, element, &header)
                    .map_err(invalid)?;
                for row in &rows {
                    decoder.push(row)?;
                }
            }
            decoded = Some(decoder.finish(element)?);
            break;
        }
        // Elements before the vertices still have to be read past.
        Parser::<DefaultElement>::new()
            .read_payload_for_element(&mut reader, element, &header)
            .map_err(invalid)?;
    }
    decoded
        .filter(|data| !data.splats.is_empty())
        .ok_or_else(|| AssetError(format!("PLY splat '{splat_path}' contains no vertices")))
}

/// A cloud this large whose opacities all lie in [0, 1] and whose scales
/// are all non-negative stores them as they are, not as logits and logs.
/// Trainers store logits and logs, so in their files many opacities are
/// negative and nearly every scale is, and read that way such a file would
/// make every splat between 50% and 73% opaque and at least a unit wide.
/// Some exporters write the values already activated.
pub const LINEAR_CHECK_MIN_SPLATS: usize = 1000;

/// Turns raw vertex rows into splats as they are read.
struct Decoder<'a> {
    splat_path: &'a str,
    coefficients: usize,
    splats: Vec<SplatPoint>,
    sh_rest: Vec<f32>,
    /// Whether every opacity so far is in [0, 1] and every scale is
    /// non-negative, as stored.
    stored_linear: bool,
}

impl<'a> Decoder<'a> {
    fn new(splat_path: &'a str, sh_degree: u32, count: usize) -> Self {
        let coefficients = sh_rest_coefficients(sh_degree);
        // Capacity from the header, but not more than a bad header can
        // make us allocate before the rows show up.
        let count = count.min(1 << 20);
        Self {
            splat_path,
            coefficients,
            splats: Vec::with_capacity(count),
            sh_rest: Vec::with_capacity(count * 3 * coefficients),
            stored_linear: true,
        }
    }

    fn push(&mut self, row: &RawVertex) -> Result<(), AssetError> {
        let index = self.splats.len();
        let point = decode_vertex(row).map_err(|error| {
            AssetError(format!(
                "PLY splat '{}' vertex {index}: {error}",
                self.splat_path
            ))
        })?;
        self.stored_linear &=
            (0.0..=1.0).contains(&point.opacity) && point.scale.iter().all(|&scale| scale >= 0.0);
        self.splats.push(point);
        // The file stores each channel's coefficients together.
        let n = self.coefficients;
        let rest = &row.values[REST..REST + 3 * n];
        for k in 0..n {
            self.sh_rest
                .extend((0..3).map(|channel| rest[channel * n + k]));
        }
        Ok(())
    }

    /// Activate opacity and scale, as logits and logs or, for a file that
    /// stores them linear, as they are.
    fn finish(mut self, element: &ElementDef) -> Result<SplatData, AssetError> {
        u32::try_from(self.splats.len()).map_err(|_| {
            AssetError(format!(
                "PLY splat '{}' has too many vertices",
                self.splat_path
            ))
        })?;
        let stores_both = ["opacity", "scale_0", "scale_1", "scale_2"]
            .iter()
            .all(|name| element.properties.contains_key(*name));
        let linear =
            self.stored_linear && stores_both && self.splats.len() >= LINEAR_CHECK_MIN_SPLATS;
        if linear {
            tracing::info!(
                "Splat '{}' stores opacity and scale linear (every opacity in [0, 1], no \
                 negative scale), not as logits and logs; reading them as stored",
                self.splat_path
            );
        }
        for (index, point) in self.splats.iter_mut().enumerate() {
            if !linear {
                point.opacity = sigmoid(point.opacity);
                point.scale = point.scale.map(f32::exp);
                if point
                    .scale
                    .iter()
                    .any(|value| !value.is_finite() || *value <= 0.0)
                {
                    return Err(AssetError(format!(
                        "PLY splat '{}' vertex {index}: activated scale must be finite and \
                         positive",
                        self.splat_path
                    )));
                }
            }
        }
        Ok(SplatData {
            splats: self.splats,
            sh_degree: sh_rest_coefficients_degree(self.coefficients),
            sh_rest: self.sh_rest,
        })
    }
}

/// The degree whose higher bands have `coefficients` per channel.
fn sh_rest_coefficients_degree(coefficients: usize) -> u32 {
    (0..=MAX_SH_DEGREE)
        .find(|&degree| sh_rest_coefficients(degree) == coefficients)
        .unwrap_or(0)
}

/// Whether every property of `element` is a 32-bit float, so its rows can
/// be read straight from the file.
fn all_f32(element: &ElementDef) -> bool {
    element
        .properties
        .values()
        .all(|property| property.data_type == PropertyType::Scalar(ScalarType::Float))
}

enum RowError {
    Io(std::io::Error),
    Decode(AssetError),
}

/// Read `element`'s rows of little-endian 32-bit floats straight from the
/// file, a few megabytes at a time, checking each value as
/// [`RawVertex::set_property`] does. ply-rs hands over one property at a
/// time, which took 23 s for a 10.8M-splat capture; this takes about two.
fn read_f32_rows(
    reader: &mut impl Read,
    element: &ElementDef,
    mut each: impl FnMut(&RawVertex) -> Result<(), AssetError>,
) -> Result<(), RowError> {
    let columns: Vec<(&str, Option<usize>)> = element
        .properties
        .keys()
        .map(|name| (name.as_str(), field_index(name)))
        .collect();
    let stride = columns.len() * 4;
    if stride == 0 {
        return Ok(());
    }
    let chunk_rows = ((4 << 20) / stride).max(1);
    let mut buffer = vec![0u8; chunk_rows * stride];
    let mut left = element.count;
    while left > 0 {
        let rows = chunk_rows.min(left);
        let bytes = &mut buffer[..rows * stride];
        reader.read_exact(bytes).map_err(RowError::Io)?;
        for row_bytes in bytes.chunks_exact(stride) {
            let mut row = RawVertex::new();
            for (&(name, field), value) in columns.iter().zip(row_bytes.as_chunks::<4>().0) {
                let value = f32::from_le_bytes(*value);
                if !value.is_finite() {
                    row.error = Some(format!("property '{name}' is not finite"));
                    break;
                }
                if let Some(index) = field {
                    row.values[index] = value;
                }
            }
            each(&row).map_err(RowError::Decode)?;
        }
        left -= rows;
    }
    Ok(())
}

// Where each known property goes in a `RawVertex`.
const X: usize = 0;
const SCALE: usize = 3;
const ROT: usize = 6;
const OPACITY: usize = 10;
const DC: usize = 11;
const REST: usize = 14;
const MAX_REST: usize = 45;
const FIELDS: usize = REST + MAX_REST;

/// Values for properties a file leaves out, before activation.
const DEFAULTS: [f32; FIELDS] = {
    let mut values = [0.0; FIELDS];
    values[ROT] = 1.0;
    values[DC] = 0.5;
    values[DC + 1] = 0.5;
    values[DC + 2] = 0.5;
    values
};

fn field_index(name: &str) -> Option<usize> {
    let index = match name {
        "x" => X,
        "y" => X + 1,
        "z" => X + 2,
        "scale_0" => SCALE,
        "scale_1" => SCALE + 1,
        "scale_2" => SCALE + 2,
        "rot_0" => ROT,
        "rot_1" => ROT + 1,
        "rot_2" => ROT + 2,
        "rot_3" => ROT + 3,
        "opacity" => OPACITY,
        "f_dc_0" => DC,
        "f_dc_1" => DC + 1,
        "f_dc_2" => DC + 2,
        _ => {
            let digits = name.strip_prefix("f_rest_")?;
            let n: usize = digits.parse().ok()?;
            // Only canonical names: "f_rest_01" is not coefficient 1.
            if n >= MAX_REST || n.to_string() != digits {
                return None;
            }
            REST + n
        }
    };
    Some(index)
}

/// Check the vertex properties once, from the header: known properties
/// must be floats and positions must be there. Returns the SH degree the
/// `f_rest` coefficients make; a set that is not a whole 3DGS degree (a
/// gap, another degree, a non-float value) is ignored, as before higher
/// harmonics were rendered, so the splats keep their base colours.
fn vertex_layout(element: &ply_rs::ply::ElementDef, splat_path: &str) -> Result<u32, String> {
    let mut rest = Vec::new();
    let mut regular = true;
    for (name, property) in &element.properties {
        let is_float = matches!(
            property.data_type,
            PropertyType::Scalar(ScalarType::Float | ScalarType::Double)
        );
        match field_index(name) {
            Some(index) if index >= REST => {
                regular &= is_float;
                rest.push(index - REST);
            }
            Some(_) if !is_float => {
                return Err(format!("property '{name}' must be float or double"));
            }
            Some(_) => {}
            None => regular &= !name.starts_with("f_rest_"),
        }
    }
    for name in ["x", "y", "z"] {
        if !element.properties.contains_key(name) {
            return Err(format!("missing property '{name}'"));
        }
    }
    rest.sort_unstable();
    let degree = (0..=MAX_SH_DEGREE)
        .find(|&degree| rest.len() == 3 * sh_rest_coefficients(degree))
        .filter(|_| regular && rest.iter().enumerate().all(|(i, &n)| i == n));
    Ok(degree.unwrap_or_else(|| {
        tracing::warn!(
            "Splat '{splat_path}': its f_rest properties are not a whole 3DGS spherical-harmonics \
             degree (0, 9, 24 or 45 floats from f_rest_0); drawing base colours only"
        );
        0
    }))
}

/// One vertex's known properties, before activation.
struct RawVertex {
    values: [f32; FIELDS],
    error: Option<String>,
}

impl PropertyAccess for RawVertex {
    fn new() -> Self {
        Self {
            values: DEFAULTS,
            error: None,
        }
    }

    fn set_property(&mut self, name: String, property: Property) {
        if self.error.is_some() {
            return;
        }
        // Reject nonfinite data even in properties the renderer ignores.
        let finite = match &property {
            Property::Float(value) => value.is_finite(),
            Property::Double(value) => value.is_finite(),
            Property::ListFloat(values) => values.iter().all(|value| value.is_finite()),
            Property::ListDouble(values) => values.iter().all(|value| value.is_finite()),
            _ => true,
        };
        if !finite {
            self.error = Some(format!("property '{name}' is not finite"));
            return;
        }
        let Some(index) = field_index(&name) else {
            return;
        };
        let value = match property {
            Property::Float(value) => value,
            Property::Double(value) => value as f32,
            // A non-float coefficient makes the header ignore the whole set.
            _ if index >= REST => return,
            _ => {
                self.error = Some(format!("property '{name}' must be float or double"));
                return;
            }
        };
        if !value.is_finite() {
            self.error = Some(format!("property '{name}' is not a finite f32"));
            return;
        }
        self.values[index] = value;
    }
}

/// One vertex as a splat, its opacity and scale still as stored:
/// [`Decoder::finish`] activates them once it knows how they are stored.
fn decode_vertex(row: &RawVertex) -> Result<SplatPoint, AssetError> {
    if let Some(error) = &row.error {
        return Err(AssetError(error.clone()));
    }
    let values = &row.values;
    let position = [values[X], values[X + 1], values[X + 2]];
    let scale = [values[SCALE], values[SCALE + 1], values[SCALE + 2]];
    let mut rotation = [
        values[ROT],
        values[ROT + 1],
        values[ROT + 2],
        values[ROT + 3],
    ];
    // Scale first so finite, very large quaternions cannot overflow their norm.
    let largest = rotation.iter().map(|value| value.abs()).fold(0.0, f32::max);
    if largest == 0.0 {
        return Err(AssetError("rotation quaternion has zero length".into()));
    }
    rotation.iter_mut().for_each(|value| *value /= largest);
    let length = rotation
        .iter()
        .map(|value| value * value)
        .sum::<f32>()
        .sqrt();
    rotation.iter_mut().for_each(|value| *value /= length);
    let opacity = values[OPACITY];
    let sh_dc = [
        sh_dc_to_color(values[DC]),
        sh_dc_to_color(values[DC + 1]),
        sh_dc_to_color(values[DC + 2]),
    ];
    Ok(SplatPoint {
        position,
        opacity,
        scale,
        rotation,
        sh_dc,
    })
}

/// Sigmoid activation: 1 / (1 + exp(-x)).
fn sigmoid(value: f32) -> f32 {
    1.0 / (1.0 + (-value).exp())
}

/// SH DC coefficient to colour (Y_0^0 normalization), unclamped.
fn sh_dc_to_color(value: f32) -> f32 {
    value * 0.28209479 + 0.5
}

/// Clamps a linear colour to [0, 1] and encodes it as sRGB, as splat colours
/// are stored (IEC 61966-2-1).
fn linear_to_srgb(color: [f32; 3]) -> [f32; 3] {
    color.map(|c| {
        let c = c.clamp(0.0, 1.0);
        if c <= 0.003_130_8 {
            c * 12.92
        } else {
            1.055 * c.powf(1.0 / 2.4) - 0.055
        }
    })
}

/// Create a galaxy/nebula spiral procedural splat cloud. Its colours were
/// chosen as linear light, and are stored as sRGB so it looks as designed.
fn create_procedural_splats() -> SplatData {
    use std::f32::consts::PI;

    let mut gpu_data = Vec::new();

    // Simple LCG pseudo-random for deterministic results without rand crate
    let mut seed: u32 = 42;
    let mut next_rand = || -> f32 {
        seed = seed.wrapping_mul(1103515245).wrapping_add(12345);
        ((seed >> 16) & 0x7FFF) as f32 / 32767.0
    };

    let num_arms = 3;
    let splats_per_arm = 1500;
    let core_splats = 500;
    let total = num_arms * splats_per_arm + core_splats;

    // Galaxy spiral arms
    for arm in 0..num_arms {
        let arm_offset = (arm as f32 / num_arms as f32) * 2.0 * PI;
        for i in 0..splats_per_arm {
            let t = i as f32 / splats_per_arm as f32;
            let radius = t * 3.0 + 0.3;
            let angle = arm_offset + t * 4.0 * PI;

            // Add some spread perpendicular to the arm
            let spread = 0.15 + t * 0.3;
            let dx = (next_rand() - 0.5) * spread;
            let dy = (next_rand() - 0.5) * 0.15; // Thin disk
            let dz = (next_rand() - 0.5) * spread;

            let x = radius * angle.cos() + dx;
            let y = dy;
            let z = radius * angle.sin() + dz;

            let pos = [x, y, z];

            // Color: warm core fading to cool blue/purple at edges
            let core_mix = (1.0 - t).powf(1.5);
            let r = 0.9 * core_mix + 0.15 * (1.0 - core_mix) + next_rand() * 0.1;
            let g = 0.6 * core_mix + 0.1 * (1.0 - core_mix) + next_rand() * 0.05;
            let b = 0.3 * core_mix + 0.7 * (1.0 - core_mix) + next_rand() * 0.15;

            let opacity = (0.6 - t * 0.3).max(0.15) + next_rand() * 0.1;
            let scale_val = 0.04 + t * 0.06 + next_rand() * 0.02;

            gpu_data.push(SplatPoint {
                position: pos,
                opacity,
                scale: [scale_val, scale_val * 0.5, scale_val],
                rotation: [1.0, 0.0, 0.0, 0.0],
                sh_dc: linear_to_srgb([r, g, b]),
            });
        }
    }

    // Dense bright core
    for _ in 0..core_splats {
        let r_dist = next_rand().powf(2.0) * 0.5;
        let theta = next_rand() * 2.0 * PI;
        let phi = (next_rand() - 0.5) * PI * 0.3;

        let x = r_dist * theta.cos() * phi.cos();
        let y = r_dist * phi.sin() * 0.4;
        let z = r_dist * theta.sin() * phi.cos();

        let pos = [x, y, z];

        // Hot white/yellow core
        let r = 1.0;
        let g = 0.85 + next_rand() * 0.15;
        let b = 0.5 + next_rand() * 0.3;

        gpu_data.push(SplatPoint {
            position: pos,
            opacity: 0.5 + next_rand() * 0.3,
            scale: [0.06, 0.03, 0.06],
            rotation: [1.0, 0.0, 0.0, 0.0],
            sh_dc: linear_to_srgb([r, g, b]),
        });
    }

    let count = gpu_data.len();
    assert_eq!(count, total);

    SplatData {
        splats: gpu_data,
        sh_degree: 0,
        sh_rest: Vec::new(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    struct Project(std::path::PathBuf);
    impl Project {
        fn new() -> Self {
            static NEXT: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
            let next = NEXT.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
            let path = std::env::temp_dir()
                .join(format!("naive-splat-assets-{}-{next}", std::process::id()));
            std::fs::create_dir_all(&path).unwrap();
            Self(path)
        }
        fn write(&self, source: impl AsRef<[u8]>) {
            std::fs::write(self.0.join("cloud.ply"), source).unwrap();
        }
    }
    impl Drop for Project {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }
    fn ply(properties: &str, vertex: &str) -> String {
        format!("ply\nformat ascii 1.0\nelement vertex 1\nproperty float x\nproperty float y\nproperty float z\n{properties}end_header\n{vertex}\n")
    }

    #[test]
    fn ascii_and_binary_ply_share_decoded_positions_and_defaults() {
        let project = Project::new();
        project.write(ply("", "1 2 3"));
        let ascii = load_splat(&project.0, "cloud.ply").unwrap();
        assert_eq!(ascii.splats.len(), 1);
        let point = &ascii.splats[0];
        assert_eq!(point.position, [1.0, 2.0, 3.0]);
        assert_eq!(point.scale, [1.0; 3]);
        assert_eq!(point.opacity, 0.5);
        assert_eq!(point.rotation, [1.0, 0.0, 0.0, 0.0]);
        let mut binary = b"ply\nformat binary_little_endian 1.0\nelement vertex 1\nproperty float x\nproperty float y\nproperty float z\nend_header\n".to_vec();
        for value in [1.0f32, 2.0, 3.0] {
            binary.extend(value.to_le_bytes());
        }
        project.write(binary);
        assert_eq!(load_splat(&project.0, "cloud.ply").unwrap(), ascii);
    }

    #[test]
    fn three_dgs_attributes_are_activated_and_quaternion_is_normalized() {
        let project = Project::new();
        project.write(ply(
            "property double scale_0\nproperty float opacity\nproperty float rot_0\nproperty float rot_1\nproperty float f_dc_0\n",
            "1 2 3 0.6931471805599453 0 2 2 0",
        ));
        let data = load_splat(&project.0, "cloud.ply").unwrap();
        let point = &data.splats[0];
        assert!((point.scale[0] - 2.0).abs() < 1e-6);
        assert_eq!(point.opacity, 0.5);
        assert_eq!(point.sh_dc[0], 0.5);
        assert!((point.rotation[0] - std::f32::consts::FRAC_1_SQRT_2).abs() < 1e-6);
        assert_eq!(point.rotation[0], point.rotation[1]);
    }

    #[test]
    fn missing_malformed_empty_and_wrong_property_types_are_errors() {
        let project = Project::new();
        assert!(load_splat(&project.0, "missing.ply").is_err());
        for source in [
            "not a PLY".to_string(),
            "ply\nformat ascii 1.0\nelement vertex 0\nend_header\n".into(),
            "ply\nformat ascii 1.0\nelement face 0\nend_header\n".into(),
            ply("", "1 2"),
            "ply\nformat ascii 1.0\nelement vertex 1\nproperty float x\nend_header\n1\n".into(),
            ply("property int opacity\n", "1 2 3 1"),
        ] {
            project.write(source);
            assert!(load_splat(&project.0, "cloud.ply").is_err());
        }
    }

    #[test]
    fn nonfinite_and_unrepresentable_values_are_rejected() {
        let project = Project::new();
        for source in [
            ply("", "nan 2 3"),
            ply("property float opacity\n", "1 2 3 inf"),
            ply("property double f_dc_0\n", "1 2 3 1e100"),
            ply("property float scale_0\n", "1 2 3 1000"),
            ply("property float rot_0\n", "1 2 3 0"),
            ply("property float f_rest_0\n", "1 2 3 nan"),
        ] {
            project.write(source);
            assert!(load_splat(&project.0, "cloud.ply").is_err());
        }
    }

    fn rest_properties(count: usize) -> String {
        (0..count)
            .map(|i| format!("property float f_rest_{i}\n"))
            .collect()
    }

    /// The file keeps each channel's coefficients together; `sh_rest`
    /// keeps each coefficient's (r, g, b) together.
    #[test]
    fn higher_spherical_harmonics_are_read_per_coefficient() {
        let project = Project::new();
        for degree in 1..=MAX_SH_DEGREE {
            let n = sh_rest_coefficients(degree);
            // Channel c, coefficient k is stored at c * n + k as 100c + k.
            let values: Vec<String> = (0..3)
                .flat_map(|c| (0..n).map(move |k| (100 * c + k).to_string()))
                .collect();
            project.write(ply(
                &format!("property float f_dc_0\n{}", rest_properties(3 * n)),
                &format!("1 2 3 10 {}", values.join(" ")),
            ));
            let data = load_splat(&project.0, "cloud.ply").unwrap();
            assert_eq!(data.sh_degree, degree);
            let expected: Vec<f32> = (0..n)
                .flat_map(|k| (0..3).map(move |c| (100 * c + k) as f32))
                .collect();
            assert_eq!(data.sh_rest, expected, "degree {degree}");
            // The base colour is left unclamped for the higher bands.
            assert!(data.splats[0].sh_dc[0] > 3.0);
        }
        project.write(ply("", "1 2 3"));
        let data = load_splat(&project.0, "cloud.ply").unwrap();
        assert_eq!((data.sh_degree, data.sh_rest.len()), (0, 0));
    }

    /// Coefficients that do not make a whole degree are ignored, as before
    /// higher harmonics were rendered: the splat keeps its base colour.
    #[test]
    fn irregular_spherical_harmonics_fall_back_to_the_base_colour() {
        let project = Project::new();
        let mut gap = rest_properties(8);
        gap.push_str("property float f_rest_9\n");
        for properties in [
            rest_properties(3),
            gap,
            rest_properties(72),
            "property float f_rest_01\n".to_string(),
            format!("{}property uchar f_rest_8\n", rest_properties(8)),
        ] {
            let count = properties.lines().count();
            project.write(ply(&properties, &format!("1 2 3{}", " 0".repeat(count))));
            let data = load_splat(&project.0, "cloud.ply").unwrap();
            assert_eq!((data.sh_degree, data.sh_rest.len()), (0, 0), "{properties}");
            assert_eq!(data.splats[0].position, [1.0, 2.0, 3.0]);
        }
        // Nonfinite values are still rejected, used or not.
        project.write(ply(&rest_properties(3), "1 2 3 0 nan 0"));
        assert!(load_splat(&project.0, "cloud.ply").is_err());
    }

    #[test]
    fn elements_before_the_vertices_are_read_past() {
        let project = Project::new();
        project.write(
            "ply\nformat ascii 1.0\nelement camera 2\nproperty float fx\nproperty list uchar int ids\n\
             element vertex 1\nproperty float x\nproperty float y\nproperty float z\nend_header\n\
             1.5 2 7 8\n2.5 1 9\n4 5 6\n",
        );
        let data = load_splat(&project.0, "cloud.ply").unwrap();
        assert_eq!(data.splats[0].position, [4.0, 5.0, 6.0]);
    }

    /// A binary little-endian PLY of 32-bit floats, `rows` under `names`.
    fn binary_ply(names: &[String], rows: &[Vec<f32>]) -> Vec<u8> {
        let mut bytes = format!(
            "ply\nformat binary_little_endian 1.0\nelement vertex {}\n",
            rows.len()
        )
        .into_bytes();
        for name in names {
            bytes.extend(format!("property float {name}\n").bytes());
        }
        bytes.extend(b"end_header\n");
        for value in rows.iter().flatten() {
            bytes.extend(value.to_le_bytes());
        }
        bytes
    }

    /// The properties a degree-2 capture with normals has, in a trainer's
    /// order.
    fn capture_properties() -> Vec<String> {
        [
            "x", "y", "z", "nx", "ny", "nz", "f_dc_0", "f_dc_1", "f_dc_2",
        ]
        .iter()
        .map(|name| name.to_string())
        .chain((0..24).map(|i| format!("f_rest_{i}")))
        .chain(
            [
                "opacity", "scale_0", "scale_1", "scale_2", "rot_0", "rot_1", "rot_2", "rot_3",
            ]
            .iter()
            .map(|name| name.to_string()),
        )
        .collect()
    }

    /// Rows of exact binary fractions, so ASCII holds them exactly too.
    fn capture_rows(count: usize) -> Vec<Vec<f32>> {
        (0..count)
            .map(|i| {
                (0..capture_properties().len())
                    .map(|j| ((i * 7 + j * 3) % 23) as f32 * 0.125 - 1.5)
                    .collect()
            })
            .collect()
    }

    /// Rows of 32-bit floats are read straight from the file; the result is
    /// what ply-rs makes of the same rows written as text.
    #[test]
    fn binary_float_rows_read_like_ascii_ones() {
        let project = Project::new();
        let names = capture_properties();
        let rows = capture_rows(300);
        project.write(binary_ply(&names, &rows));
        let binary = load_splat(&project.0, "cloud.ply").unwrap();
        assert_eq!((binary.splats.len(), binary.sh_degree), (300, 2));

        let mut ascii = format!("ply\nformat ascii 1.0\nelement vertex {}\n", rows.len());
        for name in &names {
            ascii.push_str(&format!("property float {name}\n"));
        }
        ascii.push_str("end_header\n");
        for row in &rows {
            let values: Vec<String> = row.iter().map(|value| value.to_string()).collect();
            ascii.push_str(&(values.join(" ") + "\n"));
        }
        project.write(ascii);
        assert_eq!(load_splat(&project.0, "cloud.ply").unwrap(), binary);
    }

    #[test]
    fn binary_float_rows_reject_nonfinite_values_and_short_files() {
        let project = Project::new();
        let names = capture_properties();
        let mut rows = capture_rows(10);
        // A property the renderer ignores is checked too.
        rows[7][3] = f32::NAN;
        project.write(binary_ply(&names, &rows));
        let error = load_splat(&project.0, "cloud.ply").unwrap_err().0;
        assert!(
            error.contains("vertex 7") && error.contains("'nx'"),
            "{error}"
        );

        let mut short = binary_ply(&names, &capture_rows(10));
        short.truncate(short.len() - 5);
        project.write(short);
        assert!(load_splat(&project.0, "cloud.ply").is_err());
    }

    #[test]
    fn binary_elements_before_the_vertices_are_read_past() {
        let project = Project::new();
        let mut bytes =
            b"ply\nformat binary_little_endian 1.0\nelement camera 2\nproperty float fx\n\
            element vertex 1\nproperty float x\nproperty float y\nproperty float z\nend_header\n"
                .to_vec();
        for value in [500.0f32, 600.0, 4.0, 5.0, 6.0] {
            bytes.extend(value.to_le_bytes());
        }
        project.write(bytes);
        let data = load_splat(&project.0, "cloud.ply").unwrap();
        assert_eq!(data.splats[0].position, [4.0, 5.0, 6.0]);
    }

    /// Some exporters store opacity and scale already activated. A large
    /// cloud whose every opacity is in [0, 1] and every scale non-negative is
    /// read as stored; one value outside that, a smaller cloud, or a file
    /// without both properties is read as logits and logs.
    #[test]
    fn linear_opacity_and_scale_are_read_as_stored() {
        let project = Project::new();
        let names: Vec<String> = ["x", "y", "z", "opacity", "scale_0", "scale_1", "scale_2"]
            .iter()
            .map(|name| name.to_string())
            .collect();
        let row = |i: usize| vec![i as f32, 0.0, 0.0, 0.25, 0.01, 0.0, 0.5];
        let rows: Vec<Vec<f32>> = (0..LINEAR_CHECK_MIN_SPLATS).map(row).collect();

        project.write(binary_ply(&names, &rows));
        let point = load_splat(&project.0, "cloud.ply").unwrap().splats[3].clone();
        assert_eq!((point.opacity, point.scale), (0.25, [0.01, 0.0, 0.5]));

        let standard = |point: &SplatPoint| {
            (point.opacity - sigmoid(0.25)).abs() < 1e-6
                && (point.scale[0] - 0.01f32.exp()).abs() < 1e-6
        };
        let mut negative = rows.clone();
        negative[500][5] = -0.1;
        project.write(binary_ply(&names, &negative));
        assert!(standard(
            &load_splat(&project.0, "cloud.ply").unwrap().splats[3]
        ));

        project.write(binary_ply(&names, &rows[1..]));
        assert!(standard(
            &load_splat(&project.0, "cloud.ply").unwrap().splats[3]
        ));

        let without_scale: Vec<Vec<f32>> = rows.iter().map(|row| row[..4].to_vec()).collect();
        project.write(binary_ply(&names[..4], &without_scale));
        let point = load_splat(&project.0, "cloud.ply").unwrap().splats[3].clone();
        assert_eq!(point.scale, [1.0; 3]);
        assert!((point.opacity - sigmoid(0.25)).abs() < 1e-6);
    }

    #[test]
    fn procedural_source_is_explicit_deterministic_and_paths_are_confined() {
        let project = Project::new();
        let first = load_splat(&project.0, "procedural").unwrap();
        assert_eq!(first.splats.len(), 5000);
        assert_eq!(first, load_splat(&project.0, "procedural").unwrap());
        for path in ["../cloud.ply", "/tmp/cloud.ply", "procedural:unknown"] {
            assert!(load_splat(&project.0, path).is_err());
        }
        #[cfg(unix)]
        {
            let outside = Project::new();
            outside.write(ply("", "1 2 3"));
            std::os::unix::fs::symlink(outside.0.join("cloud.ply"), project.0.join("escape.ply"))
                .unwrap();
            assert!(load_splat(&project.0, "escape.ply").is_err());
        }
    }
}
