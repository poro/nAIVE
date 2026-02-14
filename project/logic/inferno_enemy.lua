-- Arena enemy: patrols, chases player, attacks, takes damage
function init()
    self.state = "patrol"
    self.health = 40
    self.patrol_speed = 1.8
    self.chase_speed = 3.5
    self.attack_range = 2.0
    self.detect_range = 12.0
    self.damage = 15
    self.attack_cooldown = 0
    self.attack_interval = 1.2
    self.patrol_timer = 0
    self.patrol_dir_x = (math.random() > 0.5) and 1 or -1
    self.patrol_dir_z = (math.random() > 0.5) and 1 or -1
    self.patrol_switch = 2.0 + math.random() * 2.0
    self.dead = false
end

function update(dt)
    if self.dead then return end
    if game.game_over or game.level_complete then return end

    if self.attack_cooldown > 0 then
        self.attack_cooldown = self.attack_cooldown - dt
    end

    local px, _, pz = entity.get_position("player")
    local ex, ey, ez = entity.get_position(_entity_string_id)
    local dx, dz = px - ex, pz - ez
    local dist = math.sqrt(dx * dx + dz * dz)

    if dist < self.attack_range then
        self.state = "attack"
        if self.attack_cooldown <= 0 then
            self.attack_cooldown = self.attack_interval
            game.player_health = game.player_health - self.damage
            events.emit("player.damaged", {amount = self.damage, source = _entity_string_id})
            if game.player_health <= 0 then
                game.game_over = true
                events.emit("player.died", {})
            end
        end
    elseif dist < self.detect_range then
        self.state = "chase"
        local nx, nz = dx / dist, dz / dist
        local spd = self.chase_speed * dt
        local new_x = math.max(-14, math.min(14, ex + nx * spd))
        local new_z = math.max(-14, math.min(14, ez + nz * spd))
        entity.set_position(_entity_string_id, new_x, ey, new_z)
    else
        self.state = "patrol"
        self.patrol_timer = self.patrol_timer + dt
        if self.patrol_timer > self.patrol_switch then
            self.patrol_timer = 0
            self.patrol_dir_x = -self.patrol_dir_x
            if math.random() > 0.5 then self.patrol_dir_z = -self.patrol_dir_z end
        end
        local spd = self.patrol_speed * dt
        local new_x = math.max(-14, math.min(14, ex + spd * self.patrol_dir_x * 0.7))
        local new_z = math.max(-14, math.min(14, ez + spd * self.patrol_dir_z * 0.7))
        entity.set_position(_entity_string_id, new_x, ey, new_z)
    end

    if input.just_pressed("attack") and dist < 3.0 then
        self.health = self.health - 20
        entity.set_emission(_entity_string_id, 3.0, 0.5, 0.0)
        if self.health <= 0 then
            self.dead = true
            entity.set_position(_entity_string_id, 0, -100, 0)
            if game.enemies_alive then
                game.enemies_alive = game.enemies_alive - 1
                game.kills = (game.kills or 0) + 1
            end
            events.emit("enemy.defeated", {enemy_id = _entity_string_id})
        end
    else
        if self.state == "chase" then
            entity.set_emission(_entity_string_id, 1.0, 0.1, 0.0)
        else
            entity.set_emission(_entity_string_id, 0.5, 0.05, 0.0)
        end
    end
end
