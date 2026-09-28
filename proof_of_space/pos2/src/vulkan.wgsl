struct Configuration {
    keys: array<vec4<u32>, 2>,
    rounds: u32,
    count: u32,
    padding_first: u32,
    padding_second: u32,
}

@group(0) @binding(0) var<uniform> configuration: Configuration;
@group(0) @binding(1) var<storage, read> inputs: array<vec4<u32>>;
@group(0) @binding(2) var<storage, read_write> outputs: array<vec4<u32>>;
@group(0) @binding(3) var<storage, read> aes_table_source: array<u32, 256>;
var<workgroup> aes_table: array<u32, 256>;

@compute @workgroup_size(64)
fn hash(
    @builtin(global_invocation_id) invocation: vec3<u32>,
    @builtin(local_invocation_index) local_index: u32,
) {
    for (var table_index = local_index; table_index < 256u; table_index += 64u) {
        aes_table[table_index] = aes_table_source[table_index];
    }
    workgroupBarrier();
    let position = invocation.x;
    if position >= configuration.count {
        return;
    }
    var state = inputs[position];
    let first_key = configuration.keys[0];
    let second_key = configuration.keys[1];
    for (var round = 0u; round < configuration.rounds; round += 1u) {
        state = aes_round(state, first_key);
        state = aes_round(state, second_key);
    }
    outputs[position] = state;
}
