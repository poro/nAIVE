-- Turntable rotation + scroll-wheel zoom for 3D model showcase
function init()
    self.angle = 0
    self.speed = 25  -- degrees per second
    self.zoom = 3.0  -- camera distance from origin
    self.zoom_min = 0.5
    self.zoom_max = 15.0
    self.cam_height = 1.0
end

function update(dt)
    -- Rotate the model
    self.angle = self.angle + self.speed * dt
    entity.set_rotation(_entity_string_id, 0, self.angle, 0)

    -- Scroll-wheel zoom: move camera along its forward axis
    local sx, sy = input.scroll_delta()
    if sy ~= 0 then
        self.zoom = self.zoom - sy * 0.5
        if self.zoom < self.zoom_min then self.zoom = self.zoom_min end
        if self.zoom > self.zoom_max then self.zoom = self.zoom_max end
    end

    -- Position camera at orbit distance looking at model
    entity.set_position("player", 0, self.cam_height, self.zoom)

    ui.text(20, 20, "3D Asset Turntable", 24, 1, 1, 1, 1)
    ui.text(20, 50, "Scroll to zoom | WASD to walk", 14, 0.7, 0.7, 0.7, 1)
end
