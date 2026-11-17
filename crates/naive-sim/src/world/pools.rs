//! Reusable entity pools shared by native and headless simulation.

use std::collections::HashMap;

// ---------------------------------------------------------------------------
// Entity Pool Manager (Tier 2)
// ---------------------------------------------------------------------------

struct EntityPool {
    available: Vec<String>,
    all: Vec<String>,
    mesh: String,
    material: String,
}

/// Manages reusable entity pools to avoid spawn/destroy overhead.
pub struct EntityPoolManager {
    pools: HashMap<String, EntityPool>,
}

impl EntityPoolManager {
    pub fn new() -> Self {
        Self {
            pools: HashMap::new(),
        }
    }

    /// Create a new pool (does not pre-warm — that happens via acquire calls).
    pub fn create_pool(&mut self, name: &str, mesh: &str, material: &str) {
        self.pools.insert(
            name.to_string(),
            EntityPool {
                available: Vec::new(),
                all: Vec::new(),
                mesh: mesh.to_string(),
                material: material.to_string(),
            },
        );
    }

    /// Return an available entity ID from the pool, or None if pool is empty.
    pub fn try_acquire(&mut self, pool_name: &str) -> Option<String> {
        self.pools
            .get_mut(pool_name)
            .and_then(|pool| pool.available.pop())
    }

    /// Release an entity back to the pool's available list.
    pub fn release(&mut self, pool_name: &str, entity_id: &str) {
        if let Some(pool) = self.pools.get_mut(pool_name) {
            if !pool.available.contains(&entity_id.to_string()) {
                pool.available.push(entity_id.to_string());
            }
        }
    }

    /// Register a newly spawned entity in the pool's tracking.
    pub fn register_entity(&mut self, pool_name: &str, entity_id: &str) {
        if let Some(pool) = self.pools.get_mut(pool_name) {
            pool.all.push(entity_id.to_string());
        }
    }

    /// Get the mesh and material for a pool.
    pub fn get_pool_assets(&self, pool_name: &str) -> Option<(String, String)> {
        self.pools
            .get(pool_name)
            .map(|p| (p.mesh.clone(), p.material.clone()))
    }

    /// Check if a pool exists.
    pub fn has_pool(&self, name: &str) -> bool {
        self.pools.contains_key(name)
    }

    /// Get pool stats: (total, available).
    pub fn pool_size(&self, name: &str) -> (usize, usize) {
        self.pools
            .get(name)
            .map(|p| (p.all.len(), p.available.len()))
            .unwrap_or((0, 0))
    }
}

impl Default for EntityPoolManager {
    fn default() -> Self {
        Self::new()
    }
}
