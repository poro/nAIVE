-- TITAN Camera: dramatic orbit matched to genesis-scale scene
function init()
    self.time = 0
    self.total_duration = 60.0
    self.keyframes = {
        {t=0,  x=6,  y=3,   z=6,   pitch=-12, yaw=225},
        {t=6,  x=8,  y=2,   z=0,   pitch=-5,  yaw=90},
        {t=12, x=4,  y=1.2, z=-7,  pitch=5,   yaw=150},
        {t=18, x=0,  y=7,   z=0.1, pitch=-75, yaw=0},
        {t=24, x=-7, y=2.5, z=5,   pitch=-8,  yaw=305},
        {t=30, x=2,  y=2,   z=2,   pitch=-5,  yaw=225},
        {t=36, x=-5, y=4,   z=-6,  pitch=-18, yaw=30},
        {t=42, x=7,  y=1,   z=3,   pitch=8,   yaw=110},
        {t=48, x=0,  y=5,   z=8,   pitch=-15, yaw=180},
        {t=54, x=5,  y=3,   z=6,   pitch=-10, yaw=220},
        {t=60, x=6,  y=3,   z=6,   pitch=-12, yaw=225},
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
    if span > 0 then raw_t = (self.time - a.t) / span end
    local t = raw_t * raw_t * (3 - 2 * raw_t)
    local x = a.x + (b.x - a.x) * t
    local y = a.y + (b.y - a.y) * t
    local z = a.z + (b.z - a.z) * t
    local pitch = a.pitch + (b.pitch - a.pitch) * t
    local yaw = a.yaw + (b.yaw - a.yaw) * t
    entity.set_position(_entity_string_id, x, y, z)
    entity.set_rotation(_entity_string_id, pitch, yaw, 0)
end
