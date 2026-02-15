-- third_person_demo.lua — Player script for third-person camera demo.
-- Demonstrates: third-person orbit camera, wall collision avoidance,
-- hitscan and projectile combat in third-person perspective.
--
-- Controls:
--   WASD       = move
--   Mouse      = orbit camera
--   Left Click = hitscan shot
--   E          = fire projectile
--   Space      = jump
--   Shift      = sprint

function init()
    self.shoot_cooldown = 0
    self.proj_cooldown = 0
    log("=== THIRD-PERSON CAMERA DEMO ===")
    log("WASD: move | Mouse: orbit camera | LMB: hitscan | E: projectile")
    log("Walk near buildings to see camera wall collision")
    log("Walk through the narrow alley (north) and under the overhang (east)")
end

function update(dt)
    self.shoot_cooldown = math.max(0, self.shoot_cooldown - dt)
    self.proj_cooldown = math.max(0, self.proj_cooldown - dt)

    -- Hitscan on left click
    if input.just_pressed("attack") and self.shoot_cooldown <= 0 then
        self.shoot_cooldown = 0.25
        local px, py, pz = entity.get_position(_entity_string_id)
        local pitch, yaw, roll = entity.get_rotation(_entity_string_id)
        local yaw_rad = math.rad(yaw)
        -- In third person, shoot forward from player's facing direction (flat)
        local dx = -math.sin(yaw_rad)
        local dy = 0
        local dz = -math.cos(yaw_rad)

        local hit, eid, dist, hx, hy, hz = physics.hitscan(
            px, py + 1.0, pz, dx, dy, dz, 80)
        if hit and eid ~= "" and eid ~= _entity_string_id then
            entity.damage(eid, 25)
            log("Hit " .. eid .. " at distance " .. string.format("%.1f", dist))
            ui.flash(1, 1, 0, 0.15, 0.05)
        end
    end

    -- Projectile on E
    if input.just_pressed("interact") and self.proj_cooldown <= 0 then
        self.proj_cooldown = 0.5
        local px, py, pz = entity.get_position(_entity_string_id)
        local pitch, yaw, roll = entity.get_rotation(_entity_string_id)
        local yaw_rad = math.rad(yaw)
        local dx = -math.sin(yaw_rad)
        local dz = -math.cos(yaw_rad)

        entity.spawn_projectile(
            _entity_string_id,
            "procedural:sphere",
            "assets/materials/bullet.yaml",
            px + dx * 1.5, py + 1.0, pz + dz * 1.5,
            dx, 0, dz,
            20, 10, 4.0, false
        )
    end

    -- HUD
    local hp, max_hp = entity.get_health(_entity_string_id)
    local bar_w = 180
    local bar_h = 16
    local bar_x = 20
    local bar_y = ui.screen_height() - 45

    ui.rect(bar_x, bar_y, bar_w, bar_h, 0.15, 0.15, 0.15, 0.7)
    local fill = (hp / max_hp) * bar_w
    ui.rect(bar_x, bar_y, fill, bar_h, 0.2, 0.8, 0.3, 0.9)
    ui.text(bar_x + 5, bar_y + 1, math.floor(hp) .. " HP", 14, 1, 1, 1, 1)

    -- Title
    ui.text(20, 20, "THIRD-PERSON DEMO", 24, 0.3, 0.8, 1.0, 1)
    ui.text(20, 48, "Walk near walls to see camera collision", 13, 0.6, 0.6, 0.6, 1)
end

function on_damage(amount, source_id)
    ui.flash(1, 0, 0, 0.3, 0.1)
end

function on_death()
    log("Player died!")
end
