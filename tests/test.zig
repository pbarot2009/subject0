// Zig Syntax Test
const std = @import("std");

pub const Config = struct {
    buffer_capacity: usize = 4096,
    auto_save: bool = true,
};

pub fn init_buffer(cfg: Config) !usize {
    if (cfg.buffer_capacity == 0) return error.InvalidCapacity;
    return cfg.buffer_capacity * 2;
}
