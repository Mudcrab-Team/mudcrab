// DDS -> UASTC 4x4 block transcoder (compute shader).
//
// Batching: one dispatch encodes the 4x4 blocks of many unrelated images at
// once (every mip and face of many textures). `images` describes where each
// image's bytes live in the shared `source` buffer and where its blocks go in
// the shared `blocks` output; every invocation handles exactly one block and
// finds its image by binary search, so all blocks of the batch run in parallel.
//
// Decoding: `source` holds 8-bit texels as stored in the DDS (BGRA/BGRX/BGR)
// or as RGBA decoded on the CPU. Each invocation reads its own 4x4 block.
//
// Bitstream layout follows basisu's unpack_uastc() (basisu_transcoder.cpp):
// LSB-first, [mode code][hint bits][ccs][endpoints][weights]; the anchor
// weight of each plane has its MSB dropped. Trit endpoints are stored the
// UASTC way: all base-3 bundles first (plain base-3 numbers, 5 digits per
// bundle), then every value's low bits. Endpoint fitting follows the MIT
// ComputeASTC encoder (github.com/niepp/astc_encoder: PCA axis + projection)
// plus optional least-squares refinement.
//
// Every block tries each mode that fits it (see the mode choice in main):
//   opaque: 18 (RGB 5b ep / 5b w), 1 (RGB 8b ep / 2b w), 0 (RGB trit ep / 4b w),
//           2 (2 subsets, 4b ep / 3b w), 3 (3 subsets, trit ep / 2b w),
//           4 (2 subsets, quint ep / 2b w)
//   alpha:  14 (RGBA 8b / 2b w), 10 (RGBA trit / 4b w), 12 (RGBA trit / 3b w),
//           11 (RGBA dual plane: alpha has its own 2b weights),
//           9 (RGBA 2 subsets, 4b ep / 2b w)
//   solid:  8
// Multi-subset modes first pick a partition with a cheap estimate over all
// common UASTC patterns, then fit each subset.
// Hint bits (only used when transcoding to ETC1/ETC2/BC1, never for BC7/ASTC)
// are written as neutral values.

struct ImageDesc {
    // Byte offset of the image in `source`.
    source_offset: u32,
    width: u32,
    height: u32,
    blocks_x: u32,
    first_block: u32,
    // FMT_* below (texture_gpu::SourceFormat).
    format: u32,
    // Bytes per texel row, per-texel formats only.
    pitch: u32,
    pad: u32,
};

const FMT_RGBA8: u32 = 0u;
const FMT_BGRA8: u32 = 1u;
const FMT_BGRX8: u32 = 2u;
const FMT_BGR8: u32 = 3u;

struct Params {
    image_count: u32,
    // One past the last block of this dispatch.
    block_end: u32,
    // 0 = fastest. Each step adds one least-squares endpoint refinement pass.
    quality: u32,
    // First block of this dispatch: a batch is encoded in several
    // dispatches so no single one runs long enough to trip a GPU watchdog.
    block_base: u32,
};

@group(0) @binding(0) var<storage, read> source: array<u32>;
@group(0) @binding(1) var<storage, read> images: array<ImageDesc>;
@group(0) @binding(2) var<uniform> params: Params;
@group(0) @binding(3) var<storage, read_write> blocks: array<vec4<u32>>;
// Trit range tables from astc_tables.rs: per range, 256 quantize entries
// followed by its unquantize entries.
@group(0) @binding(4) var<storage, read> tables: array<u32>;
// Per image: set when any texel's alpha is not 255 (picks the KTX2 RGB/RGBA
// channel layout, like the CPU path does from the first image).
@group(0) @binding(5) var<storage, read_write> alpha_flags: array<atomic<u32>>;

const WORKGROUP_SIZE: u32 = 64u;

// ASTC endpoint ranges used by UASTC.
const R7: u32 = 7u;   // 12 levels, trit + 2 bits
const R8: u32 = 8u;   // 16 levels, 4 bits
const R11: u32 = 11u; // 32 levels, 5 bits
const R12: u32 = 12u; // 40 levels, quint + 3 bits
const R13: u32 = 13u; // 48 levels, trit + 4 bits
const R19: u32 = 19u; // 192 levels, trit + 6 bits
const R20: u32 = 20u; // 256 levels, 8 bits
// Offsets in `tables` (astc_tables::BISE_RANGES order: 13, 19, 7, 12).
const R13_QUANT: u32 = 0u;
const R13_UNQUANT: u32 = 256u;
const R19_QUANT: u32 = 304u;
const R19_UNQUANT: u32 = 560u;
const R7_QUANT: u32 = 752u;
const R7_UNQUANT: u32 = 1008u;
const R12_QUANT: u32 = 1020u;
const R12_UNQUANT: u32 = 1276u;

const MASK_RGB: vec4<f32> = vec4<f32>(1.0, 1.0, 1.0, 0.0);
const MASK_RGBA: vec4<f32> = vec4<f32>(1.0, 1.0, 1.0, 1.0);
const MASK_A: vec4<f32> = vec4<f32>(0.0, 0.0, 0.0, 1.0);
const ALL_TEXELS: u32 = 0xFFFFu;

// ------------------------------------------------------------------ modes

struct Mode {
    // UASTC mode number.
    id: u32,
    code: u32,
    code_bits: u32,
    comps: u32,
    range: u32,
    w_bits: u32,
    subsets: u32,
    dual: bool,
    // Hint fields present in this mode (basisu g_uastc_mode_has_*).
    bc1_hint1: bool,
    etc1_bias: bool,
    etc2_alpha: bool,
};

fn mode(id: u32, code: u32, code_bits: u32, comps: u32, range: u32, w_bits: u32, subsets: u32,
        dual: bool, bc1_hint1: bool, etc1_bias: bool, etc2_alpha: bool) -> Mode {
    return Mode(id, code, code_bits, comps, range, w_bits, subsets, dual, bc1_hint1, etc1_bias,
                etc2_alpha);
}

fn mode0() -> Mode { return mode(0u, 0x1u, 4u, 3u, R19, 4u, 1u, false, true, true, false); }
fn mode1() -> Mode { return mode(1u, 0x35u, 6u, 3u, R20, 2u, 1u, false, true, true, false); }
fn mode2() -> Mode { return mode(2u, 0x1Du, 5u, 3u, R8, 3u, 2u, false, true, true, false); }
fn mode3() -> Mode { return mode(3u, 0x3u, 5u, 3u, R7, 2u, 3u, false, true, true, false); }
fn mode4() -> Mode { return mode(4u, 0x13u, 5u, 3u, R12, 2u, 2u, false, true, true, false); }
fn mode9() -> Mode { return mode(9u, 0xFu, 5u, 4u, R8, 2u, 2u, false, true, true, true); }
fn mode10() -> Mode { return mode(10u, 0x2u, 3u, 4u, R13, 4u, 1u, false, false, false, true); }
fn mode11() -> Mode { return mode(11u, 0x0u, 2u, 4u, R13, 2u, 1u, true, false, false, true); }
fn mode12() -> Mode { return mode(12u, 0x6u, 3u, 4u, R19, 3u, 1u, false, false, false, true); }
fn mode14() -> Mode { return mode(14u, 0xDu, 5u, 4u, R20, 2u, 1u, false, true, true, true); }
fn mode18() -> Mode { return mode(18u, 0x9u, 4u, 3u, R11, 5u, 1u, false, true, true, false); }

// ------------------------------------------------------------- partitions
//
// The UASTC multi-subset partitions (basisu g_astc_bc7_patterns2/3 and
// their anchors), 2 bits per texel with texel 0 in the lowest bits. The
// anchor of each subset is its first texel; anchors are packed a byte each.

const PATTERNS2: array<u32, 30> = array<u32, 30>(
    0x50505050u, 0x40404040u, 0x01010101u, 0x54505040u, 0x05151555u, 0x55545450u,
    0x00010515u, 0x01051555u, 0x50400000u, 0x00000105u, 0x55544000u, 0x01155555u,
    0x00000115u, 0x00005555u, 0x55555500u, 0x00555555u, 0x55551501u, 0x40545555u,
    0x00405054u, 0x00004050u, 0x15050100u, 0x50545555u, 0x15050501u, 0x00404050u,
    0x50545455u, 0x14141414u, 0x55000055u, 0x11111111u, 0x00550055u, 0x05145041u);

const ANCHORS2: array<u32, 30> = array<u32, 30>(
    0x00000200u, 0x00000300u, 0x00000001u, 0x00000300u, 0x00000007u, 0x00000200u,
    0x00000003u, 0x00000007u, 0x00000B00u, 0x00000002u, 0x00000700u, 0x0000000Bu,
    0x00000003u, 0x00000008u, 0x00000400u, 0x0000000Cu, 0x00000001u, 0x00000008u,
    0x00000100u, 0x00000200u, 0x00000400u, 0x00000008u, 0x00000001u, 0x00000200u,
    0x00000004u, 0x00000100u, 0x00000004u, 0x00000001u, 0x00000004u, 0x00000001u);

const PATTERNS3: array<u32, 11> = array<u32, 11>(
    0xA5A50000u, 0xAA005555u, 0xAA000055u, 0x0000AA55u, 0x25252525u, 0x94949494u,
    0x58585858u, 0x56560202u, 0x92929292u, 0x55AA0055u, 0xA05050A0u);

const ANCHORS3: array<u32, 11> = array<u32, 11>(
    0x000A0800u, 0x000C0008u, 0x000C0004u, 0x00040008u, 0x00020003u, 0x00030100u,
    0x00010200u, 0x00000901u, 0x00000201u, 0x00080004u, 0x00020600u);

fn pattern_count(subsets: u32) -> u32 {
    return select(select(1u, 30u, subsets == 2u), 11u, subsets == 3u);
}

fn pattern_bits(subsets: u32) -> u32 {
    return select(select(0u, 5u, subsets == 2u), 4u, subsets == 3u);
}

fn pattern_word(subsets: u32, index: u32) -> u32 {
    if (subsets == 2u) {
        return PATTERNS2[index];
    }
    if (subsets == 3u) {
        return PATTERNS3[index];
    }
    return 0u;
}

fn anchor(subsets: u32, index: u32, subset: u32) -> u32 {
    if (subsets == 2u) {
        return (ANCHORS2[index] >> (8u * subset)) & 255u;
    }
    if (subsets == 3u) {
        return (ANCHORS3[index] >> (8u * subset)) & 255u;
    }
    return 0u;
}

// Texels (bitmask) of `subset` in a pattern.
fn members(word: u32, subset: u32) -> u32 {
    var mask = 0u;
    for (var i = 0u; i < 16u; i++) {
        if (((word >> (2u * i)) & 3u) == subset) {
            mask |= 1u << i;
        }
    }
    return mask;
}

// ------------------------------------------------------------------ bits

struct Bits {
    w: array<u32, 4>,
    pos: u32,
};

fn put(b: ptr<function, Bits>, value: u32, count: u32) {
    if (count == 0u) {
        return;
    }
    let v = value & ((1u << count) - 1u);
    let word = (*b).pos >> 5u;
    let bit = (*b).pos & 31u;
    (*b).w[word] |= v << bit;
    if (bit + count > 32u) {
        (*b).w[word + 1u] |= v >> (32u - bit);
    }
    (*b).pos += count;
}

fn finish(b: Bits) -> vec4<u32> {
    return vec4<u32>(b.w[0], b.w[1], b.w[2], b.w[3]);
}

// ------------------------------------------------------- quantization

// ASTC weight unquantization to 0..64 for bit-only weight ranges.
fn weight_unquant(q: u32, bits: u32) -> u32 {
    var v6: u32;
    switch (bits) {
        case 2u: { v6 = (q << 4u) | (q << 2u) | q; }
        case 3u: { v6 = (q << 3u) | q; }
        case 4u: { v6 = (q << 2u) | (q >> 2u); }
        default: { v6 = (q << 1u) | (q >> 4u); } // 5 bits
    }
    if (v6 > 32u) {
        v6 += 1u;
    }
    return v6;
}

// Low bits per BISE value (the trit/quint digit sits above them).
fn range_bits(range: u32) -> u32 {
    switch (range) {
        case 7u: { return 2u; }
        case 8u: { return 4u; }
        case 11u: { return 5u; }
        case 12u: { return 3u; }
        case 13u: { return 4u; }
        case 19u: { return 6u; }
        default: { return 8u; }
    }
}

// BISE value -> 0..255, exactly as the transcoder decodes it.
fn endpoint_unquant(q: u32, range: u32) -> u32 {
    switch (range) {
        case 7u: { return tables[R7_UNQUANT + q]; }
        case 8u: { return (q << 4u) | q; }
        case 11u: { return (q << 3u) | (q >> 2u); }
        case 12u: { return tables[R12_UNQUANT + q]; }
        case 13u: { return tables[R13_UNQUANT + q]; }
        case 19u: { return tables[R19_UNQUANT + q]; }
        default: { return q; }
    }
}

// Nearest code of a bit-only range (bit replication), checking neighbors.
fn quantize_bits(c: u32, range: u32, top: u32) -> u32 {
    let guess = min(u32(round(f32(c) * f32(top) / 255.0)), top);
    var best = guess;
    var best_err = abs(i32(endpoint_unquant(guess, range)) - i32(c));
    let lo = select(guess - 1u, 0u, guess == 0u);
    let hi = min(guess + 1u, top);
    for (var q = lo; q <= hi; q++) {
        let err = abs(i32(endpoint_unquant(q, range)) - i32(c));
        if (err < best_err) {
            best_err = err;
            best = q;
        }
    }
    return best;
}

fn quantize_endpoint(v: f32, range: u32) -> u32 {
    let c = u32(round(clamp(v, 0.0, 255.0)));
    switch (range) {
        case 7u: { return tables[R7_QUANT + c]; }
        case 8u: { return quantize_bits(c, R8, 15u); }
        case 11u: { return quantize_bits(c, R11, 31u); }
        case 12u: { return tables[R12_QUANT + c]; }
        case 13u: { return tables[R13_QUANT + c]; }
        case 19u: { return tables[R19_QUANT + c]; }
        default: { return c; }
    }
}

// ASTC LDR interpolation (non-sRGB decode mode), weight in 0..64.
fn interpolate(l: u32, h: u32, w: u32) -> f32 {
    let l16 = l * 257u;
    let h16 = h * 257u;
    return f32(((l16 * (64u - w) + h16 * w + 32u) >> 6u) >> 8u);
}

// ------------------------------------------------------- block encode

struct Candidate {
    err: f32,
    // Partition pattern index (multi-subset modes).
    pattern: u32,
    // BISE endpoints; subset s, channel ch: e[(s * comps + ch) * 2 + {0 low, 1 high}].
    e: array<u32, 18>,
    // Quantized weights, raster order; dual plane interleaves P0 P1 per texel.
    w: array<u32, 32>,
};

// One line fit: the `mask` channels of the `members` texels of one subset,
// with weights at w[i * stride + offset].
struct Plane {
    mask: vec4<f32>,
    comps: u32,
    range: u32,
    w_bits: u32,
    stride: u32,
    offset: u32,
    members: u32,
    subset: u32,
};

fn endpoint_index(p: Plane, ch: u32) -> u32 {
    return (p.subset * p.comps + ch) * 2u;
}

fn unquant_endpoints(c: ptr<function, Candidate>, p: Plane) -> array<vec4<f32>, 2> {
    var e: array<vec4<f32>, 2>;
    for (var ch = 0u; ch < 4u; ch++) {
        if (p.mask[ch] > 0.0) {
            let k = endpoint_index(p, ch);
            e[0][ch] = f32(endpoint_unquant((*c).e[k], p.range));
            e[1][ch] = f32(endpoint_unquant((*c).e[k + 1u], p.range));
        }
    }
    return e;
}

// Squared error of texel i over the plane's channels for weight level q.
fn texel_error(px: ptr<function, array<vec4<f32>, 16>>, e: array<vec4<f32>, 2>, i: u32, q: u32,
               p: Plane) -> f32 {
    let w = weight_unquant(q, p.w_bits);
    var err = 0.0;
    for (var ch = 0u; ch < 4u; ch++) {
        if (p.mask[ch] > 0.0) {
            let d = interpolate(u32(e[0][ch]), u32(e[1][ch]), w) - (*px)[i][ch];
            err += d * d;
        }
    }
    return err;
}

// Picks each member texel's weight for fixed endpoints (nearest of three
// levels around the projection); returns the plane's error.
fn fit_weights(px: ptr<function, array<vec4<f32>, 16>>, c: ptr<function, Candidate>, p: Plane) -> f32 {
    let e = unquant_endpoints(c, p);
    let axis = (e[1] - e[0]) * p.mask;
    let len2 = dot(axis, axis);
    let levels = (1u << p.w_bits) - 1u;
    var total = 0.0;
    for (var i = 0u; i < 16u; i++) {
        if (((p.members >> i) & 1u) == 0u) {
            continue;
        }
        var t = 0.0;
        if (len2 > 0.0) {
            t = clamp(dot(((*px)[i] - e[0]) * p.mask, axis) / len2, 0.0, 1.0);
        }
        let guess = u32(round(t * f32(levels)));
        let lo = select(guess - 1u, 0u, guess == 0u);
        let hi = min(guess + 1u, levels);
        var best_q = guess;
        var best_err = 3.4e38;
        for (var q = lo; q <= hi; q++) {
            let err = texel_error(px, e, i, q, p);
            if (err < best_err) {
                best_err = err;
                best_q = q;
            }
        }
        (*c).w[i * p.stride + p.offset] = best_q;
        total += best_err;
    }
    return total;
}

fn set_endpoints(c: ptr<function, Candidate>, e0: vec4<f32>, e1: vec4<f32>, p: Plane) {
    for (var ch = 0u; ch < 4u; ch++) {
        if (p.mask[ch] > 0.0) {
            let k = endpoint_index(p, ch);
            (*c).e[k] = quantize_endpoint(e0[ch], p.range);
            (*c).e[k + 1u] = quantize_endpoint(e1[ch], p.range);
        }
    }
}

// Fits one plane: PCA line over its texels, weight fit, then
// `params.quality` passes of least-squares endpoint refinement.
fn fit_plane(px: ptr<function, array<vec4<f32>, 16>>, c: ptr<function, Candidate>, p: Plane) -> f32 {
    let count = f32(countOneBits(p.members));
    var mean = vec4<f32>(0.0);
    for (var i = 0u; i < 16u; i++) {
        if (((p.members >> i) & 1u) != 0u) {
            mean += (*px)[i] * p.mask;
        }
    }
    mean /= count;

    var cov = mat4x4<f32>(vec4<f32>(0.0), vec4<f32>(0.0), vec4<f32>(0.0), vec4<f32>(0.0));
    for (var i = 0u; i < 16u; i++) {
        if (((p.members >> i) & 1u) != 0u) {
            let d = ((*px)[i] - mean) * p.mask;
            cov[0] += d * d.x;
            cov[1] += d * d.y;
            cov[2] += d * d.z;
            cov[3] += d * d.w;
        }
    }
    // Power iteration for the dominant axis.
    var axis = normalize(vec4<f32>(0.57735, 0.57735, 0.57735, 0.5) * p.mask);
    for (var it = 0u; it < 8u; it++) {
        let next = cov * axis;
        let len = length(next);
        if (len < 1e-5) {
            break;
        }
        axis = next / len;
    }

    var tmin = 3.4e38;
    var tmax = -3.4e38;
    for (var i = 0u; i < 16u; i++) {
        if (((p.members >> i) & 1u) != 0u) {
            let t = dot(((*px)[i] - mean) * p.mask, axis);
            tmin = min(tmin, t);
            tmax = max(tmax, t);
        }
    }
    set_endpoints(c, mean + axis * tmin, mean + axis * tmax, p);
    var err = fit_weights(px, c, p);

    for (var refine = 0u; refine < params.quality; refine++) {
        // Least squares for the endpoints given the chosen weights.
        var a = 0.0;
        var b = 0.0;
        var d = 0.0;
        var x = vec4<f32>(0.0);
        var y = vec4<f32>(0.0);
        for (var i = 0u; i < 16u; i++) {
            if (((p.members >> i) & 1u) == 0u) {
                continue;
            }
            let w = f32(weight_unquant((*c).w[i * p.stride + p.offset], p.w_bits)) / 64.0;
            let iw = 1.0 - w;
            a += iw * iw;
            b += iw * w;
            d += w * w;
            x += iw * (*px)[i];
            y += w * (*px)[i];
        }
        let det = a * d - b * b;
        if (abs(det) < 1e-6) {
            break;
        }
        var trial = *c;
        set_endpoints(&trial, (d * x - b * y) / det, (a * y - b * x) / det, p);
        let trial_err = fit_weights(px, &trial, p);
        if (trial_err >= err) {
            break;
        }
        *c = trial;
        err = trial_err;
    }
    return err;
}

fn plane(mask: vec4<f32>, m: Mode, stride: u32, offset: u32, members: u32, subset: u32) -> Plane {
    return Plane(mask, m.comps, m.range, m.w_bits, stride, offset, members, subset);
}

fn encode(px: ptr<function, array<vec4<f32>, 16>>, m: Mode, pattern: u32) -> Candidate {
    var c: Candidate;
    c.pattern = pattern;
    if (m.dual) {
        // Plane 0 carries RGB, plane 1 alpha (ccs = 3).
        c.err = fit_plane(px, &c, plane(MASK_RGB, m, 2u, 0u, ALL_TEXELS, 0u))
              + fit_plane(px, &c, plane(MASK_A, m, 2u, 1u, ALL_TEXELS, 0u));
        return c;
    }
    let mask = select(MASK_RGB, MASK_RGBA, m.comps == 4u);
    let word = pattern_word(m.subsets, pattern);
    c.err = 0.0;
    for (var s = 0u; s < m.subsets; s++) {
        c.err += fit_plane(px, &c, plane(mask, m, 1u, 0u, members(word, s), s));
    }
    return c;
}

// Cheap pattern estimate (basisu does the same before encoding): per subset,
// a line through its bounding box, members snapped to the mode's weight
// levels, no endpoint quantization. Returns the best pattern index.
fn best_pattern(px: ptr<function, array<vec4<f32>, 16>>, m: Mode) -> u32 {
    let mask = select(MASK_RGB, MASK_RGBA, m.comps == 4u);
    let levels = f32((1u << m.w_bits) - 1u);
    var best = 0u;
    var best_err = 3.4e38;
    for (var index = 0u; index < pattern_count(m.subsets); index++) {
        let word = pattern_word(m.subsets, index);
        var err = 0.0;
        for (var s = 0u; s < m.subsets; s++) {
            var lo = vec4<f32>(255.0);
            var hi = vec4<f32>(0.0);
            for (var i = 0u; i < 16u; i++) {
                if (((word >> (2u * i)) & 3u) == s) {
                    lo = min(lo, (*px)[i]);
                    hi = max(hi, (*px)[i]);
                }
            }
            let axis = (hi - lo) * mask;
            let len2 = dot(axis, axis);
            for (var i = 0u; i < 16u; i++) {
                if (((word >> (2u * i)) & 3u) == s) {
                    var t = 0.0;
                    if (len2 > 0.0) {
                        t = round(clamp(dot(((*px)[i] - lo) * mask, axis) / len2, 0.0, 1.0) * levels) / levels;
                    }
                    let d = ((*px)[i] - (lo + axis * t)) * mask;
                    err += dot(d, d);
                }
            }
            if (err >= best_err) {
                break;
            }
        }
        if (err < best_err) {
            best_err = err;
            best = index;
        }
    }
    return best;
}

// ------------------------------------------------------- BC7 scoring
//
// Desktop GPUs never see UASTC directly: Bevy transcodes it to BC7 at load
// time. How much a block loses there depends on its UASTC mode, so the mode
// choice also weighs the simulated BC7 error. The simulation follows basisu's
// transcode_uastc_to_bc7 (basisu_transcoder.cpp):
//   0/10/12/14/18 -> BC7 mode 6 (7-bit + unique p-bits, 4-bit weights)
//   1             -> BC7 mode 3 (7-bit + unique p-bits, 2-bit weights)
//   2             -> BC7 mode 1 (6-bit + shared p-bit per subset, 3-bit weights)
//   3             -> BC7 mode 2 (5-bit, 2-bit weights)
//   4             -> BC7 mode 3 (7-bit + unique p-bits, 2-bit weights)
//   9             -> BC7 mode 7 (5-bit + unique p-bits, RGBA, 2-bit weights)
//   11            -> BC7 mode 5 (7-bit color, 8-bit alpha, 2-bit weights/plane)

// basisu encode_uastc: UASTC_ERROR_THRESH and DEFAULT_BC7_ERROR_WEIGHT (50%).
const UASTC_ERROR_THRESH: f32 = 1.3;
const BC7_ERROR_WEIGHT: f32 = 0.5;

const BC7_WEIGHTS2: array<u32, 4> = array<u32, 4>(0u, 21u, 43u, 64u);
const BC7_WEIGHTS3: array<u32, 8> = array<u32, 8>(0u, 9u, 18u, 27u, 37u, 46u, 55u, 64u);
const BC7_WEIGHTS4: array<u32, 16> = array<u32, 16>(
    0u, 4u, 9u, 13u, 17u, 21u, 26u, 30u, 34u, 38u, 43u, 47u, 51u, 55u, 60u, 64u);
// UASTC weight -> BC7 4-bit selector (basisu s_bc7_5_to_4 / 3_to_4 / 2_to_4).
const BC7_5_TO_4: array<u32, 32> = array<u32, 32>(
    0u, 0u, 1u, 1u, 2u, 2u, 3u, 3u, 4u, 4u, 5u, 5u, 6u, 6u, 6u, 7u,
    8u, 9u, 9u, 9u, 10u, 10u, 11u, 11u, 12u, 12u, 13u, 13u, 14u, 14u, 15u, 15u);
const BC7_3_TO_4: array<u32, 8> = array<u32, 8>(0u, 2u, 4u, 6u, 9u, 11u, 13u, 15u);

fn bc7_interpolate(l: u32, h: u32, w: u32) -> f32 {
    return f32(((64u - w) * l + w * h + 32u) >> 6u);
}

// A `total_bits`-bit BC7 endpoint value expanded to 8 bits.
fn bc7_expand(x: u32, total_bits: u32) -> u32 {
    let shifted = x << (8u - total_bits);
    return shifted | (shifted >> total_bits);
}

// basisu's p-bit quantization of one 8-bit value: `total_bits` bits
// (components + p-bit), the p-bit fixed to `p`. Returns the stored value.
fn bc7_pbit_quant(v: u32, p: u32, total_bits: u32) -> u32 {
    let scale = f32((1u << total_bits) - 1u);
    let q = i32((f32(v) / 255.0 * scale - f32(p)) / 2.0 + 0.5) * 2 + i32(p);
    return u32(clamp(q, i32(p), i32(scale) - 1 + i32(p)));
}

// determine_unique_pbits: each endpoint picks its own p-bit. Returns the
// endpoint decoded to 8 bits.
fn bc7_unique_pbits(v: vec4<u32>, comps: u32, total_bits: u32) -> vec4<u32> {
    var best = v;
    var best_err = 3.4e38;
    for (var p = 0u; p < 2u; p++) {
        var x: vec4<u32>;
        var err = 0.0;
        for (var c = 0u; c < 4u; c++) {
            x[c] = bc7_expand(bc7_pbit_quant(v[c], p, total_bits), total_bits);
            if (c < comps) {
                let d = f32(x[c]) - f32(v[c]);
                err += d * d;
            }
        }
        if (err < best_err) {
            best_err = err;
            best = x;
        }
    }
    return best;
}

// determine_shared_pbits: both endpoints of a subset share one p-bit.
fn bc7_shared_pbits(lo: vec4<u32>, hi: vec4<u32>, comps: u32, total_bits: u32) -> array<vec4<u32>, 2> {
    var best: array<vec4<u32>, 2>;
    var best_err = 3.4e38;
    for (var p = 0u; p < 2u; p++) {
        var x: array<vec4<u32>, 2>;
        var err = 0.0;
        for (var c = 0u; c < 4u; c++) {
            x[0][c] = bc7_expand(bc7_pbit_quant(lo[c], p, total_bits), total_bits);
            x[1][c] = bc7_expand(bc7_pbit_quant(hi[c], p, total_bits), total_bits);
            if (c < comps) {
                let d0 = f32(x[0][c]) - f32(lo[c]);
                let d1 = f32(x[1][c]) - f32(hi[c]);
                err += d0 * d0 + d1 * d1;
            }
        }
        if (err < best_err) {
            best_err = err;
            best = x;
        }
    }
    return best;
}

// Weight (0..64) the BC7 transcode of UASTC weight q decodes with.
fn bc7_weight(m: Mode, q: u32) -> u32 {
    switch (m.id) {
        case 1u, 3u, 4u, 9u, 11u: { return BC7_WEIGHTS2[q]; }
        case 2u: { return BC7_WEIGHTS3[q]; }
        case 14u: { return BC7_WEIGHTS4[q * 5u]; }
        case 12u: { return BC7_WEIGHTS4[BC7_3_TO_4[q]]; }
        case 18u: { return BC7_WEIGHTS4[BC7_5_TO_4[q]]; }
        default: { return BC7_WEIGHTS4[q]; } // 0, 10
    }
}

fn bc7_error(px: ptr<function, array<vec4<f32>, 16>>, c: ptr<function, Candidate>, m: Mode) -> f32 {
    // Decoded 8-bit BC7 endpoints per subset.
    var lo: array<vec4<u32>, 3>;
    var hi: array<vec4<u32>, 3>;
    for (var s = 0u; s < m.subsets; s++) {
        var l = vec4<u32>(255u);
        var h = vec4<u32>(255u);
        for (var ch = 0u; ch < m.comps; ch++) {
            let k = (s * m.comps + ch) * 2u;
            l[ch] = endpoint_unquant((*c).e[k], m.range);
            h[ch] = endpoint_unquant((*c).e[k + 1u], m.range);
        }
        switch (m.id) {
            case 2u: {
                let x = bc7_shared_pbits(l, h, 3u, 7u);
                l = x[0];
                h = x[1];
            }
            case 3u: {
                for (var ch = 0u; ch < 3u; ch++) {
                    l[ch] = bc7_expand((l[ch] * 31u + 127u) / 255u, 5u);
                    h[ch] = bc7_expand((h[ch] * 31u + 127u) / 255u, 5u);
                }
            }
            case 9u: {
                l = bc7_unique_pbits(l, 4u, 6u);
                h = bc7_unique_pbits(h, 4u, 6u);
            }
            case 11u: {
                // 7-bit color without p-bits, alpha kept at 8 bits.
                for (var ch = 0u; ch < 3u; ch++) {
                    l[ch] = bc7_expand((l[ch] * 127u + 127u) / 255u, 7u);
                    h[ch] = bc7_expand((h[ch] * 127u + 127u) / 255u, 7u);
                }
            }
            default: {
                l = bc7_unique_pbits(l, m.comps, 8u);
                h = bc7_unique_pbits(h, m.comps, 8u);
            }
        }
        lo[s] = l;
        hi[s] = h;
    }
    let word = pattern_word(m.subsets, (*c).pattern);
    var err = 0.0;
    for (var i = 0u; i < 16u; i++) {
        let s = (word >> (2u * i)) & 3u;
        let wc = bc7_weight(m, (*c).w[select(i, i * 2u, m.dual)]);
        let wa = select(wc, bc7_weight(m, (*c).w[i * 2u + 1u]), m.dual);
        for (var ch = 0u; ch < m.comps; ch++) {
            let w = select(wc, wa, ch == 3u);
            let d = bc7_interpolate(lo[s][ch], hi[s][ch], w) - (*px)[i][ch];
            err += d * d;
        }
    }
    return err;
}

// ---------------------------------------------------------------- pack

// Swaps one plane's endpoints and inverts its weights when its anchor weight
// has the MSB set, since anchors are stored without it (same colors).
fn fix_anchor(c: ptr<function, Candidate>, p: Plane, anchor_texel: u32) {
    if (((*c).w[anchor_texel * p.stride + p.offset] >> (p.w_bits - 1u)) == 0u) {
        return;
    }
    for (var ch = 0u; ch < 4u; ch++) {
        if (p.mask[ch] > 0.0) {
            let k = endpoint_index(p, ch);
            let t = (*c).e[k];
            (*c).e[k] = (*c).e[k + 1u];
            (*c).e[k + 1u] = t;
        }
    }
    let top = (1u << p.w_bits) - 1u;
    for (var i = 0u; i < 16u; i++) {
        if (((p.members >> i) & 1u) != 0u) {
            let k = i * p.stride + p.offset;
            (*c).w[k] = top - (*c).w[k];
        }
    }
}

// Endpoints as UASTC stores them: for trit/quint ranges all base-3/base-5
// bundles first (plain numbers, 5 trits or 3 quints per bundle; a short
// last bundle uses fewer bits), then every value's low bits.
fn put_endpoints(b: ptr<function, Bits>, c: ptr<function, Candidate>, count: u32, range: u32) {
    let bits = range_bits(range);
    let trits = range == R7 || range == R13 || range == R19;
    let quints = range == R12;
    if (trits || quints) {
        let per_bundle = select(3u, 5u, trits);
        let base = select(5u, 3u, trits);
        let bundles = (count + per_bundle - 1u) / per_bundle;
        for (var k = 0u; k < bundles; k++) {
            let first = k * per_bundle;
            let n = min(per_bundle, count - first);
            var value = 0u;
            var scale = 1u;
            for (var j = 0u; j < n; j++) {
                value += ((*c).e[first + j] >> bits) * scale;
                scale *= base;
            }
            var size: u32;
            if (trits) {
                switch (n) {
                    case 1u: { size = 2u; }
                    case 2u: { size = 4u; }
                    case 3u: { size = 5u; }
                    case 4u: { size = 7u; }
                    default: { size = 8u; }
                }
            } else {
                switch (n) {
                    case 1u: { size = 3u; }
                    case 2u: { size = 5u; }
                    default: { size = 7u; }
                }
            }
            put(b, value, size);
        }
    }
    for (var i = 0u; i < count; i++) {
        put(b, (*c).e[i], bits);
    }
}

fn pack(c_in: Candidate, m: Mode) -> vec4<u32> {
    var c = c_in;
    let word = pattern_word(m.subsets, c.pattern);
    if (m.dual) {
        fix_anchor(&c, plane(MASK_RGB, m, 2u, 0u, ALL_TEXELS, 0u), 0u);
        fix_anchor(&c, plane(MASK_A, m, 2u, 1u, ALL_TEXELS, 0u), 0u);
    } else {
        let mask = select(MASK_RGB, MASK_RGBA, m.comps == 4u);
        for (var s = 0u; s < m.subsets; s++) {
            fix_anchor(&c, plane(mask, m, 1u, 0u, members(word, s), s), anchor(m.subsets, c.pattern, s));
        }
    }

    var b: Bits;
    put(&b, m.code, m.code_bits);
    // Hints: bc1 hint0, [bc1 hint1], etc1 flip, etc1 diff, inten0, inten1,
    // [etc1 bias], [etc2 alpha].
    put(&b, 0u, 1u);
    if (m.bc1_hint1) {
        put(&b, 0u, 1u);
    }
    put(&b, 0u, 1u);
    put(&b, 1u, 1u);
    put(&b, 0u, 3u);
    put(&b, 0u, 3u);
    if (m.etc1_bias) {
        put(&b, 0u, 5u);
    }
    if (m.etc2_alpha) {
        put(&b, (1u << 4u) | 11u, 8u); // ETC2 alpha: table 11, multiplier 1
    }
    put(&b, c.pattern, pattern_bits(m.subsets));
    if (m.dual) {
        put(&b, 3u, 2u); // ccs: alpha on plane 1
    }
    put_endpoints(&b, &c, m.comps * 2u * m.subsets, m.range);

    // Anchor weights (the first texel of each subset, or of each plane) are
    // stored without their MSB.
    let stride = select(1u, 2u, m.dual);
    for (var i = 0u; i < 16u * stride; i++) {
        var is_anchor = i < stride;
        if (!m.dual) {
            for (var s = 0u; s < m.subsets; s++) {
                is_anchor = is_anchor || i == anchor(m.subsets, c.pattern, s);
            }
        }
        put(&b, c.w[i], select(m.w_bits, m.w_bits - 1u, is_anchor));
    }
    return finish(b);
}

fn pack_solid(color: vec4<u32>) -> vec4<u32> {
    var b: Bits;
    put(&b, 0x17u, 5u); // mode 8
    put(&b, color.r, 8u);
    put(&b, color.g, 8u);
    put(&b, color.b, 8u);
    put(&b, color.a, 8u);
    // ETC1 hints: diff, inten0, selector, 5-bit base color.
    put(&b, 1u, 1u);
    put(&b, 0u, 3u);
    put(&b, 0u, 2u);
    put(&b, color.r >> 3u, 5u);
    put(&b, color.g >> 3u, 5u);
    put(&b, color.b >> 3u, 5u);
    return finish(b);
}

// -------------------------------------------------------------- decode

fn src_word(addr: u32) -> u32 {
    return source[addr >> 2u];
}

fn src_byte(addr: u32) -> u32 {
    return (source[addr >> 2u] >> ((addr & 3u) * 8u)) & 255u;
}

fn rgba(r: u32, g: u32, b: u32, a: u32) -> u32 {
    return r | (g << 8u) | (b << 16u) | (a << 24u);
}

// Decodes block (bx, by) of `img` to packed RGBA texels in raster order.
// Texels past the image edge repeat the last column/row, matching the CPU
// path, which decodes the cropped image and then clamps.
fn load_block(img: ImageDesc, bx: u32, by: u32) -> array<u32, 16> {
    var out: array<u32, 16>;
    let fmt = img.format;
    let bpp = select(4u, 3u, fmt == FMT_BGR8);
    for (var i = 0u; i < 16u; i++) {
        let x = min(bx * 4u + (i & 3u), img.width - 1u);
        let y = min(by * 4u + (i >> 2u), img.height - 1u);
        let addr = img.source_offset + y * img.pitch + x * bpp;
        switch (fmt) {
            case FMT_BGRA8: {
                let w = src_word(addr);
                out[i] = rgba((w >> 16u) & 255u, (w >> 8u) & 255u, w & 255u, w >> 24u);
            }
            case FMT_BGRX8: {
                let w = src_word(addr);
                out[i] = rgba((w >> 16u) & 255u, (w >> 8u) & 255u, w & 255u, 255u);
            }
            case FMT_BGR8: {
                out[i] = rgba(src_byte(addr + 2u), src_byte(addr + 1u), src_byte(addr), 255u);
            }
            default: { // RGBA8
                out[i] = src_word(addr);
            }
        }
    }
    return out;
}

// ---------------------------------------------------------------- main

fn find_image(block: u32) -> u32 {
    var lo = 0u;
    var hi = params.image_count - 1u;
    while (lo < hi) {
        let mid = (lo + hi + 1u) / 2u;
        if (images[mid].first_block <= block) {
            lo = mid;
        } else {
            hi = mid - 1u;
        }
    }
    return lo;
}

@compute @workgroup_size(64)
fn main(@builtin(workgroup_id) wg: vec3<u32>,
        @builtin(num_workgroups) groups: vec3<u32>,
        @builtin(local_invocation_index) local: u32) {
    let block = params.block_base + (wg.y * groups.x + wg.x) * WORKGROUP_SIZE + local;
    if (block >= params.block_end) {
        return;
    }
    let img_index = find_image(block);
    let img = images[img_index];
    let local_block = block - img.first_block;
    let bx = local_block % img.blocks_x;
    let by = local_block / img.blocks_x;

    var raw = load_block(img, bx, by);
    var px: array<vec4<f32>, 16>;
    var solid = true;
    var opaque = true;
    for (var i = 0u; i < 16u; i++) {
        px[i] = unpack4x8unorm(raw[i]) * 255.0;
        solid = solid && raw[i] == raw[0];
        opaque = opaque && (raw[i] >> 24u) == 255u;
    }
    if (!opaque) {
        atomicOr(&alpha_flags[img_index], 1u);
    }

    if (solid) {
        let c = vec4<u32>(raw[0] & 255u, (raw[0] >> 8u) & 255u, (raw[0] >> 16u) & 255u, raw[0] >> 24u);
        blocks[block] = pack_solid(c);
        return;
    }

    // Mode choice mirrors basisu's encode_uastc: only modes whose ASTC error
    // (what ASTC GPUs show) is within UASTC_ERROR_THRESH of the best one are
    // eligible, and among those the lowest ASTC + 0.5 * BC7 error wins (BC7 is
    // what desktop GPUs show after Bevy's transcode). Errors are kept, not the
    // candidates, and the winner is encoded again: cheaper in registers.
    var modes: array<Mode, 6>;
    var mode_count: u32;
    if (opaque) {
        modes = array<Mode, 6>(mode18(), mode0(), mode1(), mode2(), mode3(), mode4());
        mode_count = 6u;
    } else {
        modes = array<Mode, 6>(mode10(), mode12(), mode11(), mode14(), mode9(), mode9());
        mode_count = 5u;
    }
    var patterns: array<u32, 6>;
    var astc_err: array<f32, 6>;
    var bc7_err: array<f32, 6>;
    var best_astc = 3.4e38;
    for (var i = 0u; i < mode_count; i++) {
        if (modes[i].subsets > 1u) {
            patterns[i] = best_pattern(&px, modes[i]);
        }
        var c = encode(&px, modes[i], patterns[i]);
        astc_err[i] = c.err;
        bc7_err[i] = bc7_error(&px, &c, modes[i]);
        best_astc = min(best_astc, c.err);
    }
    // basisu compares RMS errors: sqrt(e) <= 1.3 * sqrt(best) <=> e <= 1.69 * best.
    // Room for quality (the author's out-of-tree benchmark: ~0.3-0.5 dB below
    // basisu UASTC level 2):
    // - fit_plane's refinement is least squares on unquantized endpoints; a
    //   search over +-1 quantization steps per endpoint channel is where most
    //   of the gap is, and would make `quality` above 2 worthwhile.
    // - when a mode reaches zero ASTC error, basisu (and this code) lets any
    //   mode compete on the BC7 score, which can cost very smooth textures
    //   ~10 dB of ASTC (still ~69 dB); restricting to zero-error modes avoids it.
    let window = UASTC_ERROR_THRESH * UASTC_ERROR_THRESH * best_astc;
    var best_index = 0u;
    var best_score = 3.4e38;
    for (var i = 0u; i < mode_count; i++) {
        if (best_astc == 0.0 || astc_err[i] <= window) {
            let score = astc_err[i] + BC7_ERROR_WEIGHT * bc7_err[i];
            if (score < best_score) {
                best_score = score;
                best_index = i;
            }
        }
    }
    let best_mode = modes[best_index];
    let best = encode(&px, best_mode, patterns[best_index]);
    blocks[block] = pack(best, best_mode);
}
