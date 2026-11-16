pub fn keel_fanout_target() -> u32 { 7 }
pub fn keel_fanout_a() -> u32 { keel_fanout_target() }
pub fn keel_fanout_b() -> u32 { keel_fanout_target() }
pub fn keel_fanout_c() -> u32 { keel_fanout_target() }
pub fn keel_fanout_d() -> u32 { keel_fanout_target() }
pub fn keel_fanout_e() -> u32 { keel_fanout_target() }
pub fn keel_fanout_f() -> u32 { keel_fanout_target() }
pub fn keel_fanout_g() -> u32 { keel_fanout_target() }
pub fn keel_fanout_h() -> u32 { keel_fanout_target() }
pub fn keel_diamond_left() -> u32 { keel_fanout_a() }
pub fn keel_diamond_right() -> u32 { keel_fanout_a() }
pub fn keel_diamond_top() -> u32 { keel_diamond_left() + keel_diamond_right() }
pub fn keel_fanout_decoy() -> &'static str { "keel_fanout_target()" }
