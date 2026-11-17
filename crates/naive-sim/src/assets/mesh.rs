//! CPU mesh decoding shared by GPU uploads and headless simulation.
use super::AssetError;
use std::path::Path;

#[derive(Clone, Copy, Debug)]
pub struct CpuVertex {
    pub position: [f32; 3],
    pub normal: [f32; 3],
    pub tex_coords: [f32; 2],
    pub color: [f32; 4],
    pub joint_indices: [u32; 4],
    pub joint_weights: [f32; 4],
}

pub struct SkinData {
    pub skeleton: naive_core::animation::Skeleton,
    pub clips: Vec<naive_core::animation::AnimationClip>,
}

pub struct CpuMesh {
    pub vertices: Vec<CpuVertex>,
    pub indices: Vec<u32>,
    pub skin: Option<SkinData>,
    /// Imported images normalized to tightly packed RGBA8 for native uploads.
    pub images: Vec<gltf::image::Data>,
    pub base_color_image: Option<usize>,
}

impl CpuMesh {
    pub fn physics_trimesh(&self, scale: glam::Vec3) -> crate::physics::PhysicsTrimesh {
        (
            self.vertices
                .iter()
                .map(|v| {
                    rapier3d::math::Vector::new(
                        v.position[0] * scale.x,
                        v.position[1] * scale.y,
                        v.position[2] * scale.z,
                    )
                })
                .collect(),
            self.indices
                .as_chunks::<3>()
                .0
                .iter()
                .map(|v| [v[0], v[1], v[2]])
                .collect(),
        )
    }
}

fn validate_indices(positions: &[[f32; 3]], indices: &[u32]) -> Result<(), AssetError> {
    if positions.is_empty()
        || indices.is_empty()
        || !indices.len().is_multiple_of(3)
        || indices.iter().any(|&i| i as usize >= positions.len())
        || positions.iter().flatten().any(|x| !x.is_finite())
    {
        return Err(AssetError(
            "Mesh has empty, non-finite or invalid triangle geometry".into(),
        ));
    }
    Ok(())
}

pub fn load_mesh(root: &Path, path: &str) -> Result<CpuMesh, AssetError> {
    if let Some(shape) = path.strip_prefix("procedural:") {
        let (vertices, indices) = match shape {
            "cube" => cube_geometry(),
            "sphere" => sphere_geometry(0.5, 32, 32),
            "plane" => plane_geometry(1.0, 1.0, 1),
            "cylinder" => cylinder_geometry(0.5, 1.0, 32),
            "cone" => cone_geometry(0.5, 1.0, 32),
            "torus" => torus_geometry(0.3, 0.1, 32, 16),
            _ => return Err(AssetError(format!("Unknown procedural mesh: {shape}"))),
        };
        return Ok(CpuMesh {
            vertices,
            indices,
            skin: None,
            images: vec![],
            base_color_image: None,
        });
    }
    let full = crate::project_path::resolve(root, path).map_err(AssetError)?;
    if path.to_ascii_lowercase().ends_with(".stl") {
        return load_stl(&full);
    }
    crate::project_path::validate_gltf_resources(root, &full).map_err(AssetError)?;
    let (document, buffers, images) =
        gltf::import(&full).map_err(|e| AssetError(format!("glTF {path}: {e}")))?;
    let mut vertices = Vec::new();
    let mut indices = Vec::new();
    let mut primitives = 0;
    for scene in document.scenes() {
        for node in scene.nodes() {
            collect_node_meshes(
                &node,
                glam::Mat4::IDENTITY,
                &buffers,
                &mut vertices,
                &mut indices,
                &mut primitives,
            )?;
        }
    }
    validate_indices(
        &vertices.iter().map(|v| v.position).collect::<Vec<_>>(),
        &indices,
    )?;
    let base_color_image = document
        .meshes()
        .flat_map(|m| m.primitives())
        .find_map(|p| {
            p.material()
                .pbr_metallic_roughness()
                .base_color_texture()
                .map(|t| t.texture().source().index())
        });
    let skin = extract_skin_data(&document, &buffers)?;
    let images = images
        .into_iter()
        .map(normalize_image)
        .collect::<Result<Vec<_>, _>>()?;
    Ok(CpuMesh {
        vertices,
        indices,
        skin,
        images,
        base_color_image,
    })
}

fn load_stl(path: &Path) -> Result<CpuMesh, AssetError> {
    let mut file = std::io::BufReader::new(std::fs::File::open(path)?);
    let mesh = stl_io::read_stl(&mut file)
        .map_err(|e| AssetError(format!("STL {}: {e}", path.display())))?;
    let positions: Vec<[f32; 3]> = mesh.vertices.iter().map(|v| [v[0], v[1], v[2]]).collect();
    let indices: Vec<u32> = mesh
        .faces
        .iter()
        .flat_map(|f| f.vertices.map(|i| i as u32))
        .collect();
    validate_indices(&positions, &indices)?;
    // Preserve the native STL loader's accumulation of authored face normals;
    // geometry-based area weighting changes the appearance of existing models.
    let mut normals = vec![glam::Vec3::ZERO; positions.len()];
    for face in &mesh.faces {
        let normal = glam::Vec3::new(face.normal[0], face.normal[1], face.normal[2]);
        if !normal.is_finite() {
            return Err(AssetError("STL face normal is not finite".into()));
        }
        for &index in &face.vertices {
            normals[index] += normal;
        }
    }
    let normals = normals.into_iter().map(|normal| {
        let length = normal.length();
        if length > 1e-8 {
            (normal / length).to_array()
        } else {
            [0.0, 1.0, 0.0]
        }
    });
    let vertices = positions
        .into_iter()
        .zip(normals)
        .map(|(position, normal)| CpuVertex {
            position,
            normal,
            tex_coords: [0.; 2],
            color: [1.; 4],
            joint_indices: [0; 4],
            joint_weights: [1., 0., 0., 0.],
        })
        .collect();
    Ok(CpuMesh {
        vertices,
        indices,
        skin: None,
        images: vec![],
        base_color_image: None,
    })
}
/// Convert every glTF image format into the one byte layout native uploads use.
fn normalize_image(mut source: gltf::image::Data) -> Result<gltf::image::Data, AssetError> {
    use gltf::image::Format;
    let (channels, bytes_per_channel) = match source.format {
        Format::R8 => (1, 1),
        Format::R8G8 => (2, 1),
        Format::R8G8B8 => (3, 1),
        Format::R8G8B8A8 => (4, 1),
        Format::R16 => (1, 2),
        Format::R16G16 => (2, 2),
        Format::R16G16B16 => (3, 2),
        Format::R16G16B16A16 => (4, 2),
        Format::R32G32B32FLOAT => (3, 4),
        Format::R32G32B32A32FLOAT => (4, 4),
    };
    let count = (source.width as usize)
        .checked_mul(source.height as usize)
        .filter(|&count| count > 0)
        .ok_or_else(|| AssetError("glTF image has invalid dimensions".into()))?;
    let stride = channels * bytes_per_channel;
    let expected = count
        .checked_mul(stride)
        .ok_or_else(|| AssetError("glTF image is too large".into()))?;
    if source.pixels.len() != expected {
        return Err(AssetError(
            "glTF image byte count does not match its format".into(),
        ));
    }
    let mut pixels = Vec::with_capacity(
        count
            .checked_mul(4)
            .ok_or_else(|| AssetError("glTF image is too large".into()))?,
    );
    for pixel in source.pixels.chunks_exact(stride) {
        let mut color = [255u8; 4];
        for (index, component) in pixel.chunks_exact(bytes_per_channel).enumerate() {
            color[index] = match bytes_per_channel {
                1 => component[0],
                2 => {
                    ((u16::from_ne_bytes([component[0], component[1]]) as u32 * 255 + 32767)
                        / 65535) as u8
                }
                _ => {
                    let value =
                        f32::from_ne_bytes(component.try_into().expect("four-byte image channel"));
                    if !value.is_finite() {
                        return Err(AssetError("glTF image contains a nonfinite channel".into()));
                    }
                    (value.clamp(0.0, 1.0) * 255.0).round() as u8
                }
            };
        }
        if channels <= 2 {
            color = [
                color[0],
                color[0],
                color[0],
                if channels == 2 { color[1] } else { 255 },
            ];
        }
        pixels.extend(color);
    }
    source.pixels = pixels;
    source.format = Format::R8G8B8A8;
    Ok(source)
}

fn normalized_quaternion(value: [f32; 4], context: &str) -> Result<glam::Quat, AssetError> {
    let quaternion = glam::Quat::from_array(value);
    let norm = quaternion.length_squared();
    if !quaternion.is_finite() || !norm.is_finite() || norm <= 0.0 {
        return Err(AssetError(format!("{context} has an invalid quaternion")));
    }
    Ok(quaternion.normalize())
}

fn extract_skin_data(
    document: &gltf::Document,
    buffers: &[gltf::buffer::Data],
) -> Result<Option<SkinData>, AssetError> {
    use naive_core::animation::{
        AnimationChannel, AnimationClip, ChannelProperty, ChannelValues, Interpolation, Joint,
        JointTransform, Skeleton, MAX_JOINTS,
    };
    let Some(skin) = document.skins().next() else {
        return Ok(None);
    };
    let nodes: Vec<_> = skin.joints().collect();
    if nodes.is_empty() || nodes.len() > MAX_JOINTS {
        return Err(AssetError(format!(
            "glTF skin must contain 1..={MAX_JOINTS} joints"
        )));
    }
    let ibms: Vec<_> = skin
        .reader(|buffer| Some(&buffers[buffer.index()]))
        .read_inverse_bind_matrices()
        .map(|values| {
            values
                .map(|matrix| glam::Mat4::from_cols_array_2d(&matrix))
                .collect()
        })
        .unwrap_or_else(|| vec![glam::Mat4::IDENTITY; nodes.len()]);
    if ibms.len() != nodes.len() || ibms.iter().any(|matrix| !matrix.is_finite()) {
        return Err(AssetError(
            "glTF inverse bind matrices are missing or nonfinite".into(),
        ));
    }
    let node_to_joint: std::collections::HashMap<_, _> = nodes
        .iter()
        .enumerate()
        .map(|(index, node)| (node.index(), index))
        .collect();
    let mut parents = std::collections::HashMap::new();
    for node in &nodes {
        for child in node.children() {
            if node_to_joint.contains_key(&child.index()) {
                parents.insert(child.index(), node.index());
            }
        }
    }
    let joints = nodes
        .iter()
        .enumerate()
        .map(|(index, node)| {
            let (translation, rotation, scale) = node.transform().decomposed();
            let translation = glam::Vec3::from(translation);
            let scale = glam::Vec3::from(scale);
            if !translation.is_finite() || !scale.is_finite() {
                return Err(AssetError("glTF joint transform is not finite".into()));
            }
            Ok(Joint {
                name: node.name().unwrap_or("joint").into(),
                parent: parents
                    .get(&node.index())
                    .and_then(|parent| node_to_joint.get(parent).copied()),
                inverse_bind_matrix: ibms[index],
                local_transform: JointTransform {
                    translation,
                    rotation: normalized_quaternion(rotation, "glTF joint")?,
                    scale,
                },
            })
        })
        .collect::<Result<Vec<_>, _>>()?;
    let mut clips = Vec::new();
    for (clip_index, animation) in document.animations().enumerate() {
        let mut channels = Vec::new();
        for channel in animation.channels() {
            let Some(&joint_index) = node_to_joint.get(&channel.target().node().index()) else {
                continue;
            };
            let property = match channel.target().property() {
                gltf::animation::Property::Translation => ChannelProperty::Translation,
                gltf::animation::Property::Scale => ChannelProperty::Scale,
                gltf::animation::Property::Rotation => ChannelProperty::Rotation,
                _ => continue,
            };
            let interpolation = match channel.sampler().interpolation() {
                gltf::animation::Interpolation::Step => Interpolation::Step,
                gltf::animation::Interpolation::Linear => Interpolation::Linear,
                gltf::animation::Interpolation::CubicSpline => Interpolation::CubicSpline,
            };
            let reader = channel.reader(|buffer| Some(&buffers[buffer.index()]));
            let timestamps: Vec<_> = reader
                .read_inputs()
                .ok_or_else(|| AssetError("glTF animation has no timestamps".into()))?
                .collect();
            if timestamps.is_empty()
                || timestamps
                    .iter()
                    .any(|time| !time.is_finite() || *time < 0.0)
                || timestamps.windows(2).any(|pair| pair[0] >= pair[1])
            {
                return Err(AssetError(
                    "glTF animation timestamps must be finite and increasing".into(),
                ));
            }
            let cubic = interpolation == Interpolation::CubicSpline;
            let expected = timestamps.len() * if cubic { 3 } else { 1 };
            let output = reader
                .read_outputs()
                .ok_or_else(|| AssetError("glTF animation has no keyframe values".into()))?;
            let values = match output {
                gltf::animation::util::ReadOutputs::Translations(values)
                | gltf::animation::util::ReadOutputs::Scales(values) => {
                    let values: Vec<_> = values.map(glam::Vec3::from).collect();
                    if values.len() != expected || values.iter().any(|value| !value.is_finite()) {
                        return Err(AssetError(
                            "glTF animation has invalid vector keyframes".into(),
                        ));
                    }
                    // The runtime approximates cubic channels with linear interpolation;
                    // retain the value slots, rather than treating tangents as keyframes.
                    ChannelValues::Vec3(if cubic {
                        values
                            .as_chunks::<3>()
                            .0
                            .iter()
                            .map(|value| value[1])
                            .collect()
                    } else {
                        values
                    })
                }
                gltf::animation::util::ReadOutputs::Rotations(values) => {
                    let values: Vec<_> = values.into_f32().collect();
                    if values.len() != expected
                        || values.iter().flatten().any(|value| !value.is_finite())
                    {
                        return Err(AssetError(
                            "glTF animation has invalid rotation keyframes".into(),
                        ));
                    }
                    let values: Vec<_> = if cubic {
                        values
                            .as_chunks::<3>()
                            .0
                            .iter()
                            .map(|value| value[1])
                            .collect()
                    } else {
                        values
                    };
                    ChannelValues::Quat(
                        values
                            .into_iter()
                            .map(|value| normalized_quaternion(value, "glTF animation keyframe"))
                            .collect::<Result<_, _>>()?,
                    )
                }
                _ => {
                    return Err(AssetError(
                        "glTF animation output does not match its target".into(),
                    ))
                }
            };
            channels.push(AnimationChannel {
                joint_index,
                property,
                interpolation,
                timestamps,
                values,
            });
        }
        let duration = channels
            .iter()
            .flat_map(|channel| channel.timestamps.last())
            .copied()
            .fold(0.0, f32::max);
        clips.push(AnimationClip {
            name: animation
                .name()
                .map(str::to_string)
                .unwrap_or_else(|| format!("clip_{clip_index}")),
            duration,
            channels,
        });
    }
    Ok(Some(SkinData {
        skeleton: Skeleton::new(joints),
        clips,
    }))
}

fn collect_node_meshes(
    node: &gltf::Node,
    parent_transform: glam::Mat4,
    buffers: &[gltf::buffer::Data],
    vertices: &mut Vec<CpuVertex>,
    indices: &mut Vec<u32>,
    prim_count: &mut u32,
) -> Result<(), AssetError> {
    let local = glam::Mat4::from_cols_array_2d(&node.transform().matrix());
    let world = parent_transform * local;
    if !local.is_finite() || !world.is_finite() {
        return Err(AssetError("glTF node transform is not finite".into()));
    }
    // Normal matrix: inverse-transpose of upper-left 3x3
    let normal_mat = glam::Mat3::from_mat4(world).inverse().transpose();

    if let Some(mesh) = node.mesh() {
        for primitive in mesh.primitives() {
            if primitive.mode() != gltf::mesh::Mode::Triangles {
                return Err(AssetError(
                    "Only triangle glTF primitives are supported".into(),
                ));
            }
            let reader = primitive.reader(|buf| Some(&buffers[buf.index()]));

            let positions: Vec<[f32; 3]> = match reader.read_positions() {
                Some(p) => p.collect(),
                None => return Err(AssetError("glTF primitive has no positions".into())),
            };

            let tex_coords: Vec<[f32; 2]> = reader
                .read_tex_coords(0)
                .map(|t| t.into_f32().collect())
                .unwrap_or_else(|| vec![[0.0, 0.0]; positions.len()]);

            let colors: Vec<[f32; 4]> = reader
                .read_colors(0)
                .map(|c| c.into_rgba_f32().collect())
                .unwrap_or_else(|| vec![[1.0, 1.0, 1.0, 1.0]; positions.len()]);

            // Collect indices first so we can use them for normal generation
            let prim_indices: Vec<u32> = if let Some(read_indices) = reader.read_indices() {
                read_indices.into_u32().collect()
            } else {
                (0..positions.len() as u32).collect()
            };

            validate_indices(&positions, &prim_indices)?;
            let normals: Vec<[f32; 3]> =
                reader
                    .read_normals()
                    .map(|n| n.collect())
                    .unwrap_or_else(|| {
                        if !prim_indices.is_empty() {
                            generate_smooth_normals(&positions, &prim_indices)
                        } else {
                            generate_flat_normals(&positions)
                        }
                    });

            let base_vertex = vertices.len() as u32;

            // Read joint indices and weights for skinned meshes
            let joints: Vec<[u32; 4]> = reader
                .read_joints(0)
                .map(|j| {
                    j.into_u16()
                        .map(|v| [v[0] as u32, v[1] as u32, v[2] as u32, v[3] as u32])
                        .collect()
                })
                .unwrap_or_else(|| vec![[0, 0, 0, 0]; positions.len()]);

            let weights: Vec<[f32; 4]> = reader
                .read_weights(0)
                .map(|w| w.into_f32().collect())
                .unwrap_or_else(|| vec![[1.0, 0.0, 0.0, 0.0]; positions.len()]);

            for (name, length, finite) in [
                (
                    "NORMAL",
                    normals.len(),
                    normals.iter().flatten().all(|value| value.is_finite()),
                ),
                (
                    "TEXCOORD_0",
                    tex_coords.len(),
                    tex_coords.iter().flatten().all(|value| value.is_finite()),
                ),
                (
                    "COLOR_0",
                    colors.len(),
                    colors.iter().flatten().all(|value| value.is_finite()),
                ),
                (
                    "WEIGHTS_0",
                    weights.len(),
                    weights.iter().flatten().all(|value| value.is_finite()),
                ),
            ] {
                if length != positions.len() || !finite {
                    return Err(AssetError(format!(
                        "glTF {name} has an invalid count or nonfinite values"
                    )));
                }
            }
            if joints.len() != positions.len()
                || weights.iter().any(|weights| {
                    let sum = weights.iter().sum::<f32>();
                    weights.iter().any(|weight| *weight < 0.0) || !sum.is_finite() || sum <= 0.0
                })
            {
                return Err(AssetError(
                    "glTF skin attributes have invalid counts or weights".into(),
                ));
            }
            let has_joint_attribute = reader.read_joints(0).is_some();
            let has_weight_attribute = reader.read_weights(0).is_some();
            let has_joints = node.skin().is_some();
            if has_joint_attribute != has_weight_attribute || (has_joints && !has_joint_attribute) {
                return Err(AssetError(
                    "glTF skin requires paired JOINTS_0 and WEIGHTS_0 attributes".into(),
                ));
            }
            if has_joint_attribute {
                let joint_count = node
                    .skin()
                    .map(|skin| skin.joints().len())
                    .unwrap_or(naive_core::animation::MAX_JOINTS);
                if joint_count > naive_core::animation::MAX_JOINTS
                    || joints
                        .iter()
                        .flatten()
                        .any(|&joint| joint as usize >= joint_count)
                {
                    return Err(AssetError(
                        "glTF vertex joint index is outside the supported skin".into(),
                    ));
                }
            }

            for (i, pos) in positions.iter().enumerate() {
                let (p, n) = if has_joints {
                    // Skinned: store local-space positions (skeleton transforms at runtime)
                    let local_p = local.transform_point3(glam::Vec3::from(*pos));
                    let local_n = glam::Mat3::from_mat4(local).inverse().transpose()
                        * glam::Vec3::from(normals.get(i).copied().unwrap_or([0.0, 1.0, 0.0]));
                    (local_p, local_n)
                } else {
                    // Non-skinned: bake world transform
                    let p = world.transform_point3(glam::Vec3::from(*pos));
                    let n = normal_mat
                        * glam::Vec3::from(normals.get(i).copied().unwrap_or([0.0, 1.0, 0.0]));
                    (p, n)
                };
                if !p.is_finite() || !n.is_finite() {
                    return Err(AssetError("glTF transformed vertex is not finite".into()));
                }
                vertices.push(CpuVertex {
                    position: p.to_array(),
                    normal: n.normalize_or_zero().to_array(),
                    tex_coords: tex_coords.get(i).copied().unwrap_or([0.0, 0.0]),
                    color: colors.get(i).copied().unwrap_or([1.0, 1.0, 1.0, 1.0]),
                    joint_indices: joints.get(i).copied().unwrap_or([0, 0, 0, 0]),
                    joint_weights: weights.get(i).copied().unwrap_or([1.0, 0.0, 0.0, 0.0]),
                });
            }

            for idx in &prim_indices {
                indices.push(base_vertex + idx);
            }

            *prim_count += 1;
        }
    }

    // Recurse into children
    for child in node.children() {
        collect_node_meshes(&child, world, buffers, vertices, indices, prim_count)?;
    }
    Ok(())
}

fn generate_smooth_normals(positions: &[[f32; 3]], indices: &[u32]) -> Vec<[f32; 3]> {
    let mut normals = vec![glam::Vec3::ZERO; positions.len()];

    // Accumulate face normals at each vertex
    for tri in indices.as_chunks::<3>().0 {
        let i0 = tri[0] as usize;
        let i1 = tri[1] as usize;
        let i2 = tri[2] as usize;
        if i0 < positions.len() && i1 < positions.len() && i2 < positions.len() {
            let v0 = glam::Vec3::from(positions[i0]);
            let v1 = glam::Vec3::from(positions[i1]);
            let v2 = glam::Vec3::from(positions[i2]);
            let face_normal = (v1 - v0).cross(v2 - v0);
            // Weight by face area (unnormalized cross product magnitude)
            normals[i0] += face_normal;
            normals[i1] += face_normal;
            normals[i2] += face_normal;
        }
    }

    // Normalize
    normals
        .iter()
        .map(|n| {
            let normalized = n.normalize_or_zero();
            if normalized == glam::Vec3::ZERO {
                [0.0, 1.0, 0.0]
            } else {
                normalized.to_array()
            }
        })
        .collect()
}

fn generate_flat_normals(positions: &[[f32; 3]]) -> Vec<[f32; 3]> {
    let mut normals = vec![[0.0f32, 1.0, 0.0]; positions.len()];
    for chunk in (0..positions.len()).step_by(3) {
        if chunk + 2 < positions.len() {
            let v0 = glam::Vec3::from(positions[chunk]);
            let v1 = glam::Vec3::from(positions[chunk + 1]);
            let v2 = glam::Vec3::from(positions[chunk + 2]);
            let normal = (v1 - v0).cross(v2 - v0).normalize_or_zero();
            let n = normal.to_array();
            normals[chunk] = n;
            normals[chunk + 1] = n;
            normals[chunk + 2] = n;
        }
    }
    normals
}

fn sphere_geometry(radius: f32, rings: u32, sectors: u32) -> (Vec<CpuVertex>, Vec<u32>) {
    let mut vertices = Vec::new();
    let mut indices = Vec::new();

    for ring in 0..=rings {
        let theta = std::f32::consts::PI * ring as f32 / rings as f32;
        let sin_theta = theta.sin();
        let cos_theta = theta.cos();

        for sector in 0..=sectors {
            let phi = 2.0 * std::f32::consts::PI * sector as f32 / sectors as f32;
            let sin_phi = phi.sin();
            let cos_phi = phi.cos();

            let nx = sin_theta * cos_phi;
            let ny = cos_theta;
            let nz = sin_theta * sin_phi;

            vertices.push(CpuVertex {
                position: [radius * nx, radius * ny, radius * nz],
                normal: [nx, ny, nz],
                tex_coords: [sector as f32 / sectors as f32, ring as f32 / rings as f32],
                color: [1.0, 1.0, 1.0, 1.0],
                joint_indices: [0, 0, 0, 0],
                joint_weights: [1.0, 0.0, 0.0, 0.0],
            });
        }
    }

    for ring in 0..rings {
        for sector in 0..sectors {
            let curr_row = ring * (sectors + 1);
            let next_row = (ring + 1) * (sectors + 1);

            // CCW winding when viewed from outside the sphere
            indices.push(curr_row + sector);
            indices.push(next_row + sector + 1);
            indices.push(next_row + sector);

            indices.push(curr_row + sector);
            indices.push(curr_row + sector + 1);
            indices.push(next_row + sector + 1);
        }
    }

    (vertices, indices)
}

fn cube_geometry() -> (Vec<CpuVertex>, Vec<u32>) {
    let j = [0u32, 0, 0, 0];
    let w = [1.0f32, 0.0, 0.0, 0.0];
    #[rustfmt::skip]
    let vertices: Vec<CpuVertex> = vec![
        // Front face (z = 0.5)
        CpuVertex { position: [-0.5, -0.5,  0.5], normal: [ 0.0,  0.0,  1.0], tex_coords: [0.0, 1.0], color: [1.0, 1.0, 1.0, 1.0], joint_indices: j, joint_weights: w },
        CpuVertex { position: [ 0.5, -0.5,  0.5], normal: [ 0.0,  0.0,  1.0], tex_coords: [1.0, 1.0], color: [1.0, 1.0, 1.0, 1.0], joint_indices: j, joint_weights: w },
        CpuVertex { position: [ 0.5,  0.5,  0.5], normal: [ 0.0,  0.0,  1.0], tex_coords: [1.0, 0.0], color: [1.0, 1.0, 1.0, 1.0], joint_indices: j, joint_weights: w },
        CpuVertex { position: [-0.5,  0.5,  0.5], normal: [ 0.0,  0.0,  1.0], tex_coords: [0.0, 0.0], color: [1.0, 1.0, 1.0, 1.0], joint_indices: j, joint_weights: w },
        // Back face (z = -0.5)
        CpuVertex { position: [ 0.5, -0.5, -0.5], normal: [ 0.0,  0.0, -1.0], tex_coords: [0.0, 1.0], color: [1.0, 1.0, 1.0, 1.0], joint_indices: j, joint_weights: w },
        CpuVertex { position: [-0.5, -0.5, -0.5], normal: [ 0.0,  0.0, -1.0], tex_coords: [1.0, 1.0], color: [1.0, 1.0, 1.0, 1.0], joint_indices: j, joint_weights: w },
        CpuVertex { position: [-0.5,  0.5, -0.5], normal: [ 0.0,  0.0, -1.0], tex_coords: [1.0, 0.0], color: [1.0, 1.0, 1.0, 1.0], joint_indices: j, joint_weights: w },
        CpuVertex { position: [ 0.5,  0.5, -0.5], normal: [ 0.0,  0.0, -1.0], tex_coords: [0.0, 0.0], color: [1.0, 1.0, 1.0, 1.0], joint_indices: j, joint_weights: w },
        // Top face (y = 0.5)
        CpuVertex { position: [-0.5,  0.5,  0.5], normal: [ 0.0,  1.0,  0.0], tex_coords: [0.0, 1.0], color: [1.0, 1.0, 1.0, 1.0], joint_indices: j, joint_weights: w },
        CpuVertex { position: [ 0.5,  0.5,  0.5], normal: [ 0.0,  1.0,  0.0], tex_coords: [1.0, 1.0], color: [1.0, 1.0, 1.0, 1.0], joint_indices: j, joint_weights: w },
        CpuVertex { position: [ 0.5,  0.5, -0.5], normal: [ 0.0,  1.0,  0.0], tex_coords: [1.0, 0.0], color: [1.0, 1.0, 1.0, 1.0], joint_indices: j, joint_weights: w },
        CpuVertex { position: [-0.5,  0.5, -0.5], normal: [ 0.0,  1.0,  0.0], tex_coords: [0.0, 0.0], color: [1.0, 1.0, 1.0, 1.0], joint_indices: j, joint_weights: w },
        // Bottom face (y = -0.5)
        CpuVertex { position: [-0.5, -0.5, -0.5], normal: [ 0.0, -1.0,  0.0], tex_coords: [0.0, 1.0], color: [1.0, 1.0, 1.0, 1.0], joint_indices: j, joint_weights: w },
        CpuVertex { position: [ 0.5, -0.5, -0.5], normal: [ 0.0, -1.0,  0.0], tex_coords: [1.0, 1.0], color: [1.0, 1.0, 1.0, 1.0], joint_indices: j, joint_weights: w },
        CpuVertex { position: [ 0.5, -0.5,  0.5], normal: [ 0.0, -1.0,  0.0], tex_coords: [1.0, 0.0], color: [1.0, 1.0, 1.0, 1.0], joint_indices: j, joint_weights: w },
        CpuVertex { position: [-0.5, -0.5,  0.5], normal: [ 0.0, -1.0,  0.0], tex_coords: [0.0, 0.0], color: [1.0, 1.0, 1.0, 1.0], joint_indices: j, joint_weights: w },
        // Right face (x = 0.5)
        CpuVertex { position: [ 0.5, -0.5,  0.5], normal: [ 1.0,  0.0,  0.0], tex_coords: [0.0, 1.0], color: [1.0, 1.0, 1.0, 1.0], joint_indices: j, joint_weights: w },
        CpuVertex { position: [ 0.5, -0.5, -0.5], normal: [ 1.0,  0.0,  0.0], tex_coords: [1.0, 1.0], color: [1.0, 1.0, 1.0, 1.0], joint_indices: j, joint_weights: w },
        CpuVertex { position: [ 0.5,  0.5, -0.5], normal: [ 1.0,  0.0,  0.0], tex_coords: [1.0, 0.0], color: [1.0, 1.0, 1.0, 1.0], joint_indices: j, joint_weights: w },
        CpuVertex { position: [ 0.5,  0.5,  0.5], normal: [ 1.0,  0.0,  0.0], tex_coords: [0.0, 0.0], color: [1.0, 1.0, 1.0, 1.0], joint_indices: j, joint_weights: w },
        // Left face (x = -0.5)
        CpuVertex { position: [-0.5, -0.5, -0.5], normal: [-1.0,  0.0,  0.0], tex_coords: [0.0, 1.0], color: [1.0, 1.0, 1.0, 1.0], joint_indices: j, joint_weights: w },
        CpuVertex { position: [-0.5, -0.5,  0.5], normal: [-1.0,  0.0,  0.0], tex_coords: [1.0, 1.0], color: [1.0, 1.0, 1.0, 1.0], joint_indices: j, joint_weights: w },
        CpuVertex { position: [-0.5,  0.5,  0.5], normal: [-1.0,  0.0,  0.0], tex_coords: [1.0, 0.0], color: [1.0, 1.0, 1.0, 1.0], joint_indices: j, joint_weights: w },
        CpuVertex { position: [-0.5,  0.5, -0.5], normal: [-1.0,  0.0,  0.0], tex_coords: [0.0, 0.0], color: [1.0, 1.0, 1.0, 1.0], joint_indices: j, joint_weights: w },
    ];

    // Counter-clockwise seen from outside, so back-face culling keeps the
    // outer faces.
    #[rustfmt::skip]
    let indices: Vec<u32> = vec![
        0,  1,  2,  0,  2,  3,   // front
        4,  5,  6,  4,  6,  7,   // back
        8,  9,  10, 8,  10, 11,  // top
        12, 13, 14, 12, 14, 15,  // bottom
        16, 17, 18, 16, 18, 19,  // right
        20, 21, 22, 20, 22, 23,  // left
    ];

    (vertices, indices)
}

fn plane_geometry(width: f32, depth: f32, subdivisions: u32) -> (Vec<CpuVertex>, Vec<u32>) {
    let j = [0u32, 0, 0, 0];
    let w = [1.0f32, 0.0, 0.0, 0.0];
    let segs = subdivisions + 1;
    let hw = width / 2.0;
    let hd = depth / 2.0;

    let mut vertices = Vec::new();
    let mut indices = Vec::new();

    for iz in 0..=subdivisions {
        for ix in 0..=subdivisions {
            let u = ix as f32 / subdivisions as f32;
            let v = iz as f32 / subdivisions as f32;
            vertices.push(CpuVertex {
                position: [-hw + u * width, 0.0, -hd + v * depth],
                normal: [0.0, 1.0, 0.0],
                tex_coords: [u, v],
                color: [1.0, 1.0, 1.0, 1.0],
                joint_indices: j,
                joint_weights: w,
            });
        }
    }

    for iz in 0..subdivisions {
        for ix in 0..subdivisions {
            let tl = iz * segs + ix;
            let tr = tl + 1;
            let bl = (iz + 1) * segs + ix;
            let br = bl + 1;
            indices.extend_from_slice(&[tl, br, tr, tl, bl, br]);
        }
    }

    (vertices, indices)
}

fn cylinder_geometry(radius: f32, height: f32, segments: u32) -> (Vec<CpuVertex>, Vec<u32>) {
    let j = [0u32, 0, 0, 0];
    let w = [1.0f32, 0.0, 0.0, 0.0];
    let half_h = height / 2.0;

    let mut vertices = Vec::new();
    let mut indices = Vec::new();

    // Side vertices: two rings
    for i in 0..=segments {
        let angle = 2.0 * std::f32::consts::PI * i as f32 / segments as f32;
        let (sin_a, cos_a) = (angle.sin(), angle.cos());
        let u = i as f32 / segments as f32;

        // Bottom ring
        vertices.push(CpuVertex {
            position: [radius * cos_a, -half_h, radius * sin_a],
            normal: [cos_a, 0.0, sin_a],
            tex_coords: [u, 1.0],
            color: [1.0, 1.0, 1.0, 1.0],
            joint_indices: j,
            joint_weights: w,
        });
        // Top ring
        vertices.push(CpuVertex {
            position: [radius * cos_a, half_h, radius * sin_a],
            normal: [cos_a, 0.0, sin_a],
            tex_coords: [u, 0.0],
            color: [1.0, 1.0, 1.0, 1.0],
            joint_indices: j,
            joint_weights: w,
        });
    }

    // Side indices
    for i in 0..segments {
        let b0 = i * 2;
        let t0 = b0 + 1;
        let b1 = b0 + 2;
        let t1 = b0 + 3;
        indices.extend_from_slice(&[b0, t0, t1, b0, t1, b1]);
    }

    // Top cap
    let top_center = vertices.len() as u32;
    vertices.push(CpuVertex {
        position: [0.0, half_h, 0.0],
        normal: [0.0, 1.0, 0.0],
        tex_coords: [0.5, 0.5],
        color: [1.0, 1.0, 1.0, 1.0],
        joint_indices: j,
        joint_weights: w,
    });
    for i in 0..=segments {
        let angle = 2.0 * std::f32::consts::PI * i as f32 / segments as f32;
        let (sin_a, cos_a) = (angle.sin(), angle.cos());
        vertices.push(CpuVertex {
            position: [radius * cos_a, half_h, radius * sin_a],
            normal: [0.0, 1.0, 0.0],
            tex_coords: [0.5 + 0.5 * cos_a, 0.5 + 0.5 * sin_a],
            color: [1.0, 1.0, 1.0, 1.0],
            joint_indices: j,
            joint_weights: w,
        });
    }
    for i in 0..segments {
        indices.extend_from_slice(&[top_center, top_center + 2 + i, top_center + 1 + i]);
    }

    // Bottom cap
    let bot_center = vertices.len() as u32;
    vertices.push(CpuVertex {
        position: [0.0, -half_h, 0.0],
        normal: [0.0, -1.0, 0.0],
        tex_coords: [0.5, 0.5],
        color: [1.0, 1.0, 1.0, 1.0],
        joint_indices: j,
        joint_weights: w,
    });
    for i in 0..=segments {
        let angle = 2.0 * std::f32::consts::PI * i as f32 / segments as f32;
        let (sin_a, cos_a) = (angle.sin(), angle.cos());
        vertices.push(CpuVertex {
            position: [radius * cos_a, -half_h, radius * sin_a],
            normal: [0.0, -1.0, 0.0],
            tex_coords: [0.5 + 0.5 * cos_a, 0.5 - 0.5 * sin_a],
            color: [1.0, 1.0, 1.0, 1.0],
            joint_indices: j,
            joint_weights: w,
        });
    }
    for i in 0..segments {
        indices.extend_from_slice(&[bot_center, bot_center + 1 + i, bot_center + 2 + i]);
    }

    (vertices, indices)
}

fn cone_geometry(radius: f32, height: f32, segments: u32) -> (Vec<CpuVertex>, Vec<u32>) {
    let j = [0u32, 0, 0, 0];
    let w = [1.0f32, 0.0, 0.0, 0.0];
    let half_h = height / 2.0;
    let slope = radius / height;

    let mut vertices = Vec::new();
    let mut indices = Vec::new();

    // Tip vertex
    let tip_idx = 0u32;
    vertices.push(CpuVertex {
        position: [0.0, half_h, 0.0],
        normal: [0.0, 1.0, 0.0],
        tex_coords: [0.5, 0.0],
        color: [1.0, 1.0, 1.0, 1.0],
        joint_indices: j,
        joint_weights: w,
    });

    // Base ring vertices for sides
    for i in 0..=segments {
        let angle = 2.0 * std::f32::consts::PI * i as f32 / segments as f32;
        let (sin_a, cos_a) = (angle.sin(), angle.cos());
        let ny = slope;
        let len = (1.0 + ny * ny).sqrt();
        vertices.push(CpuVertex {
            position: [radius * cos_a, -half_h, radius * sin_a],
            normal: [cos_a / len, ny / len, sin_a / len],
            tex_coords: [i as f32 / segments as f32, 1.0],
            color: [1.0, 1.0, 1.0, 1.0],
            joint_indices: j,
            joint_weights: w,
        });
    }

    // Side triangles
    for i in 0..segments {
        indices.extend_from_slice(&[tip_idx, 2 + i, 1 + i]);
    }

    // Base cap
    let bot_center = vertices.len() as u32;
    vertices.push(CpuVertex {
        position: [0.0, -half_h, 0.0],
        normal: [0.0, -1.0, 0.0],
        tex_coords: [0.5, 0.5],
        color: [1.0, 1.0, 1.0, 1.0],
        joint_indices: j,
        joint_weights: w,
    });
    for i in 0..=segments {
        let angle = 2.0 * std::f32::consts::PI * i as f32 / segments as f32;
        let (sin_a, cos_a) = (angle.sin(), angle.cos());
        vertices.push(CpuVertex {
            position: [radius * cos_a, -half_h, radius * sin_a],
            normal: [0.0, -1.0, 0.0],
            tex_coords: [0.5 + 0.5 * cos_a, 0.5 - 0.5 * sin_a],
            color: [1.0, 1.0, 1.0, 1.0],
            joint_indices: j,
            joint_weights: w,
        });
    }
    for i in 0..segments {
        indices.extend_from_slice(&[bot_center, bot_center + 1 + i, bot_center + 2 + i]);
    }

    (vertices, indices)
}

fn torus_geometry(
    major_radius: f32,
    minor_radius: f32,
    major_segments: u32,
    minor_segments: u32,
) -> (Vec<CpuVertex>, Vec<u32>) {
    let j = [0u32, 0, 0, 0];
    let w = [1.0f32, 0.0, 0.0, 0.0];
    let pi2 = 2.0 * std::f32::consts::PI;

    let mut vertices = Vec::new();
    let mut indices = Vec::new();

    for i in 0..=major_segments {
        let theta = pi2 * i as f32 / major_segments as f32;
        let (sin_t, cos_t) = (theta.sin(), theta.cos());

        for k in 0..=minor_segments {
            let phi = pi2 * k as f32 / minor_segments as f32;
            let (sin_p, cos_p) = (phi.sin(), phi.cos());

            let x = (major_radius + minor_radius * cos_p) * cos_t;
            let y = minor_radius * sin_p;
            let z = (major_radius + minor_radius * cos_p) * sin_t;

            vertices.push(CpuVertex {
                position: [x, y, z],
                normal: [cos_p * cos_t, sin_p, cos_p * sin_t],
                tex_coords: [
                    i as f32 / major_segments as f32,
                    k as f32 / minor_segments as f32,
                ],
                color: [1.0, 1.0, 1.0, 1.0],
                joint_indices: j,
                joint_weights: w,
            });
        }
    }

    let ring_size = minor_segments + 1;
    for i in 0..major_segments {
        for k in 0..minor_segments {
            let a = i * ring_size + k;
            let b = a + 1;
            let c = (i + 1) * ring_size + k;
            let d = c + 1;
            indices.extend_from_slice(&[a, b, d, a, d, c]);
        }
    }

    (vertices, indices)
}

#[cfg(test)]
mod tests {
    use super::*;
    use glam::Vec3;

    struct Fixture {
        root: std::path::PathBuf,
        document: serde_json::Value,
        bytes: Vec<u8>,
        offsets: std::collections::HashMap<String, usize>,
    }

    impl Fixture {
        fn new() -> Self {
            static NEXT: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
            let id = NEXT.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
            let root = std::env::temp_dir()
                .join(format!("naive-mesh-validation-{}-{id}", std::process::id()));
            std::fs::create_dir_all(&root).unwrap();
            let mut fixture = Self {
                root,
                bytes: vec![],
                offsets: Default::default(),
                document: serde_json::json!({
                    "asset":{"version":"2.0"}, "scene":0, "scenes":[{"nodes":[0,1]}],
                    "nodes":[{"mesh":0,"skin":0},{"name":"root"}], "skins":[{"joints":[1]}],
                    "meshes":[{"primitives":[{"attributes":{}}]}],
                    "buffers":[{"uri":"mesh.bin","byteLength":0}], "bufferViews":[], "accessors":[],
                }),
            };
            for (name, values, kind, count) in [
                (
                    "POSITION",
                    vec![0., 0., 0., 1., 0., 0., 0., 1., 0.],
                    "VEC3",
                    3,
                ),
                (
                    "NORMAL",
                    vec![0., 0., 1., 0., 0., 1., 0., 0., 1.],
                    "VEC3",
                    3,
                ),
                ("TEXCOORD_0", vec![0.; 6], "VEC2", 3),
                ("COLOR_0", vec![1.; 12], "VEC4", 3),
                (
                    "WEIGHTS_0",
                    vec![1., 0., 0., 0., 1., 0., 0., 0., 1., 0., 0., 0.],
                    "VEC4",
                    3,
                ),
            ] {
                let accessor = fixture.floats(name, &values, kind, count);
                fixture.document["meshes"][0]["primitives"][0]["attributes"][name] =
                    accessor.into();
            }
            fixture.document["accessors"][0]["min"] = serde_json::json!([0, 0, 0]);
            fixture.document["accessors"][0]["max"] = serde_json::json!([1, 1, 0]);
            let joints = fixture.accessor("JOINTS_0", &[0; 24], 5123, "VEC4", 3);
            fixture.document["meshes"][0]["primitives"][0]["attributes"]["JOINTS_0"] =
                joints.into();
            let indices = fixture.accessor("indices", &[0, 0, 1, 0, 2, 0], 5123, "SCALAR", 3);
            fixture.document["meshes"][0]["primitives"][0]["indices"] = indices.into();
            let input = fixture.floats("timestamps", &[0., 1.], "SCALAR", 2);
            let output = fixture.floats("rotations", &[0., 0., 0., 1., 0., 0., 0., 1.], "VEC4", 2);
            fixture.document["animations"] = serde_json::json!([{
                "samplers":[{"input":input,"output":output}],
                "channels":[{"sampler":0,"target":{"node":1,"path":"rotation"}}],
            }]);
            fixture
        }

        fn accessor(
            &mut self,
            name: &str,
            bytes: &[u8],
            component: u32,
            kind: &str,
            count: usize,
        ) -> usize {
            while !self.bytes.len().is_multiple_of(4) {
                self.bytes.push(0);
            }
            let offset = self.bytes.len();
            self.offsets.insert(name.into(), offset);
            self.bytes.extend(bytes);
            let views = self.document["bufferViews"].as_array_mut().unwrap();
            let view = views.len();
            views
                .push(serde_json::json!({"buffer":0,"byteOffset":offset,"byteLength":bytes.len()}));
            let accessors = self.document["accessors"].as_array_mut().unwrap();
            let index = accessors.len();
            accessors.push(serde_json::json!({"bufferView":view,"componentType":component,"count":count,"type":kind}));
            index
        }

        fn floats(&mut self, name: &str, values: &[f32], kind: &str, count: usize) -> usize {
            self.accessor(
                name,
                &values
                    .iter()
                    .flat_map(|value| value.to_le_bytes())
                    .collect::<Vec<_>>(),
                5126,
                kind,
                count,
            )
        }

        fn write(&mut self) {
            self.document["buffers"][0]["byteLength"] = self.bytes.len().into();
            std::fs::write(self.root.join("mesh.bin"), &self.bytes).unwrap();
            std::fs::write(self.root.join("mesh.gltf"), self.document.to_string()).unwrap();
        }

        fn bad_float(&mut self, field: &str, value: f32) {
            let start = self.offsets[field];
            self.bytes[start..start + 4].copy_from_slice(&value.to_le_bytes());
        }
    }
    impl Drop for Fixture {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.root);
        }
    }

    #[test]
    fn imported_luma_texture_and_all_supported_pixel_formats_normalize_to_rgba8() {
        use gltf::image::{Data, Format};
        let mut fixture = Fixture::new();
        image::GrayImage::from_pixel(1, 1, image::Luma([64]))
            .save(fixture.root.join("gray.png"))
            .unwrap();
        fixture.document["images"] = serde_json::json!([{"uri":"gray.png"}]);
        fixture.document["textures"] = serde_json::json!([{"source":0}]);
        fixture.document["materials"] =
            serde_json::json!([{"pbrMetallicRoughness":{"baseColorTexture":{"index":0}}}]);
        fixture.document["meshes"][0]["primitives"][0]["material"] = 0.into();
        fixture.write();
        let loaded = load_mesh(&fixture.root, "mesh.gltf").unwrap();
        assert_eq!(loaded.images[0].format, Format::R8G8B8A8);
        assert_eq!(loaded.images[0].pixels, [64, 64, 64, 255]);
        for (format, pixels, expected) in [
            (Format::R8, vec![12], vec![12, 12, 12, 255]),
            (Format::R8G8, vec![12, 34], vec![12, 12, 12, 34]),
            (Format::R8G8B8, vec![12, 34, 56], vec![12, 34, 56, 255]),
            (Format::R8G8B8A8, vec![12, 34, 56, 78], vec![12, 34, 56, 78]),
            (Format::R16, u16::MAX.to_ne_bytes().to_vec(), vec![255; 4]),
            (
                Format::R16G16,
                [u16::MAX, 0]
                    .into_iter()
                    .flat_map(u16::to_ne_bytes)
                    .collect(),
                vec![255, 255, 255, 0],
            ),
            (
                Format::R16G16B16,
                [u16::MAX, 0, 0]
                    .into_iter()
                    .flat_map(u16::to_ne_bytes)
                    .collect(),
                vec![255, 0, 0, 255],
            ),
            (
                Format::R16G16B16A16,
                [u16::MAX, 0, 0, u16::MAX]
                    .into_iter()
                    .flat_map(u16::to_ne_bytes)
                    .collect(),
                vec![255, 0, 0, 255],
            ),
            (
                Format::R32G32B32FLOAT,
                [1.0f32, 0., 0.]
                    .into_iter()
                    .flat_map(f32::to_ne_bytes)
                    .collect(),
                vec![255, 0, 0, 255],
            ),
            (
                Format::R32G32B32A32FLOAT,
                [1.0f32, 0., 0., 1.]
                    .into_iter()
                    .flat_map(f32::to_ne_bytes)
                    .collect(),
                vec![255, 0, 0, 255],
            ),
        ] {
            let normalized = normalize_image(Data {
                format,
                pixels,
                width: 1,
                height: 1,
            })
            .unwrap();
            assert_eq!(normalized.format, Format::R8G8B8A8);
            assert_eq!(normalized.pixels, expected, "{format:?}");
        }
        assert!(normalize_image(Data {
            format: Format::R8G8B8A8,
            pixels: vec![0],
            width: 1,
            height: 1
        })
        .is_err());
        assert!(normalize_image(Data {
            format: Format::R32G32B32FLOAT,
            pixels: [f32::NAN, 0., 0.]
                .into_iter()
                .flat_map(f32::to_ne_bytes)
                .collect(),
            width: 1,
            height: 1
        })
        .is_err());
    }

    #[test]
    fn invalid_geometry_and_nonfinite_vertex_attributes_fail_before_use() {
        for field in ["POSITION", "NORMAL", "TEXCOORD_0", "COLOR_0", "WEIGHTS_0"] {
            let mut fixture = Fixture::new();
            fixture.bad_float(field, f32::NAN);
            fixture.write();
            assert!(
                load_mesh(&fixture.root, "mesh.gltf").is_err(),
                "accepted nonfinite {field}"
            );
        }
        let mut fixture = Fixture::new();
        let index = fixture.offsets["indices"];
        fixture.bytes[index..index + 2].copy_from_slice(&9u16.to_le_bytes());
        fixture.write();
        assert!(load_mesh(&fixture.root, "mesh.gltf").is_err());
        let mut fixture = Fixture::new();
        let normal = fixture.document["meshes"][0]["primitives"][0]["attributes"]["NORMAL"]
            .as_u64()
            .unwrap() as usize;
        fixture.document["accessors"][normal]["count"] = 2.into();
        fixture.write();
        assert!(load_mesh(&fixture.root, "mesh.gltf").is_err());
    }

    #[test]
    fn invalid_skin_indices_weights_transforms_and_rotation_clips_are_errors() {
        for field in ["rotations", "timestamps"] {
            let mut fixture = Fixture::new();
            fixture.bad_float(field, f32::NAN);
            fixture.write();
            assert!(
                load_mesh(&fixture.root, "mesh.gltf").is_err(),
                "accepted nonfinite {field}"
            );
        }
        let mut fixture = Fixture::new();
        let start = fixture.offsets["JOINTS_0"];
        fixture.bytes[start..start + 2].copy_from_slice(&1u16.to_le_bytes());
        fixture.write();
        assert!(load_mesh(&fixture.root, "mesh.gltf").is_err());
        let mut fixture = Fixture::new();
        fixture.bad_float("WEIGHTS_0", -1.0);
        fixture.write();
        assert!(load_mesh(&fixture.root, "mesh.gltf").is_err());
        let mut fixture = Fixture::new();
        fixture.document["nodes"][1]["rotation"] = serde_json::json!([0, 0, 0, 0]);
        fixture.write();
        assert!(load_mesh(&fixture.root, "mesh.gltf").is_err());
        let mut fixture = Fixture::new();
        let start = fixture.offsets["rotations"];
        fixture.bytes[start..start + 16].fill(0);
        fixture.write();
        assert!(load_mesh(&fixture.root, "mesh.gltf").is_err());
        let mut fixture = Fixture::new();
        let ibms = fixture.floats("inverse_bind", &[f32::NAN; 16], "MAT4", 1);
        fixture.document["skins"][0]["inverseBindMatrices"] = ibms.into();
        fixture.write();
        assert!(load_mesh(&fixture.root, "mesh.gltf").is_err());
    }

    #[test]
    fn cubic_rotation_tangents_do_not_become_invalid_zero_quaternions() {
        let mut fixture = Fixture::new();
        let output = fixture.floats(
            "cubic_rotations",
            &[
                0., 0., 0., 0., 0., 0., 0., 1., 0., 0., 0., 0., 0., 0., 0., 0., 0., 0., 0., 1., 0.,
                0., 0., 0.,
            ],
            "VEC4",
            6,
        );
        fixture.document["animations"][0]["samplers"][0]["output"] = output.into();
        fixture.document["animations"][0]["samplers"][0]["interpolation"] = "CUBICSPLINE".into();
        fixture.write();
        let loaded = load_mesh(&fixture.root, "mesh.gltf").unwrap();
        let skin = loaded.skin.unwrap();
        let naive_core::animation::ChannelValues::Quat(values) = &skin.clips[0].channels[0].values
        else {
            panic!("rotation clip missing");
        };
        assert_eq!(values, &[glam::Quat::IDENTITY; 2]);
    }

    #[test]
    fn stl_keeps_original_authored_face_normal_accumulation() {
        let fixture = Fixture::new();
        std::fs::write(fixture.root.join("mesh.stl"), "solid normals\nfacet normal 0 0 1\nouter loop\nvertex 0 0 0\nvertex 1 0 0\nvertex 0 1 0\nendloop\nendfacet\nfacet normal 0 1 0\nouter loop\nvertex 0 0 0\nvertex 0 0 4\nvertex 4 0 0\nendloop\nendfacet\nendsolid normals\n").unwrap();
        let mesh = load_mesh(&fixture.root, "mesh.stl").unwrap();
        let normal = mesh
            .vertices
            .iter()
            .find(|vertex| vertex.position == [0.; 3])
            .unwrap()
            .normal;
        assert!((normal[1] - std::f32::consts::FRAC_1_SQRT_2).abs() < 1e-6);
        assert_eq!(normal[1], normal[2]);
    }

    /// The mesh passes cull back faces with `FrontFace::Ccw`, so every
    /// triangle must wind counter-clockwise seen from the side its normals
    /// point to. One wound the other way is culled from outside and drawn
    /// from inside, where its normals face away from the light.
    fn assert_wound_outward(shape: &str, (vertices, indices): (Vec<CpuVertex>, Vec<u32>)) {
        for tri in indices.as_chunks::<3>().0 {
            let [a, b, c] = [tri[0], tri[1], tri[2]].map(|i| &vertices[i as usize]);
            let (pa, pb, pc) = (
                Vec3::from(a.position),
                Vec3::from(b.position),
                Vec3::from(c.position),
            );
            let face_normal = (pb - pa).cross(pc - pa);
            // Triangles that collapse at a pole have no winding.
            if face_normal.length() < 1e-6 {
                continue;
            }
            let vertex_normal = Vec3::from(a.normal) + Vec3::from(b.normal) + Vec3::from(c.normal);
            assert!(
                face_normal.dot(vertex_normal) > 0.0,
                "procedural {shape}: triangle {tri:?} winds clockwise seen from outside"
            );
        }
    }

    #[test]
    fn procedural_meshes_wind_counter_clockwise_seen_from_outside() {
        assert_wound_outward("cube", cube_geometry());
        assert_wound_outward("sphere", sphere_geometry(0.5, 32, 32));
        assert_wound_outward("plane", plane_geometry(1.0, 1.0, 4));
        assert_wound_outward("cylinder", cylinder_geometry(0.5, 1.0, 32));
        assert_wound_outward("cone", cone_geometry(0.5, 1.0, 32));
        assert_wound_outward("torus", torus_geometry(0.3, 0.1, 32, 16));
    }
}
