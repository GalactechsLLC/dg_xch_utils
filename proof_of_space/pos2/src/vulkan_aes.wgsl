fn rotate(value: u32, amount: u32) -> u32 {
    return (value << amount) | (value >> (32u - amount));
}

fn aes_column(first: u32, second: u32, third: u32, fourth: u32) -> u32 {
    return aes_table[first & 255u]
        ^ rotate(aes_table[(second >> 8u) & 255u], 8u)
        ^ rotate(aes_table[(third >> 16u) & 255u], 16u)
        ^ rotate(aes_table[fourth >> 24u], 24u);
}

fn aes_round(state: vec4<u32>, key: vec4<u32>) -> vec4<u32> {
    return vec4<u32>(
        aes_column(state.x, state.y, state.z, state.w),
        aes_column(state.y, state.z, state.w, state.x),
        aes_column(state.z, state.w, state.x, state.y),
        aes_column(state.w, state.x, state.y, state.z)
    ) ^ key;
}
