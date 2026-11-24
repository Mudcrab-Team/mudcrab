// Observes one pinned Skyrim runtime. All original input/camera functions run unchanged.
#include "runtime.generated.h"

using U32 = unsigned int;
using U64 = unsigned long long;
using Handle = void*;
#define API extern "C" __declspec(dllimport)
API Handle LoadLibraryW(const wchar_t*);
API void* GetProcAddress(Handle, const char*);
API U32 GetSystemDirectoryW(wchar_t*, U32);
API Handle GetModuleHandleW(const wchar_t*);
API U32 GetEnvironmentVariableA(const char*, char*, U32);
API Handle CreateFileA(const char*, U32, U32, void*, U32, U32, Handle);
API int WriteFile(Handle, const void*, U32, U32*, void*);
API int CloseHandle(Handle);
API int QueryPerformanceCounter(U64*);
API int QueryPerformanceFrequency(U64*);
API U32 GetCurrentThreadId();
API Handle GetCurrentProcess();
API int ReadProcessMemory(Handle, const void*, void*, U64, U64*);
API int VirtualProtect(void*, U64, U32, U32*);
API void* VirtualAlloc(void*, U64, U32, U32);
API int FlushInstructionCache(Handle, const void*, U64);
API void AcquireSRWLockExclusive(void*);
API void ReleaseSRWLockExclusive(void*);
extern "C" { int _fltused = 0; }
extern "C" void* memset(void* destination, int value, U64 length) {
    auto* bytes = static_cast<volatile unsigned char*>(destination);
    for (U64 i = 0; i < length; ++i) bytes[i] = static_cast<unsigned char>(value);
    return destination;
}
extern "C" void* memcpy(void* destination, const void* source, U64 length) {
    auto* output = static_cast<volatile unsigned char*>(destination);
    const auto* input = static_cast<const volatile unsigned char*>(source);
    for (U64 i = 0; i < length; ++i) output[i] = input[i];
    return destination;
}

// Fixed 256-byte little-endian wire records, independently decoded by the runner.
struct Record {
    U64 sequence, qpc, sample, input_sequence;
    U32 kind, thread, code, state;
    float values[40];
    char text[48];
};
static_assert(sizeof(Record) == 256);
static Handle file;
static unsigned char* base;
static U64 sequence, samples, inputs;
static U64 clock_frequency, stop_at;
static void* record_lock;
struct RecordLock {
    RecordLock() { AcquireSRWLockExclusive(&record_lock); }
    ~RecordLock() { ReleaseSRWLockExclusive(&record_lock); }
};
static bool recording;
static Record pending[8];
static U32 pending_count;
using CameraUpdate = void (*)(void*);
using MovementButton = void (*)(void*, void*, void*);
static CameraUpdate original_camera;
static MovementButton original_movement;
static MovementButton original_jump;
using CharacterUpdate = void (*)(void*, void*);
static CharacterUpdate original_air;
static CharacterUpdate original_launch;

template<class T> bool read(const void* pointer, T& value) {
    U64 copied = 0;
    return pointer && ReadProcessMemory(GetCurrentProcess(), pointer, &value, sizeof(T), &copied)
        && copied == sizeof(T);
}
template<class T> T field(const void* object, U64 offset) {
    T result{};
    if (!object) return result;
    read(static_cast<const unsigned char*>(object) + offset, result);
    return result;
}
static void text_copy(char* target, const char* source) {
    if (!source) return;
    for (U32 i = 0; i < 47; ++i) {
        char c = 0;
        if (!read(source + i, c) || !c) break;
        target[i] = c;
    }
}
static void flush() {
    if (!pending_count || !recording) return;
    U32 written = 0;
    const U32 bytes = pending_count * sizeof(Record);
    if (!WriteFile(file, pending, bytes, &written, nullptr) || written != bytes) recording = false;
    pending_count = 0;
}
static void emit(Record& record, bool immediate = false) {
    if (!recording) return;
    record.sequence = ++sequence;
    QueryPerformanceCounter(&record.qpc);
    record.thread = GetCurrentThreadId();
    record.input_sequence = inputs;
    pending[pending_count++] = record;
    if (pending_count == 8 || immediate) flush();
}
static void diagnostic(const char* message, U32 code) {
    Record record{};
    record.kind = 4;
    record.code = code;
    text_copy(record.text, message);
    emit(record, true);
}
struct Vec3 { float x, y, z; };
static void copy_vec(float* target, Vec3 value) {
    target[0] = value.x; target[1] = value.y; target[2] = value.z;
}
static void* player_character() {
    void* player = field<void*>(base, player_global);
    void* process = field<void*>(player, actor_process_offset);
    void* middle = field<void*>(process, 8);
    void* controller = field<void*>(middle, 0x250);
    return field<void*>(controller, 0) == base + character_controller_vtable ? controller : nullptr;
}
static bool character_snapshot(void* controller, float* values) {
    if (!controller || field<void*>(controller, 0) != base + character_controller_vtable) return false;
    void* proxy = field<void*>(controller, 0x340);
    void* player = field<void*>(base, player_global);
    // Only deltaTime and its reciprocal are initialized on this native path.
    for (U32 i = 2; i < 4; ++i)
        if (!read(static_cast<unsigned char*>(controller) + 0x80 + i * 4, values[i])) return false;
    void* phantom = field<void*>(proxy, 0x80);
    void* transform = field<void*>(phantom, 0x30);
    void* world = field<void*>(phantom, 0x10);
    Vec3 gravity{};
    if (!transform || !world ||
        !read(static_cast<unsigned char*>(transform) + 0x38, values[0]) ||
        !read(static_cast<unsigned char*>(world) + 0x20, gravity)) return false;
    Vec3 out{}, velocity{}, position{};
    if (!read(static_cast<unsigned char*>(controller) + 0x90, out) ||
        !proxy || !read(static_cast<unsigned char*>(proxy) + 0x60, velocity) ||
        !player || !read(static_cast<unsigned char*>(player) + 0x54, position)) return false;
    copy_vec(values + 4, out);
    copy_vec(values + 7, velocity);
    values[10] = field<float>(controller, 0x248);
    values[11] = field<float>(controller, 0x23c);
    values[12] = field<float>(controller, 0x240);
    copy_vec(values + 13, position);
    values[16] = static_cast<float>(field<U32>(controller, 0x200));
    values[17] = static_cast<float>(field<U32>(controller, 0x21c));
    return true;
}
static Vec3 world_gravity(void* controller) {
    void* proxy = field<void*>(controller, 0x340);
    void* phantom = field<void*>(proxy, 0x80);
    return field<Vec3>(field<void*>(phantom, 0x10), 0x20);
}
#ifdef MUDCRAB_INPUTS_V4
static void input_snapshot(void*, Record&, const char*);
#endif
static void character_update(void* state, void* controller, CharacterUpdate original, U32 kind) {
    if (controller != player_character()) {
        original(state, controller);
        return;
    }
    Record record{};
    record.kind = kind;
    U64 begin = 0, end = 0;
    QueryPerformanceCounter(&begin);
    if (character_snapshot(controller, record.values)) record.code |= 1;
#ifdef MUDCRAB_INPUTS_V4
    Record loaded{};
    if (kind == 9) input_snapshot(controller, loaded, "launch");
#endif
    original(state, controller);
    if (character_snapshot(controller, record.values + 18)) record.code |= 2;
    QueryPerformanceCounter(&end);
    RecordLock lock;
    record.sample = samples;
    record.values[36] = field<float>(base, havok_scale_constant);
    record.values[37] = field<float>(base, fall_multiplier_setting);
    record.values[38] = static_cast<float>(static_cast<double>(end - begin) / clock_frequency);
    record.values[39] = world_gravity(controller).z;
    emit(record);
#ifdef MUDCRAB_INPUTS_V4
    if (kind == 9) { loaded.sample = samples; emit(loaded); }
#endif
}
static void air_update(void* state, void* controller) {
    character_update(state, controller, original_air, 8);
}
static void launch_update(void* state, void* controller) {
    character_update(state, controller, original_launch, 9);
}
static void* find_camera(void* node, U32 depth = 0) {
    if (!node || depth > 3) return nullptr;
    if (field<void*>(node, 0) == base + nicamera_vtable) return node;
    // The bounded, checked child reads tolerate non-node children and sparse arrays.
    void** children = field<void**>(node, 0x118);
    const auto capacity = field<unsigned short>(node, 0x120);
    const auto end = field<unsigned short>(node, 0x122);
    if (!children || end > capacity || capacity > 64) return nullptr;
    for (U32 i = 0; i < end; ++i) {
        void* child = nullptr;
        if (read(children + i, child)) {
            if (void* camera = find_camera(child, depth + 1)) return camera;
        }
    }
    return nullptr;
}
static bool spring_snapshot(void* camera, float* values, bool before) {
    void* player = field<void*>(base, player_global);
    void* state = field<void*>(camera, 0x28);
    if (!player || !state || field<void*>(state, 0) != base + firstperson_vtable) return false;
    const U32 offset = before ? 0 : 18;
    if (before) copy_vec(values, field<Vec3>(player, 0x54));
    copy_vec(values + offset + (before ? 3 : 0), field<Vec3>(state, 0x30));
    copy_vec(values + offset + (before ? 6 : 3), field<Vec3>(state, 0x3c));
    copy_vec(values + offset + (before ? 9 : 6), field<Vec3>(state, 0x48));
    void* node = field<void*>(state, 0x58);
    Vec3 node_position{};
    if (!node || !read(static_cast<unsigned char*>(node) + 0xa0, node_position)) return false;
    copy_vec(values + (before ? 12 : 27), node_position);
    values[before ? 15 : 30] = field<float>(state, 0x80);
    void* controller = field<void*>(player, player_controller_offset);
    unsigned char gate = 0;
    if (!controller || !read(static_cast<unsigned char*>(controller) + controller_gate_offset, gate))
        return false;
    values[before ? 16 : 31] = gate;
    values[before ? 17 : 32] = field<float>(base, camera_dt_global);
    values[36] = field<float>(base, vertical_spring_setting);
    values[37] = field<float>(base, vertical_damping_setting);
    return true;
}
static void camera_update(void* camera) {
    Record spring{};
    spring.kind = 6;
    if (spring_snapshot(camera, spring.values, true)) spring.code |= 1;
    original_camera(camera);
    RecordLock lock;
    if (!recording) return;
    Record record{};
    record.kind = 3;
    record.sample = ++samples;
    Record character{};
    character.kind = 7;
    character.sample = samples;
    void* controller = player_character();
    if (character_snapshot(controller, character.values)) {
        character.code = 1;
        copy_vec(character.values + 18, world_gravity(controller));
        character.values[21] = field<float>(base, havok_scale_constant);
        character.values[22] = field<float>(base, fall_multiplier_setting);
    }
    emit(character);
    spring.sample = samples;
    if (spring_snapshot(camera, spring.values, false)) spring.code |= 2;
    if (spring.code) emit(spring);
    void* player = field<void*>(base, player_global);
    void* state = field<void*>(camera, 0x28);
    void* root = field<void*>(camera, 0x20);
    record.state = state ? field<U32>(state, 0x18) : 0xffffffff;
    Vec3 player_position{}, root_position{};
    if (player && root && read(static_cast<unsigned char*>(player) + 0x54, player_position) &&
        read(static_cast<unsigned char*>(root) + 0xa0, root_position)) {
        record.code |= 1;
        copy_vec(record.values, player_position);
        copy_vec(record.values + 3, field<Vec3>(player, 0x48));
        copy_vec(record.values + 6, field<Vec3>(root, 0x6c));
        copy_vec(record.values + 9, root_position);
        if (void* eye = find_camera(root)) {
            record.code |= 2;
            copy_vec(record.values + 12, field<Vec3>(eye, 0xa0));
            struct Matrix { float values[16]; } matrix{};
            if (read(static_cast<unsigned char*>(eye) + 0x110, matrix)) {
                for (U32 i = 0; i < 16; ++i) record.values[15 + i] = matrix.values[i];
            }
        }
    }
    emit(record);
    if (stop_at && record.qpc >= stop_at) {
        Record footer{};
        footer.kind = 5;
        footer.sample = samples;
        text_copy(footer.text, "complete after native W release");
        emit(footer, true);
        recording = false;
        CloseHandle(file);
    }
}
static void observe_button(void* event, void* controls) {
    RecordLock lock;
    if (!recording) return;
    if (field<U32>(event, 8) != 0 || field<U32>(event, 0xc) != 0) return;
    Record record{};
    record.kind = 2;
    record.sample = samples;
    record.code = field<U32>(event, 0x20);
    record.state = (field<U32>(event, 8) << 16) | field<U32>(event, 0xc);
    record.values[0] = field<float>(event, 0x28);
    record.values[1] = field<float>(event, 0x2c);
    record.values[2] = field<float>(controls, 0);
    record.values[3] = field<float>(controls, 4);
    text_copy(record.text, field<const char*>(event, 0x18));
    ++inputs;
    emit(record, true);
    if (record.code == 17 && record.values[0] == 0 && record.values[1] > 0)
        stop_at = record.qpc + clock_frequency * 2;
}
static void movement_button(void* handler, void* event, void* controls) {
    original_movement(handler, event, controls);
    observe_button(event, controls);
}
static void jump_button(void* handler, void* event, void* controls) {
    original_jump(handler, event, controls);
    observe_button(event, controls);
}
static bool matches(U64 rva, const unsigned char* expected, U64 size) {
    for (U64 i = 0; i < size; ++i) if (base[rva + i] != expected[i]) return false;
    return true;
}
#ifdef MUDCRAB_INPUTS_V4
#include "jump_inputs.h"
#endif
static bool install() {
    base = static_cast<unsigned char*>(GetModuleHandleW(nullptr));
    if (!matches(movement_function, movement_signature, sizeof(movement_signature)) ||
        !matches(jump_function, jump_signature, sizeof(jump_signature)) ||
        !matches(air_function, air_signature, sizeof(air_signature)) ||
        !matches(launch_function, launch_signature, sizeof(launch_signature)) ||
        !matches(camera_function, camera_signature, sizeof(camera_signature)) ||
        !matches(controller_getter_function, controller_getter_signature, sizeof(controller_getter_signature)) ||
        !matches(controller_gate_function, controller_gate_signature, sizeof(controller_gate_signature)) ||
        !matches(camera_call, camera_call_signature, sizeof(camera_call_signature)) ||
        field<void*>(base, movement_vtable_slot) != base + movement_function ||
        field<void*>(base, jump_vtable_slot) != base + jump_function ||
        field<void*>(base, air_vtable_slot) != base + air_function ||
        field<void*>(base, launch_vtable_slot) != base + launch_function) return false;
#ifdef MUDCRAB_INPUTS_V4
    if (!validate_jump_inputs()) return false;
#endif
    original_movement = reinterpret_cast<MovementButton>(base + movement_function);
    original_jump = reinterpret_cast<MovementButton>(base + jump_function);
    original_camera = reinterpret_cast<CameraUpdate>(base + camera_function);
    original_air = reinterpret_cast<CharacterUpdate>(base + air_function);
    original_launch = reinterpret_cast<CharacterUpdate>(base + launch_function);
    // A nearby absolute-jump relay replaces one verified CALL, with no prologue relocation.
    unsigned char* relay = nullptr;
    U64 origin = reinterpret_cast<U64>(base) & ~65535ULL;
    for (U64 distance = 0x10000; distance < 0x70000000 && !relay; distance += 0x10000) {
        relay = static_cast<unsigned char*>(VirtualAlloc(reinterpret_cast<void*>(origin + distance),
                                                        4096, 0x3000, 0x40));
    }
    if (!relay) return false;
#ifdef MUDCRAB_INPUTS_V4
    if (!install_jump_inputs(relay + 16)) return false;
#endif
    relay[0] = 0x48; relay[1] = 0xb8;
    *reinterpret_cast<U64*>(relay + 2) = reinterpret_cast<U64>(&camera_update);
    relay[10] = 0xff; relay[11] = 0xe0;
    const auto displacement = reinterpret_cast<long long>(relay) - reinterpret_cast<long long>(base + camera_call + 5);
    if (displacement < -2147483648LL || displacement > 2147483647LL) return false;
    U32 protection = 0;
    if (!VirtualProtect(base + camera_call, 5, 0x40, &protection)) return false;
    *reinterpret_cast<int*>(base + camera_call + 1) = static_cast<int>(displacement);
    U32 discarded;
    VirtualProtect(base + camera_call, 5, protection, &discarded);
    FlushInstructionCache(GetCurrentProcess(), base + camera_call, 5);
    if (!VirtualProtect(base + movement_vtable_slot, 8, 0x04, &protection)) return false;
    *reinterpret_cast<MovementButton*>(base + movement_vtable_slot) = &movement_button;
    VirtualProtect(base + movement_vtable_slot, 8, protection, &discarded);
    if (!VirtualProtect(base + jump_vtable_slot, 8, 0x04, &protection)) return false;
    *reinterpret_cast<MovementButton*>(base + jump_vtable_slot) = &jump_button;
    VirtualProtect(base + jump_vtable_slot, 8, protection, &discarded);
    if (!VirtualProtect(base + air_vtable_slot, 8, 0x04, &protection)) return false;
    *reinterpret_cast<CharacterUpdate*>(base + air_vtable_slot) = &air_update;
    VirtualProtect(base + air_vtable_slot, 8, protection, &discarded);
    if (!VirtualProtect(base + launch_vtable_slot, 8, 0x04, &protection)) return false;
    *reinterpret_cast<CharacterUpdate*>(base + launch_vtable_slot) = &launch_update;
    VirtualProtect(base + launch_vtable_slot, 8, protection, &discarded);
    return true;
}
extern "C" __declspec(dllexport) long DirectInput8Create(
    Handle instance, U32 version, const void* iid, void** output, void* outer) {
    using Create = long (*)(Handle, U32, const void*, void**, void*);
    static Create original;
    if (!original) {
        wchar_t path[512]{};
        U32 length = GetSystemDirectoryW(path, 480);
        const wchar_t suffix[] = L"\\dinput8.dll";
        if (!length || length >= 480) return static_cast<long>(0x80004005);
        for (U32 i = 0; i < sizeof(suffix) / sizeof(wchar_t); ++i) path[length + i] = suffix[i];
        original = reinterpret_cast<Create>(GetProcAddress(LoadLibraryW(path), "DirectInput8Create"));
        if (!original || original == &DirectInput8Create) return static_cast<long>(0x80004005);
        char trace_path[1024]{};
        U32 size = GetEnvironmentVariableA("MUDCRAB_TRACE_FILE", trace_path, sizeof(trace_path));
        if (!size || size >= sizeof(trace_path)) return original(instance, version, iid, output, outer);
        file = CreateFileA(trace_path, 0x40000000, 1, nullptr, 1, 0x80, nullptr);
        if (file == reinterpret_cast<Handle>(-1LL)) return original(instance, version, iid, output, outer);
        recording = true;
        Record header{};
        header.kind = 1; header.code = 3;
#ifdef MUDCRAB_INPUTS_V4
        header.code = 4;
#endif
        QueryPerformanceFrequency(&header.sample);
        clock_frequency = header.sample;
#ifdef MUDCRAB_INPUTS_V4
        text_copy(header.text, "mudcrab-jump-inputs-256-v4");
#else
        text_copy(header.text, "mudcrab-motion-256-v3");
#endif
        emit(header, true);
        if (!install()) {
            diagnostic("runtime validation or hook installation failed", 1);
            recording = false;
        } else diagnostic("hooks installed", 0);
    }
    return original(instance, version, iid, output, outer);
}
