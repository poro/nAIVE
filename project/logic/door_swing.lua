-- Door that swings open when the player presses interact nearby

function init()
    self.is_open = false
    self.current_yaw = 0
    self.target_yaw = 0
    self.open_speed = 120 -- degrees per second
    self.interact_range = 3.0
    log("Gate ready")
end

function update(dt)
    if not self.is_open then
        local px, py, pz = entity.get_position("player")
        local gx, gy, gz = entity.get_position("gate")
        local dx, dz = px - gx, pz - gz
        local dist = math.sqrt(dx * dx + dz * dz)

        if dist < self.interact_range and input.just_pressed("interact") then
            self.is_open = true
            self.target_yaw = 90
            events.emit("door.opened", {door_id = "gate"})
            log("Gate opened!")
        end
    end

    -- Animate swing
    if self.current_yaw < self.target_yaw then
        self.current_yaw = math.min(self.current_yaw + self.open_speed * dt, self.target_yaw)
        entity.set_rotation("gate", 0, self.current_yaw, 0)
    end
end
