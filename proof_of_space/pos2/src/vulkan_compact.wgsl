struct Configuration {
    keys: array<vec4<u32>, 2>,
    fragment_keys: array<vec4<u32>, 4>,
    table: u32,
    testnet: u32,
    start: u32,
    count: u32,
    input_len: u32,
    output_capacity: u32,
    max_pair_evaluations: u32,
    padding: u32,
}

struct Counters {
    output_count: atomic<u32>,
    pair_evaluations: atomic<u32>,
    error_flags: atomic<u32>,
    padding: atomic<u32>,
}

@group(0) @binding(0) var<uniform> configuration: Configuration;
@group(0) @binding(1) var<storage, read> input_first: array<vec4<u32>>;
@group(0) @binding(2) var<storage, read> input_second: array<vec4<u32>>;
@group(0) @binding(3) var<storage, read> input_third: array<vec4<u32>>;
@group(0) @binding(4) var<storage, read> input_fourth: array<vec4<u32>>;
@group(0) @binding(5) var<storage, read> input_fifth: array<vec4<u32>>;
@group(0) @binding(6) var<storage, read_write> output: array<vec4<u32>>;
@group(0) @binding(7) var<storage, read_write> match_index: array<u32>;
@group(0) @binding(8) var<storage, read_write> counters: Counters;
@group(0) @binding(9) var<storage, read> aes_table_source: array<u32, 256>;

var<workgroup> aes_table: array<u32, 256>;
var<workgroup> group_output: array<vec4<u32>, 256>;
var<workgroup> group_output_count: atomic<u32>;
var<workgroup> group_pair_count: atomic<u32>;
var<workgroup> group_allowed: u32;
var<workgroup> group_output_start: u32;
var<workgroup> group_output_length: u32;

const MASK: u32 = 0x0fffffffu;
const INFO_COUNT: u32 = 0x10000000u;
const SHARD_MASK: u32 = 0x03ffffffu;

fn flat_position(group: vec3<u32>, groups: vec3<u32>, local: u32) -> u32 {
    return (group.y * groups.x + group.x) * 64u + local;
}

fn initialize_aes(local: u32) {
    for (var position = local; position < 256u; position += 64u) {
        aes_table[position] = aes_table_source[position];
    }
}

fn hash_value(input: vec4<u32>) -> vec4<u32> {
    var state = input;
    let first_key = configuration.keys[0];
    let second_key = configuration.keys[1];
    for (var round = 0u; round < 16u; round += 1u) {
        state = aes_round(state, first_key);
        state = aes_round(state, second_key);
    }
    return state;
}

fn load_entry(position: u32) -> vec4<u32> {
    let offset = position & SHARD_MASK;
    switch position >> 26u {
        case 0u: { return input_first[offset]; }
        case 1u: { return input_second[offset]; }
        case 2u: { return input_third[offset]; }
        case 3u: { return input_fourth[offset]; }
        case 4u: { return input_fifth[offset]; }
        default: {
            atomicOr(&counters.error_flags, 4u);
            return vec4<u32>(0u);
        }
    }
    return vec4<u32>(0u);
}

fn store_output(position: u32, value: vec4<u32>) {
    output[position] = value;
}

fn reserve_output(amount: u32) -> vec2<u32> {
    if amount == 0u {
        return vec2<u32>(0u, 1u);
    }
    var observed = atomicLoad(&counters.output_count);
    loop {
        if observed > configuration.output_capacity {
            atomicOr(&counters.error_flags, 1u);
            return vec2<u32>(0u);
        }
        if amount > configuration.output_capacity - observed {
            atomicOr(&counters.error_flags, 1u);
            return vec2<u32>(0u);
        }
        let result = atomicCompareExchangeWeak(
            &counters.output_count, observed, observed + amount
        );
        if result.exchanged {
            return vec2<u32>(observed, 1u);
        }
        observed = result.old_value;
    }
    return vec2<u32>(0u);
}

fn reserve_pairs(amount: u32) -> bool {
    if atomicLoad(&counters.error_flags) != 0u {
        return false;
    }
    var observed = atomicLoad(&counters.pair_evaluations);
    loop {
        if observed > configuration.max_pair_evaluations {
            atomicOr(&counters.error_flags, 2u);
            return false;
        }
        if amount > configuration.max_pair_evaluations - observed {
            atomicOr(&counters.error_flags, 2u);
            return false;
        }
        let result = atomicCompareExchangeWeak(
            &counters.pair_evaluations, observed, observed + amount
        );
        if result.exchanged {
            return true;
        }
        observed = result.old_value;
    }
    return false;
}

fn emit(value: vec4<u32>) {
    let position = atomicAdd(&group_output_count, 1u);
    if position < 256u {
        group_output[position] = value;
    } else {
        let reservation = reserve_output(1u);
        if reservation.y != 0u {
            store_output(reservation.x, value);
        }
    }
}

fn rotate_half(value: u32, amount: u32) -> u32 {
    return ((value << amount) & MASK) | (value >> (28u - amount));
}

fn fragment(first_half: u32, second_half: u32) -> vec2<u32> {
    var left = first_half;
    var right = second_half;
    for (var round = 0u; round < 4u; round += 1u) {
        let keys = configuration.fragment_keys[round];
        var first = right;
        var second = keys.x;
        var third = keys.y;
        var fourth = keys.z;
        first = (first + second) & MASK;
        fourth = rotate_half(fourth ^ first, 16u);
        third = (third + fourth) & MASK;
        second = rotate_half(second ^ third, 12u);
        first = (first + second) & MASK;
        fourth = rotate_half(fourth ^ first, 8u);
        third = (third + fourth) & MASK;
        second = rotate_half(second ^ third, 7u);
        let next = (left ^ second) & MASK;
        left = right;
        right = next;
    }
    return vec2<u32>((left << 28u) | right, left >> 4u);
}

@compute @workgroup_size(64)
fn generate(
    @builtin(workgroup_id) group: vec3<u32>,
    @builtin(num_workgroups) groups: vec3<u32>,
    @builtin(local_invocation_index) local: u32,
) {
    initialize_aes(local);
    workgroupBarrier();
    let offset = flat_position(group, groups, local);
    if offset >= configuration.count {
        return;
    }
    if offset >= configuration.output_capacity {
        atomicOr(&counters.error_flags, 1u);
        return;
    }
    let position = configuration.start + offset;
    if position < configuration.start || position >= INFO_COUNT {
        atomicOr(&counters.error_flags, 4u);
        return;
    }
    let input = position ^ select(0u, 0xa3b1c4d7u, configuration.testnet != 0u);
    let hashed = hash_value(vec4<u32>(input, 0u, 0u, 0u));
    store_output(offset, vec4<u32>(position, 0u, hashed.x & MASK, 0u));
}

@compute @workgroup_size(64)
fn build_index(
    @builtin(workgroup_id) group: vec3<u32>,
    @builtin(num_workgroups) groups: vec3<u32>,
    @builtin(local_invocation_index) local: u32,
) {
    let offset = flat_position(group, groups, local);
    if offset >= configuration.count {
        return;
    }
    let position = configuration.start + offset;
    if position < configuration.start || position >= configuration.input_len {
        atomicOr(&counters.error_flags, 4u);
        return;
    }
    let info = load_entry(position).z;
    if info >= INFO_COUNT {
        atomicOr(&counters.error_flags, 4u);
        return;
    }
    var lower = 0u;
    if position != 0u {
        let previous = load_entry(position - 1u).z;
        if previous > info {
            atomicOr(&counters.error_flags, 4u);
            return;
        }
        lower = previous + 1u;
    }
    if lower <= info {
        if info - lower >= 4096u {
            atomicOr(&counters.error_flags, 4u);
            return;
        }
        for (var value = lower; value <= info; value += 1u) {
            match_index[value] = position;
        }
    }
    if position + 1u == configuration.input_len {
        if INFO_COUNT - info > 4096u {
            atomicOr(&counters.error_flags, 4u);
            return;
        }
        for (var value = info + 1u; value <= INFO_COUNT; value += 1u) {
            match_index[value] = configuration.input_len;
        }
    }
}

@compute @workgroup_size(64)
fn match_table(
    @builtin(workgroup_id) group: vec3<u32>,
    @builtin(num_workgroups) groups: vec3<u32>,
    @builtin(local_invocation_index) local: u32,
) {
    initialize_aes(local);
    if local == 0u {
        atomicStore(&group_output_count, 0u);
        atomicStore(&group_pair_count, 0u);
        group_allowed = 0u;
        group_output_start = 0u;
        group_output_length = 0u;
    }
    workgroupBarrier();
    let offset = flat_position(group, groups, local);
    let position = configuration.start + offset;
    var left = vec4<u32>(0u);
    var ranges: array<vec2<u32>, 4>;
    var match_infos: array<u32, 4>;
    var pair_count = 0u;
    let is_active = offset < configuration.count;
    if is_active {
        if position < configuration.start || position >= configuration.input_len
            || configuration.table < 1u || configuration.table > 3u {
            atomicOr(&counters.error_flags, 4u);
        } else {
            left = load_entry(position);
            let section = left.z >> 26u;
            let rotated = (section << 1u) | (section >> 1u);
            let bumped = (rotated + 1u) & 3u;
            let partner = ((bumped >> 1u) | (bumped << 1u)) & 3u;
            for (var key = 0u; key < 4u; key += 1u) {
                let hashed = hash_value(vec4<u32>(configuration.table, key, left.x, left.y));
                let wanted_info = (partner << 26u) | (key << 24u) | (hashed.x & 0x00ffffffu);
                let first = match_index[wanted_info];
                let last = match_index[wanted_info + 1u];
                if first > last || last > configuration.input_len {
                    atomicOr(&counters.error_flags, 4u);
                } else if last - first > 64u {
                    atomicOr(&counters.error_flags, 8u);
                } else {
                    ranges[key] = vec2<u32>(first, last);
                    match_infos[key] = wanted_info;
                    pair_count += last - first;
                }
            }
        }
    }
    atomicAdd(&group_pair_count, pair_count);
    workgroupBarrier();
    if local == 0u {
        group_allowed = select(0u, 1u, reserve_pairs(atomicLoad(&group_pair_count)));
    }
    workgroupBarrier();
    if is_active && group_allowed != 0u {
        for (var key = 0u; key < 4u; key += 1u) {
            for (var candidate = ranges[key].x; candidate < ranges[key].y; candidate += 1u) {
                let right = load_entry(candidate);
                if right.z != match_infos[key] {
                    atomicOr(&counters.error_flags, 4u);
                    continue;
                }
                let hashed = hash_value(vec4<u32>(left.x, left.y, right.x, right.y));
                if (hashed.w & 3u) != 0u {
                    continue;
                }
                var result = vec4<u32>(hashed.y, hashed.z & 0x00ffffffu, hashed.x & MASK, 0u);
                if configuration.table == 1u {
                    result.x = (left.x << 28u) | right.x;
                    result.y = left.x >> 4u;
                } else {
                    result.w = ((left.y >> 10u) << 14u) | (right.y >> 10u);
                    if configuration.table == 3u {
                        let encoded = fragment(left.w, right.w);
                        result.x = encoded.x;
                        result.y = encoded.y;
                    }
                }
                emit(result);
            }
        }
    }
    workgroupBarrier();
    if local == 0u {
        let length = min(atomicLoad(&group_output_count), 256u);
        let reservation = reserve_output(length);
        if reservation.y != 0u {
            group_output_start = reservation.x;
            group_output_length = length;
        }
    }
    workgroupBarrier();
    for (var index = local; index < group_output_length; index += 64u) {
        store_output(group_output_start + index, group_output[index]);
    }
}
