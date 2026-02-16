-- Tier 2.5 Demo: Continuous Collision Detection
-- Press 1 to fire a CCD projectile, Press 2 to fire a non-CCD sphere.
-- CCD projectiles hit the thin wall; non-CCD ones may tunnel through.

function init()
    self.ccd_count = 0
    self.no_ccd_count = 0
    self.ccd_hits = 0
    self.no_ccd_hits = 0

    -- Color the wall
    entity.set_base_color("wall", 0.4, 0.4, 0.5)
    entity.set_metallic("wall", 0.3)

    -- Color the target behind the wall
    entity.set_base_color("target_1", 0.9, 0.2, 0.2)
    entity.set_emission("target_1", 1.0, 0.3, 0.3)

    -- Color the launch markers
    entity.set_base_color("marker_ccd", 0.2, 0.8, 0.2)
    entity.set_base_color("marker_no_ccd", 0.8, 0.2, 0.2)
end

function on_collision(other_id)
    -- Track hits on wall from our projectiles
end

function update(dt)
    -- Key 1: Fire CCD projectile (uses spawn_projectile which has CCD=true)
    if input.just_pressed("slot1") then
        self.ccd_count = self.ccd_count + 1
        local pid = "ccd_proj_" .. self.ccd_count
        -- spawn_projectile: owner, mesh, material, ox, oy, oz, dx, dy, dz, speed, damage, lifetime, gravity
        entity.spawn_projectile(
            _entity_string_id,
            "procedural:sphere",
            "assets/materials/default.yaml",
            -2, 1.5, 0,   -- origin at CCD marker
            0, 0, -1,     -- direction: toward wall
            80,            -- speed: very fast
            0,             -- no damage
            3.0,           -- lifetime
            false          -- no gravity
        )
        -- Muzzle flash
        particles.spawn_burst(-2, 1.5, 0, 15, {
            speed_min = 2, speed_max = 5,
            lifetime_min = 0.1, lifetime_max = 0.3,
            r = 0.2, g = 1.0, b = 0.3, a = 1,
            spread = 60,
            size_start = 0.15, size_end = 0.02,
        })
    end

    -- Key 2: Fire non-CCD sphere (manual spawn + velocity, no CCD)
    if input.just_pressed("slot2") then
        self.no_ccd_count = self.no_ccd_count + 1
        local sid = "noccd_" .. self.no_ccd_count
        entity.spawn(
            sid,
            "procedural:sphere",
            "assets/materials/default.yaml",
            2, 1.5, 0,    -- origin at non-CCD marker
            0.3, 0.3, 0.3 -- scale
        )
        -- Set very high velocity (may tunnel through thin wall)
        physics.set_velocity(sid, 0, 0, -80)
        -- Muzzle flash
        particles.spawn_burst(2, 1.5, 0, 15, {
            speed_min = 2, speed_max = 5,
            lifetime_min = 0.1, lifetime_max = 0.3,
            r = 1.0, g = 0.3, b = 0.2, a = 1,
            spread = 60,
            size_start = 0.15, size_end = 0.02,
        })
    end

    -- Clean up old projectiles that went past the scene
    for i = 1, self.no_ccd_count do
        local sid = "noccd_" .. i
        if entity.exists(sid) then
            local x, y, z = entity.get_position(sid)
            if y < -5 or z < -25 then
                entity.destroy(sid)
            end
        end
    end

    -- Draw HUD
    local sw = ui.screen_width()

    ui.text(sw * 0.5 - 80, 20, "CCD DEMO", 24, 1, 1, 1, 1)

    ui.rect(15, 55, 320, 115, 0, 0, 0, 0.6)
    ui.text(20, 60, "Press 1: Fire CCD projectile (green)", 16, 0.3, 1.0, 0.3, 1)
    ui.text(20, 82, "Press 2: Fire non-CCD sphere (red)", 16, 1.0, 0.3, 0.3, 1)
    ui.text(20, 110, "CCD projectiles hit the thin wall.", 14, 0.7, 0.7, 0.7, 1)
    ui.text(20, 128, "Non-CCD spheres may tunnel through!", 14, 0.7, 0.7, 0.7, 1)
    ui.text(20, 150, string.format("Fired — CCD: %d  |  No-CCD: %d", self.ccd_count, self.no_ccd_count), 14, 0.9, 0.9, 0.9, 1)

    -- Wall label
    local wx, wy, wz = entity.get_position("wall")
    local sx, sy, vis = camera.world_to_screen(wx, wy + 2.5, wz)
    if vis then
        ui.text(sx - 30, sy, "THIN WALL", 14, 0.8, 0.8, 1.0, 0.9)
    end

    -- Target label
    local tx, ty, tz = entity.get_position("target_1")
    local tsx, tsy, tvis = camera.world_to_screen(tx, ty + 1.8, tz)
    if tvis then
        ui.text(tsx - 20, tsy, "TARGET", 14, 1.0, 0.3, 0.3, 0.9)
    end
end
