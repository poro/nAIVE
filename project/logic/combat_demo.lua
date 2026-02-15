-- combat_demo.lua — Player script for the Tier 1 combat demo.
-- Demonstrates: health HUD, hitscan shooting (left click), projectile shooting (E key),
-- and taking damage from collision_damage spike traps.
--
-- Controls:
--   WASD      = move
--   Mouse     = look
--   Left Click = hitscan shot (instant, 25 damage)
--   E         = fire projectile (physics ball, 10 damage)
--   Space     = jump

function init()
    self.shoot_cooldown = 0
    self.proj_cooldown = 0
    log("=== COMBAT DEMO ===")
    log("Left Click: hitscan (25 dmg)  |  E: projectile (10 dmg)")
    log("Walk into spike traps to take 15 damage")
end

function update(dt)
    self.shoot_cooldown = math.max(0, self.shoot_cooldown - dt)
    self.proj_cooldown = math.max(0, self.proj_cooldown - dt)

    -- Hitscan shooting on left click
    if input.just_pressed("attack") and self.shoot_cooldown <= 0 then
        self.shoot_cooldown = 0.2
        local px, py, pz = entity.get_position(_entity_string_id)
        -- Get player look direction from rotation
        local pitch, yaw, roll = entity.get_rotation(_entity_string_id)
        local yaw_rad = math.rad(yaw)
        local pitch_rad = math.rad(pitch)
        local dx = -math.sin(yaw_rad) * math.cos(pitch_rad)
        local dy = math.sin(pitch_rad)
        local dz = -math.cos(yaw_rad) * math.cos(pitch_rad)

        local hit, eid, dist, hx, hy, hz, nx, ny, nz =
            physics.hitscan(px, py + 0.7, pz, dx, dy, dz, 100)

        if hit and eid ~= "" and eid ~= _entity_string_id then
            local new_hp = entity.damage(eid, 25)
            log("Hitscan hit " .. eid .. " for 25 dmg (hp=" .. new_hp .. ")")
            ui.flash(1, 1, 0.5, 0.15, 0.05)
        else
            ui.flash(0.3, 0.3, 0.3, 0.1, 0.05)
        end
    end

    -- Projectile shooting on E
    if input.just_pressed("interact") and self.proj_cooldown <= 0 then
        self.proj_cooldown = 0.4
        local px, py, pz = entity.get_position(_entity_string_id)
        local pitch, yaw, roll = entity.get_rotation(_entity_string_id)
        local yaw_rad = math.rad(yaw)
        local pitch_rad = math.rad(pitch)
        local dx = -math.sin(yaw_rad) * math.cos(pitch_rad)
        local dy = math.sin(pitch_rad)
        local dz = -math.cos(yaw_rad) * math.cos(pitch_rad)

        -- Spawn slightly in front of the player
        local sx = px + dx * 1.5
        local sy = py + 0.7 + dy * 1.5
        local sz = pz + dz * 1.5

        entity.spawn_projectile(
            _entity_string_id,
            "procedural:sphere",
            "assets/materials/bullet.yaml",
            sx, sy, sz,
            dx, dy, dz,
            25,     -- speed
            10,     -- damage
            5.0,    -- lifetime
            false   -- no gravity
        )
        log("Fired projectile!")
        ui.flash(1, 0.8, 0, 0.2, 0.05)
    end

    -- HUD
    local hp, max_hp = entity.get_health(_entity_string_id)
    local bar_w = 200
    local bar_h = 20
    local bar_x = 20
    local bar_y = ui.screen_height() - 50
    local fill = (hp / max_hp) * bar_w

    -- Health bar background
    ui.rect(bar_x, bar_y, bar_w, bar_h, 0.2, 0.2, 0.2, 0.8)
    -- Health bar fill (green->red based on health)
    local r = math.clamp(1.0 - (hp / max_hp), 0, 1)
    local g = math.clamp(hp / max_hp, 0, 1)
    ui.rect(bar_x, bar_y, fill, bar_h, r, g, 0.1, 0.9)
    -- Health text
    ui.text(bar_x + 5, bar_y + 2, math.floor(hp) .. " / " .. math.floor(max_hp), 16, 1, 1, 1, 1)

    -- Crosshair
    local cx = ui.screen_width() / 2
    local cy = ui.screen_height() / 2
    ui.rect(cx - 1, cy - 8, 2, 16, 1, 1, 1, 0.8)
    ui.rect(cx - 8, cy - 1, 16, 2, 1, 1, 1, 0.8)

    -- Title
    ui.text(20, 20, "COMBAT DEMO", 28, 1, 0.9, 0.3, 1)
    ui.text(20, 52, "LMB: hitscan | E: projectile | walk into spikes = damage", 14, 0.7, 0.7, 0.7, 1)
end

function on_damage(amount, source_id)
    log("Player took " .. amount .. " damage from " .. source_id)
    ui.flash(1, 0, 0, 0.3, 0.15)
end

function on_death()
    log("Player died!")
    game.game_over = true
end
