-- tier2_stress_spawner.lua — Spawns 300+ entities to test dynamic GPU buffer.
-- Previously this would crash at entity #257 due to the fixed 64KB uniform buffer.

local materials = {
    "assets/materials/neon_pink.yaml",
    "assets/materials/neon_cyan.yaml",
    "assets/materials/neon_purple.yaml",
    "assets/materials/neon_gold.yaml",
    "assets/materials/neon_blue.yaml",
    "assets/materials/neon_green.yaml",
    "assets/materials/titan_ruby.yaml",
    "assets/materials/titan_sapphire.yaml",
    "assets/materials/titan_emerald.yaml",
    "assets/materials/titan_gold.yaml",
}

function init()
    self.spawned = false
    self.count = 0
    self.target = 320
    self.time = 0
end

function update(dt)
    self.time = self.time + dt

    -- Spawn entities over several frames to avoid a single-frame spike
    if self.count < self.target then
        local batch = math.min(40, self.target - self.count)
        for i = 1, batch do
            local idx = self.count + i
            local ring = math.floor(idx / 20)
            local slot = idx % 20
            local angle = (slot / 20) * math.pi * 2
            local radius = 3 + ring * 2.5
            local x = math.sin(angle) * radius
            local z = math.cos(angle) * radius
            local y = math.sin(idx * 0.5) * 1.5 + 2

            local mat = materials[(idx % #materials) + 1]
            local scale = 0.3 + (idx % 5) * 0.08

            entity.spawn(
                "stress_" .. idx,
                "procedural:sphere",
                mat,
                x, y, z,
                scale, scale, scale
            )
        end
        self.count = self.count + batch
        if self.count >= self.target then
            log("Stress test: spawned " .. self.count .. " entities (old limit: 256)")
        end
    end

    -- Animate all spawned entities: gentle orbit
    for i = 1, self.count do
        local id = "stress_" .. i
        if entity.exists(id) then
            local ring = math.floor(i / 20)
            local slot = i % 20
            local base_angle = (slot / 20) * math.pi * 2
            local radius = 3 + ring * 2.5
            local orbit_speed = 0.2 + ring * 0.05
            local angle = base_angle + self.time * orbit_speed
            local x = math.sin(angle) * radius
            local z = math.cos(angle) * radius
            local y = math.sin(self.time * 0.8 + i * 0.3) * 1.5 + 2
            entity.set_position(id, x, y, z)
        end
    end

    -- Counter HUD
    ui.text(20, 92, "Entities: " .. self.count .. " / " .. self.target, 18, 0.3, 1, 0.3, 1)
end
