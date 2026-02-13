-- Guardian goblin: patrols, detects player, chases, attacks

function init()
    self.state = "patrol"
    self.health = 50
    self.patrol_speed = 1.5
    self.chase_speed = 3.0
    self.attack_range = 1.8
    self.detect_range = 8.0
    self.damage = 20
    self.attack_cooldown = 0
    self.attack_interval = 1.5
    self.patrol_timer = 0
    self.patrol_dir = 1
    log("Guardian spawned - 50 HP")
end

function update(dt)
    if game.game_over or game.level_complete then return end
    if self.health <= 0 then return end

    if self.attack_cooldown > 0 then
        self.attack_cooldown = self.attack_cooldown - dt
    end

    local px, _, pz = entity.get_position("player")
    local gx, gy, gz = entity.get_position("guardian")
    local dx, dz = px - gx, pz - gz
    local dist = math.sqrt(dx * dx + dz * dz)

    if dist < self.attack_range then
        -- Melee attack
        self.state = "attack"
        if self.attack_cooldown <= 0 then
            self.attack_cooldown = self.attack_interval
            game.player_health = game.player_health - self.damage
            events.emit("player.damaged", {amount = self.damage, source = "guardian"})
            log("Guardian strikes! Player HP: " .. game.player_health)
            if game.player_health <= 0 then
                game.game_over = true
                events.emit("player.died", {})
                log("Player defeated by guardian!")
            end
        end
    elseif dist < self.detect_range then
        -- Chase
        self.state = "chase"
        local nx, nz = dx / dist, dz / dist
        local spd = self.chase_speed * dt
        entity.set_position("guardian", gx + nx * spd, gy, gz + nz * spd)
    else
        -- Patrol side to side
        self.state = "patrol"
        self.patrol_timer = self.patrol_timer + dt
        if self.patrol_timer > 3.0 then
            self.patrol_timer = 0
            self.patrol_dir = -self.patrol_dir
        end
        local spd = self.patrol_speed * dt * self.patrol_dir
        entity.set_position("guardian", gx + spd, gy, gz)
    end

    -- Take damage from player attack
    if input.just_pressed("attack") and dist < 2.5 then
        self.health = self.health - 25
        log("Guardian hit! HP: " .. self.health)
        if self.health <= 0 then
            events.emit("enemy.defeated", {enemy_id = "guardian"})
            entity.set_position("guardian", 0, -100, 0)
            log("Guardian defeated!")
        end
    end
end
