// Optional v4 observations at the verified Windows x64 void(controller*, float) CALL.
using JumpSetter = void (*)(void*, float);
static JumpSetter original_setter;

template<class T> static bool checked_field(const void* object, U64 offset, T& value) {
    return object && read(static_cast<const unsigned char*>(object) + offset, value);
}
static bool form_id(const void* object, float* output) {
    U32 id = 0;
    if (checked_field(object, 0x14, id)) {
        output[0] = static_cast<float>(id & 65535);
        output[1] = static_cast<float>(id >> 16);
        return true;
    }
    return false;
}
static void input_snapshot(void* controller, Record& record, const char* stage) {
    record.kind = 10;
    text_copy(record.text, stage);
    void* actor = nullptr;
    void* npc = nullptr;
    void* race = nullptr;
    unsigned short scale = 0;
    unsigned char type = 0, sex = 0;
    U32 actor_flags = 0, state = 0;
    if (checked_field(base, jump_height_setting, record.values[0])) record.code |= 1;
    if (checked_field(base, player_global, actor) && actor &&
        checked_field(actor, 0x98, scale)) {
        record.values[1] = scale;
        record.code |= 2;
        if (form_id(actor, record.values + 16)) record.code |= 16384;
    }
    if (checked_field(actor, 0x40, npc) && npc) record.code |= 4;
    if (checked_field(npc, 0x1a, type)) record.values[2] = type;
    if (type == 0x2b && checked_field(npc, 0x38, sex) &&
        checked_field(npc, 0x1f8, record.values[4])) {
        record.values[3] = sex & 1;
        record.code |= 8;
        if (form_id(npc, record.values + 18)) record.code |= 32768;
        if (checked_field(npc, 0x158, race)) {
            record.code |= 16;
            if (race && checked_field(race, 0xf8, record.values[5]) &&
                checked_field(race, 0xfc, record.values[6])) {
                record.code |= 32;
                if (form_id(race, record.values + 20)) record.code |= 65536;
            }
        }
    }
    if (checked_field(controller, 0x23c, record.values[8]) &&
        checked_field(controller, 0x21c, state)) {
        record.values[10] = static_cast<float>(state);
        record.code |= 64;
    }
    if (checked_field(base, game_to_havok_constant, record.values[11])) record.code |= 256;
    if (checked_field(base, swim_jump_setting, record.values[13])) record.code |= 1024;
    if (checked_field(actor, 0x204, actor_flags)) {
        record.values[14] = static_cast<float>((actor_flags >> 1) & 1);
        record.code |= 2048;
    }
    if (checked_field(controller, 0x200, state)) {
        record.values[15] = static_cast<float>(state);
        record.code |= 4096;
    }
    if (checked_field(base, reference_scale_constant, record.values[22])) record.code |= 8192;
}
static void observe_setter(void* controller, float height, U32 source) {
    if (controller != player_character()) {
        original_setter(controller, height);
        return;
    }
    Record record{};
    input_snapshot(controller, record, "setter");
    record.state = source;
    record.values[7] = height;
    // Native code may invoke another observer. Never retain record_lock across it.
    original_setter(controller, height);
    U32 state = 0;
    if (checked_field(controller, 0x23c, record.values[9]) &&
        checked_field(controller, 0x21c, state)) {
        record.values[10] = static_cast<float>(state);
        record.code |= 128;
    }
    if (checked_field(base, jump_height_setting, record.values[12])) record.code |= 512;
    RecordLock lock;
    record.sample = samples;
    emit(record, true);
}
static void jump_setter(void* controller, float height) {
    observe_setter(controller, height, static_cast<U32>(setter_call));
}
static void player_jump_setter(void* controller, float height) {
    observe_setter(controller, height, static_cast<U32>(player_setter_call));
}
static bool validate_jump_inputs() {
    if (!matches(setter_call, setter_call_signature, sizeof(setter_call_signature)) ||
        !matches(player_setter_call, player_setter_call_signature, sizeof(player_setter_call_signature)) ||
        !matches(setter_function, setter_signature, sizeof(setter_signature))) return false;
    const auto displacement = *reinterpret_cast<const int*>(base + setter_call + 1);
    const auto player_displacement = *reinterpret_cast<const int*>(base + player_setter_call + 1);
    return base + setter_call + 5 + displacement == base + setter_function &&
           base + player_setter_call + 5 + player_displacement == base + setter_function;
}
static bool install_setter_call(unsigned char* relay, U64 call, JumpSetter hook) {
    relay[0] = 0x48; relay[1] = 0xb8;
    *reinterpret_cast<U64*>(relay + 2) = reinterpret_cast<U64>(hook);
    relay[10] = 0xff; relay[11] = 0xe0;
    const auto displacement = reinterpret_cast<long long>(relay) -
                              reinterpret_cast<long long>(base + call + 5);
    if (displacement < -2147483648LL || displacement > 2147483647LL) return false;
    U32 protection = 0, discarded = 0;
    if (!VirtualProtect(base + call, 5, 0x40, &protection)) return false;
    *reinterpret_cast<int*>(base + call + 1) = static_cast<int>(displacement);
    VirtualProtect(base + call, 5, protection, &discarded);
    FlushInstructionCache(GetCurrentProcess(), base + call, 5);
    return true;
}
static bool install_jump_inputs(unsigned char* relay) {
    original_setter = reinterpret_cast<JumpSetter>(base + setter_function);
    return install_setter_call(relay, setter_call, &jump_setter) &&
           install_setter_call(relay + 16, player_setter_call, &player_jump_setter);
}
