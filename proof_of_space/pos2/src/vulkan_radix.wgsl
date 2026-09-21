struct Configuration {
    count: u32,
    blocks: u32,
    chunks: u32,
    shift: u32,
    final_table: u32,
    padding_first: u32,
    padding_second: u32,
    padding_third: u32,
}

@group(0) @binding(0) var<uniform> configuration: Configuration;
@group(0) @binding(1) var<storage, read> input_first: array<vec4<u32>>;
@group(0) @binding(2) var<storage, read> input_second: array<vec4<u32>>;
@group(0) @binding(3) var<storage, read> input_third: array<vec4<u32>>;
@group(0) @binding(4) var<storage, read> input_fourth: array<vec4<u32>>;
@group(0) @binding(5) var<storage, read> input_fifth: array<vec4<u32>>;
@group(0) @binding(6) var<storage, read_write> output_first: array<vec4<u32>>;
@group(0) @binding(7) var<storage, read_write> output_second: array<vec4<u32>>;
@group(0) @binding(8) var<storage, read_write> output_third: array<vec4<u32>>;
@group(0) @binding(9) var<storage, read_write> output_fourth: array<vec4<u32>>;
@group(0) @binding(10) var<storage, read_write> output_fifth: array<vec4<u32>>;
@group(0) @binding(11) var<storage, read_write> histogram: array<u32>;
@group(0) @binding(12) var<storage, read_write> sums: array<u32>;
@group(0) @binding(13) var<storage, read_write> bins: array<u32>;
@group(0) @binding(14) var<storage, read_write> errors: array<atomic<u32>>;

var<workgroup> group_histogram: array<atomic<u32>, 2048>;
var<workgroup> group_ranks: array<u32, 1024>;
var<workgroup> global_bases: array<u32, 256>;
var<workgroup> local_bases: array<u32, 256>;
var<workgroup> scan_totals: array<u32, 8>;
var<workgroup> sorted_entries: array<vec4<u32>, 1024>;

const ITEMS: u32 = 1024u;
const BINS: u32 = 256u;
const SHARD_MASK: u32 = 0x03ffffffu;

fn load_entry(position: u32) -> vec4<u32> {
    let offset = position & SHARD_MASK;
    switch position >> 26u {
        case 0u: { return input_first[offset]; }
        case 1u: { return input_second[offset]; }
        case 2u: { return input_third[offset]; }
        case 3u: { return input_fourth[offset]; }
        case 4u: { return input_fifth[offset]; }
        default: {
            atomicOr(&errors[0], 4u);
            return vec4<u32>(0u);
        }
    }
    return vec4<u32>(0u);
}

fn store_entry(position: u32, value: vec4<u32>) {
    let offset = position & SHARD_MASK;
    switch position >> 26u {
        case 0u: { output_first[offset] = value; }
        case 1u: { output_second[offset] = value; }
        case 2u: { output_third[offset] = value; }
        case 3u: { output_fourth[offset] = value; }
        case 4u: { output_fifth[offset] = value; }
        default: { atomicOr(&errors[0], 4u); }
    }
}

fn digit(entry: vec4<u32>) -> u32 {
    if configuration.final_table == 0u {
        return (entry.z >> configuration.shift) & 255u;
    }
    if configuration.shift < 32u {
        return (entry.x >> configuration.shift) & 255u;
    }
    return (entry.y >> (configuration.shift - 32u)) & 255u;
}

fn matching_lanes(bucket: u32, valid: bool) -> vec4<u32> {
    var matching = subgroupBallot(valid);
    for (var bit = 0u; bit < 8u; bit += 1u) {
        let selected = (bucket & (1u << bit)) != 0u;
        let lanes = subgroupBallot(selected);
        matching &= select(~lanes, lanes, selected);
    }
    return matching;
}

fn lane_count(lanes: vec4<u32>) -> u32 {
    let counts = countOneBits(lanes);
    return counts.x + counts.y + counts.z + counts.w;
}

fn first_lane(lanes: vec4<u32>) -> u32 {
    if lanes.x != 0u { return firstTrailingBit(lanes.x); }
    if lanes.y != 0u { return 32u + firstTrailingBit(lanes.y); }
    if lanes.z != 0u { return 64u + firstTrailingBit(lanes.z); }
    if lanes.w != 0u { return 96u + firstTrailingBit(lanes.w); }
    return 0u;
}

fn preceding_lanes(lanes: vec4<u32>, lane: u32) -> u32 {
    var total = 0u;
    for (var word = 0u; word < 4u; word += 1u) {
        let start = word * 32u;
        if lane >= start + 32u {
            total += countOneBits(lanes[word]);
        } else if lane > start {
            total += countOneBits(lanes[word] & ((1u << (lane - start)) - 1u));
        }
    }
    return total;
}

fn block_inclusive(value: u32, lane: u32, subgroup: u32, width: u32, subgroups: u32) -> u32 {
    let inclusive = subgroupInclusiveAdd(value);
    if lane + 1u == width {
        scan_totals[subgroup] = inclusive;
    }
    workgroupBarrier();
    var subtotal = 0u;
    if subgroup == 0u && lane < subgroups {
        subtotal = scan_totals[lane];
    }
    let prefix = subgroupInclusiveAdd(subtotal);
    if subgroup == 0u && lane < subgroups {
        scan_totals[lane] = prefix;
    }
    workgroupBarrier();
    var preceding = 0u;
    if subgroup != 0u {
        preceding = scan_totals[subgroup - 1u];
    }
    let result = inclusive + preceding;
    workgroupBarrier();
    return result;
}

@compute @workgroup_size(256)
fn radix_histogram(
    @builtin(workgroup_id) group: vec3<u32>,
    @builtin(num_workgroups) groups: vec3<u32>,
    @builtin(subgroup_invocation_id) lane: u32,
    @builtin(subgroup_id) subgroup: u32,
    @builtin(subgroup_size) width: u32,
    @builtin(num_subgroups) subgroups: u32,
) {
    let block = group.y * groups.x + group.x;
    if block >= configuration.blocks { return; }
    let local = subgroup * width + lane;
    for (var index = local; index < subgroups * BINS; index += 256u) {
        atomicStore(&group_histogram[index], 0u);
    }
    workgroupBarrier();
    for (var iteration = 0u; iteration < 4u; iteration += 1u) {
        let position = block * ITEMS + subgroup * width * 4u + iteration * width + lane;
        let valid = position < configuration.count;
        var entry = vec4<u32>(0u);
        if valid { entry = load_entry(position); }
        let bucket = digit(entry);
        let matching = matching_lanes(bucket, valid);
        if valid && lane == first_lane(matching) {
            atomicAdd(&group_histogram[subgroup * BINS + bucket], lane_count(matching));
        }
        if valid && ((configuration.final_table == 0u && entry.z >= 0x10000000u)
            || (configuration.final_table != 0u && entry.y >= 0x01000000u)) {
            atomicOr(&errors[0], 1u);
        }
    }
    workgroupBarrier();
    var total = 0u;
    for (var index = 0u; index < subgroups; index += 1u) {
        total += atomicLoad(&group_histogram[index * BINS + local]);
    }
    histogram[local * configuration.blocks + block] = total;
}

@compute @workgroup_size(256)
fn radix_prefix_chunks(
    @builtin(workgroup_id) group: vec3<u32>,
    @builtin(subgroup_invocation_id) lane: u32,
    @builtin(subgroup_id) subgroup: u32,
    @builtin(subgroup_size) width: u32,
    @builtin(num_subgroups) subgroups: u32,
) {
    let local = subgroup * width + lane;
    let first = group.x * 1024u + local * 4u;
    let base = group.y * configuration.blocks;
    var values = vec4<u32>(0u);
    for (var offset = 0u; offset < 4u; offset += 1u) {
        if first + offset < configuration.blocks {
            values[offset] = histogram[base + first + offset];
        }
    }
    let total = values.x + values.y + values.z + values.w;
    let inclusive = block_inclusive(total, lane, subgroup, width, subgroups);
    var prefix = inclusive - total;
    for (var offset = 0u; offset < 4u; offset += 1u) {
        if first + offset < configuration.blocks {
            histogram[base + first + offset] = prefix;
        }
        prefix += values[offset];
    }
    if local == 255u {
        sums[group.y * configuration.chunks + group.x] = inclusive;
    }
}

@compute @workgroup_size(256)
fn radix_prefix_totals(
    @builtin(workgroup_id) group: vec3<u32>,
    @builtin(subgroup_invocation_id) lane: u32,
    @builtin(subgroup_id) subgroup: u32,
    @builtin(subgroup_size) width: u32,
    @builtin(num_subgroups) subgroups: u32,
) {
    let local = subgroup * width + lane;
    var start = 0u;
    var carry = 0u;
    while start < configuration.chunks {
        let position = start + local;
        var value = 0u;
        if position < configuration.chunks {
            value = sums[group.x * configuration.chunks + position];
        }
        let inclusive = block_inclusive(value, lane, subgroup, width, subgroups);
        if position < configuration.chunks {
            sums[group.x * configuration.chunks + position] = carry + inclusive - value;
        }
        carry += scan_totals[subgroups - 1u];
        workgroupBarrier();
        start += 256u;
    }
    if local == 0u { bins[group.x] = carry; }
}

@compute @workgroup_size(256)
fn radix_prefix_bins(
    @builtin(subgroup_invocation_id) lane: u32,
    @builtin(subgroup_id) subgroup: u32,
    @builtin(subgroup_size) width: u32,
    @builtin(num_subgroups) subgroups: u32,
) {
    let local = subgroup * width + lane;
    let value = bins[local];
    let inclusive = block_inclusive(value, lane, subgroup, width, subgroups);
    bins[local] = inclusive - value;
}

@compute @workgroup_size(256)
fn radix_scatter(
    @builtin(workgroup_id) group: vec3<u32>,
    @builtin(num_workgroups) groups: vec3<u32>,
    @builtin(subgroup_invocation_id) lane: u32,
    @builtin(subgroup_id) subgroup: u32,
    @builtin(subgroup_size) width: u32,
    @builtin(num_subgroups) subgroups: u32,
) {
    let block = group.y * groups.x + group.x;
    if block >= configuration.blocks { return; }
    let local = subgroup * width + lane;
    let tile_count = min(configuration.count - block * ITEMS, ITEMS);
    for (var index = local; index < subgroups * BINS; index += 256u) {
        atomicStore(&group_histogram[index], 0u);
    }
    global_bases[local] = histogram[local * configuration.blocks + block]
        + sums[local * configuration.chunks + block / 1024u] + bins[local];
    workgroupBarrier();
    for (var iteration = 0u; iteration < 4u; iteration += 1u) {
        let position = subgroup * width * 4u + iteration * width + lane;
        let valid = position < tile_count;
        var entry = vec4<u32>(0u);
        if valid { entry = load_entry(block * ITEMS + position); }
        let bucket = digit(entry);
        let matching = matching_lanes(bucket, valid);
        let leader = first_lane(matching);
        var preceding = 0u;
        if valid && lane == leader {
            preceding = atomicAdd(&group_histogram[subgroup * BINS + bucket], lane_count(matching));
        }
        preceding = subgroupShuffle(preceding, leader);
        if valid { group_ranks[position] = preceding + preceding_lanes(matching, lane); }
    }
    workgroupBarrier();
    var preceding = 0u;
    for (var index = 0u; index < subgroups; index += 1u) {
        let count = atomicLoad(&group_histogram[index * BINS + local]);
        atomicStore(&group_histogram[index * BINS + local], preceding);
        preceding += count;
    }
    let inclusive = block_inclusive(preceding, lane, subgroup, width, subgroups);
    local_bases[local] = inclusive - preceding;
    workgroupBarrier();
    for (var iteration = 0u; iteration < 4u; iteration += 1u) {
        let position = subgroup * width * 4u + iteration * width + lane;
        if position < tile_count {
            let entry = load_entry(block * ITEMS + position);
            let bucket = digit(entry);
            let destination = group_ranks[position]
                + atomicLoad(&group_histogram[subgroup * BINS + bucket]) + local_bases[bucket];
            if destination < tile_count {
                sorted_entries[destination] = entry;
            } else {
                atomicOr(&errors[0], 2u);
            }
        }
    }
    workgroupBarrier();
    for (var iteration = 0u; iteration < 4u; iteration += 1u) {
        let position = iteration * 256u + local;
        if position < tile_count {
            let entry = sorted_entries[position];
            let bucket = digit(entry);
            let destination = global_bases[bucket] + position - local_bases[bucket];
            if destination < configuration.count {
                store_entry(destination, entry);
            } else {
                atomicOr(&errors[0], 2u);
            }
        }
    }
}
