//! Reuse the immutable geometry produced by triangle-mesh convex decomposition.
//!
//! The cache belongs to one physics world. It retains at most 64 shapes and
//! 8 MiB of input geometry keys; this is not a byte limit on Parry's cooked
//! shapes. Live colliders own their own references and survive eviction.

use std::collections::hash_map::DefaultHasher;
use std::collections::VecDeque;
use std::hash::{Hash, Hasher};

use rapier3d::math::Vector;
use rapier3d::parry::transformation::vhacd::VHACDParameters;
use rapier3d::prelude::SharedShape;

const MAX_ENTRIES: usize = 64;
const MAX_GEOMETRY_BYTES: usize = 8 * 1024 * 1024;

/// Triangle-mesh cooking work and the geometry retained for reuse.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct ColliderCookingStats {
    pub mesh_cooks: u64,
    pub cache_hits: u64,
    pub cached_meshes: usize,
    /// Input geometry key bytes, excluding the cooked shapes' allocations.
    pub cached_geometry_bytes: usize,
    pub evictions: u64,
}

struct Entry {
    fingerprint: u64,
    vertices: Box<[[u32; 3]]>,
    indices: Box<[[u32; 3]]>,
    shape: SharedShape,
}

impl Entry {
    fn matches(&self, vertices: &[Vector], indices: &[[u32; 3]]) -> bool {
        self.indices.as_ref() == indices
            && self.vertices.len() == vertices.len()
            && self
                .vertices
                .iter()
                .zip(vertices)
                .all(|(bits, vertex)| *bits == vertex_bits(vertex))
    }

    fn geometry_bytes(&self) -> usize {
        (self.vertices.len() + self.indices.len()) * std::mem::size_of::<[u32; 3]>()
    }
}

pub(super) struct ColliderShapeCache {
    // Least recently used first. With at most 64 entries, a small scan avoids
    // allocating a second geometry key on every hit or relying on a digest
    // alone for identity.
    entries: VecDeque<Entry>,
    max_entries: usize,
    max_geometry_bytes: usize,
    stats: ColliderCookingStats,
}

impl Default for ColliderShapeCache {
    fn default() -> Self {
        Self::with_limits(MAX_ENTRIES, MAX_GEOMETRY_BYTES)
    }
}

impl ColliderShapeCache {
    pub(super) fn with_limits(max_entries: usize, max_geometry_bytes: usize) -> Self {
        Self {
            entries: VecDeque::new(),
            max_entries,
            max_geometry_bytes,
            stats: ColliderCookingStats::default(),
        }
    }

    pub(super) fn stats(&self) -> ColliderCookingStats {
        self.stats
    }

    /// The cooked shape for this geometry, or why it cannot be cooked.
    /// Scene preflight and spawn preparation call this before any live state
    /// changes, so construction only ever finds shapes that cooked.
    pub(super) fn try_get_or_cook(
        &mut self,
        vertices: &[Vector],
        indices: &[[u32; 3]],
    ) -> Result<SharedShape, String> {
        validate_geometry(vertices, indices)?;
        let mut failure = None;
        let shape = self.lookup_or_insert(vertices, indices, || {
            match std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                cook_trimesh(vertices, indices)
            })) {
                Ok(shape) => Some(shape),
                Err(panic) => {
                    failure = Some(panic_message(panic.as_ref()));
                    None
                }
            }
        });
        shape.ok_or_else(|| {
            format!(
                "trimesh collider could not be decomposed into convex parts ({})",
                failure.unwrap_or_default()
            )
        })
    }

    /// Construction-time lookup. Preparation already cooked this geometry
    /// (cooking is deterministic, so an evicted entry cooks again the same
    /// way); a failure here means a caller skipped preparation.
    pub(super) fn get_or_cook(&mut self, vertices: &[Vector], indices: &[[u32; 3]]) -> SharedShape {
        self.try_get_or_cook(vertices, indices)
            .unwrap_or_else(|error| {
                tracing::error!("{error}; using its bounding box, since it was not prepared");
                bounding_box(vertices)
            })
    }

    fn lookup_or_insert(
        &mut self,
        vertices: &[Vector],
        indices: &[[u32; 3]],
        cook: impl FnOnce() -> Option<SharedShape>,
    ) -> Option<SharedShape> {
        let geometry_bytes = vertices
            .len()
            .checked_add(indices.len())
            .and_then(|count| count.checked_mul(std::mem::size_of::<[u32; 3]>()));
        let cacheable = self.max_entries > 0
            && geometry_bytes.is_some_and(|bytes| bytes <= self.max_geometry_bytes);

        // Oversized inputs still use the same cooking algorithm, but do not
        // allocate a cache key or displace useful small entries.
        let fingerprint = cacheable.then(|| geometry_fingerprint(vertices, indices));
        if let Some(fingerprint) = fingerprint {
            if let Some(index) = self.entries.iter().position(|entry| {
                entry.fingerprint == fingerprint && entry.matches(vertices, indices)
            }) {
                let entry = self.entries.remove(index).expect("existing cache entry");
                let shape = entry.shape.clone();
                self.entries.push_back(entry);
                self.stats.cache_hits = self.stats.cache_hits.saturating_add(1);
                return Some(shape);
            }
        }

        let shape = cook()?;
        self.stats.mesh_cooks = self.stats.mesh_cooks.saturating_add(1);
        if let (Some(fingerprint), Some(bytes)) = (fingerprint, geometry_bytes) {
            while self.entries.len() >= self.max_entries
                || self.stats.cached_geometry_bytes > self.max_geometry_bytes - bytes
            {
                let removed = self
                    .entries
                    .pop_front()
                    .expect("cache limits require eviction");
                self.stats.cached_geometry_bytes -= removed.geometry_bytes();
                self.stats.evictions = self.stats.evictions.saturating_add(1);
            }
            self.entries.push_back(Entry {
                fingerprint,
                vertices: vertices.iter().map(vertex_bits).collect(),
                indices: indices.into(),
                shape: shape.clone(),
            });
            self.stats.cached_geometry_bytes += bytes;
            self.stats.cached_meshes = self.entries.len();
        }
        Some(shape)
    }
}

/// Largest coordinate a cooked collider may have, in world units. Convex
/// decomposition stalls for tens of seconds at extreme scales.
pub const MAX_COLLIDER_COORDINATE: f32 = 1.0e6;
/// Smallest triangle (twice its area) that makes a mesh more than a point or
/// a line. A zero scale collapses every triangle, and decomposing that panics.
const MIN_DOUBLE_AREA: f32 = 1.0e-12;

fn validate_geometry(vertices: &[Vector], indices: &[[u32; 3]]) -> Result<(), String> {
    if vertices.is_empty() || indices.is_empty() {
        return Err("trimesh collider has no triangles".into());
    }
    if let Some(vertex) = vertices.iter().find(|vertex| {
        [vertex.x, vertex.y, vertex.z]
            .iter()
            .any(|c| !c.is_finite() || c.abs() > MAX_COLLIDER_COORDINATE)
    }) {
        return Err(format!(
            "trimesh collider vertex {:?} is not finite or is beyond {MAX_COLLIDER_COORDINATE} \
             units (check the entity's scale)",
            [vertex.x, vertex.y, vertex.z]
        ));
    }
    let mut largest = 0.0f32;
    for triangle in indices {
        let [a, b, c] = triangle.map(|index| vertices.get(index as usize));
        let (Some(a), Some(b), Some(c)) = (a, b, c) else {
            return Err("trimesh collider has a triangle index past its vertices".into());
        };
        largest = largest.max((*b - *a).cross(*c - *a).length());
    }
    if largest < MIN_DOUBLE_AREA {
        return Err(
            "trimesh collider has no triangle with area (is the entity's scale zero?)".into(),
        );
    }
    Ok(())
}

fn panic_message(panic: &(dyn std::any::Any + Send)) -> String {
    panic
        .downcast_ref::<&str>()
        .map(|message| message.to_string())
        .or_else(|| panic.downcast_ref::<String>().cloned())
        .unwrap_or_else(|| "panicked".into())
}

fn bounding_box(vertices: &[Vector]) -> SharedShape {
    let (min, max) = vertices.iter().fold(
        (Vector::splat(f32::MAX), Vector::splat(f32::MIN)),
        |(min, max), vertex| (min.min(*vertex), max.max(*vertex)),
    );
    let half = ((max - min) * 0.5).max(Vector::splat(1.0e-3));
    let center = (min + max) * 0.5;
    SharedShape::compound(vec![(
        rapier3d::math::Pose::from_translation(center),
        SharedShape::cuboid(half.x, half.y, half.z),
    )])
}

fn vertex_bits(vertex: &Vector) -> [u32; 3] {
    [vertex.x.to_bits(), vertex.y.to_bits(), vertex.z.to_bits()]
}

fn geometry_fingerprint(vertices: &[Vector], indices: &[[u32; 3]]) -> u64 {
    let mut hash = DefaultHasher::new();
    vertices.len().hash(&mut hash);
    for vertex in vertices {
        vertex_bits(vertex).hash(&mut hash);
    }
    indices.hash(&mut hash);
    hash.finish()
}

fn cook_trimesh(vertices: &[Vector], indices: &[[u32; 3]]) -> SharedShape {
    // Keep the existing decomposition policy for every body type, including
    // its contact and winding behavior. Settings are fixed for this cache's
    // lifetime; there is no alternative cooking mode sharing these entries.
    let params = VHACDParameters {
        resolution: 256,
        concavity: 0.002,
        ..Default::default()
    };
    SharedShape::convex_decomposition_with_params(vertices, indices, &params)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Arc;

    #[test]
    fn fingerprint_collisions_do_not_reuse_different_geometry() {
        let first = [Vector::ZERO, Vector::X, Vector::Z];
        let second = [Vector::ZERO, Vector::X * 2.0, Vector::Z];
        let indices = [[0, 2, 1]];
        let mut cache = ColliderShapeCache::default();
        let original = cache.get_or_cook(&first, &indices);
        // Force the digest collision that the exact geometry check must reject.
        cache.entries[0].fingerprint = geometry_fingerprint(&second, &indices);
        let changed = cache.get_or_cook(&second, &indices);
        assert!(!Arc::ptr_eq(&original.0, &changed.0));
        assert_eq!(cache.stats().mesh_cooks, 2);
        assert_eq!(cache.stats().cache_hits, 0);
        assert_eq!(cache.stats().cached_meshes, 2);
    }
}
