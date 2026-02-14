-- GENESIS Camera: dramatic sweeping orbits, always moving

function init()
    self.time = 0
    self.total_duration = 40.0

    self.keyframes = {
        -- Start: medium shot seeing the whole scene
        {t=0,    x=6,   y=3,   z=6,    pitch=-12, yaw=225},
        -- Sweep around, descending
        {t=5,    x=8,   y=2,   z=0,    pitch=-8,  yaw=90},
        -- Low dramatic angle
        {t=10,   x=4,   y=1.2, z=-6,   pitch=5,   yaw=145},
        -- High overhead
        {t=14,   x=0,   y=7,   z=0.1,  pitch=-75, yaw=0},
        -- Fast sweep to opposite side
        {t=18,   x=-7,  y=2.5, z=5,    pitch=-10, yaw=305},
        -- Close to the spark
        {t=22,   x=2,   y=2,   z=2,    pitch=-5,  yaw=225},
        -- Pull back wide
        {t=26,   x=-5,  y=4,   z=-8,   pitch=-15, yaw=30},
        -- Low hero shot
        {t=30,   x=8,   y=0.8, z=3,    pitch=10,  yaw=110},
        -- Back to start position for loop
        {t=36,   x=5,   y=3,   z=7,    pitch=-12, yaw=220},
        {t=40,   x=6,   y=3,   z=6,    pitch=-12, yaw=225},
    }
end

function update(dt)
    self.time = self.time + dt
    if self.time >= self.total_duration then
        self.time = self.time - self.total_duration
    end

    local kf = self.keyframes
    local a, b = kf[1], kf[2]
    for i = 1, #kf - 1 do
        if self.time >= kf[i].t and self.time < kf[i+1].t then
            a = kf[i]
            b = kf[i+1]
            break
        end
    end

    local span = b.t - a.t
    local raw_t = 0
    if span > 0 then
        raw_t = (self.time - a.t) / span
    end
    local t = raw_t * raw_t * (3 - 2 * raw_t)

    local x = a.x + (b.x - a.x) * t
    local y = a.y + (b.y - a.y) * t
    local z = a.z + (b.z - a.z) * t
    local pitch = a.pitch + (b.pitch - a.pitch) * t
    local yaw = a.yaw + (b.yaw - a.yaw) * t

    entity.set_position(_entity_string_id, x, y, z)
    entity.set_rotation(_entity_string_id, pitch, yaw, 0)
end
