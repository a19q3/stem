/* Independently compile each example Type Script with its own RULE. */
typedef unsigned long long u64;
typedef unsigned int u32;
typedef unsigned char byte;

#define INPUT 1ULL
#define OUTPUT 2ULL
#define GROUP_INPUT 0x0100000000000001ULL
#define GROUP_OUTPUT 0x0100000000000002ULL
#define OUT_OF_BOUND 1
#define MISSING 2
#define LOAD_INPUT_BY_FIELD 2083
#define LOAD_CELL_FIELD 2081
#define LOAD_DATA 2092
#define LOAD_TYPES 2110
#define LOAD_TYPE 2111
#define LOAD_TYPE_HASH 2112
#define FIND_TYPE 2113
#define LOAD_GROUP 2114
#define LOAD_GROUP_INDEX 2115
#define LOAD_GROUP_PROOF 2117
#define LOAD_SCRIPT 2052
#define LOAD_SCRIPT_HASH 2062

static inline u64 load(u64 call, void *out, u64 *length, u64 index,
                       u64 source, u64 argument) {
    register u64 a0 __asm__("a0") = (u64)out;
    register u64 a1 __asm__("a1") = (u64)length;
    register u64 a2 __asm__("a2") = 0;
    register u64 a3 __asm__("a3") = index;
    register u64 a4 __asm__("a4") = source;
    register u64 a5 __asm__("a5") = argument;
    register u64 a7 __asm__("a7") = call;
    __asm__ volatile("ecall" : "+r"(a0) : "r"(a1), "r"(a2), "r"(a3),
                     "r"(a4), "r"(a5), "r"(a7) : "memory");
    return a0;
}

#if RULE >= 7
static inline u64 process(u64 call, u64 arg0, u64 arg1, u64 arg2, u64 arg4) {
    register u64 a0 __asm__("a0") = arg0;
    register u64 a1 __asm__("a1") = arg1;
    register u64 a2 __asm__("a2") = arg2;
    register u64 a3 __asm__("a3") = 0;
    register u64 a4 __asm__("a4") = arg4;
    register u64 a5 __asm__("a5") = 0;
    register u64 a7 __asm__("a7") = call;
    __asm__ volatile("ecall" : "+r"(a0) : "r"(a1), "r"(a2), "r"(a3),
                     "r"(a4), "r"(a5), "r"(a7) : "memory");
    return a0;
}
#endif

static inline int eq(const byte *a, const byte *b, u64 len) {
    for (u64 i = 0; i < len; i++)
        if (a[i] != b[i])
            return 0;
    return 1;
}

static inline u32 le32(const byte *value) {
    return (u32)value[0] | ((u32)value[1] << 8) |
           ((u32)value[2] << 16) | ((u32)value[3] << 24);
}

static inline u64 le64(const byte *value) {
    return (u64)le32(value) | ((u64)le32(value + 4) << 32);
}

static int verify(void) {
    byte group[9];
    u64 len = sizeof(group);
    if (load(LOAD_GROUP, group, &len, 0, 0, 0) || len != sizeof(group) ||
        group[0] != 1)
        return 10;
    u32 inputs = le32(group + 1);
    u32 outputs = le32(group + 5);

#if RULE == 7 || RULE == 9
    byte script[57];
    len = sizeof(script);
    if (load(LOAD_SCRIPT, script, &len, 0, 0, 0) || len != sizeof(script))
        return 29;
    u64 index = le32(script + 53);
#if RULE == 7
    u64 pid = 0;
    u64 args[4] = {0, 0, (u64)&pid, 0};
    byte result = 255;
    if (process(2601, index, 3, 0, (u64)args)
        || process(2602, pid, (u64)&result, 0, 0))
        return 30;
    return result;
#else
    process(2043, index, 3, 0, 0);
    return 31;
#endif
#elif RULE == 8
    /* A child or replacement machine sees the same type group and context. */
    byte index_bytes[4];
    len = sizeof(index_bytes);
    return inputs != 1 || outputs != 1 ||
           load(LOAD_GROUP_INDEX, index_bytes, &len, 0, GROUP_INPUT, 0) ||
           len != sizeof(index_bytes) || le32(index_bytes) != 0;
#elif RULE == 10
    byte proof[4];
    len = sizeof(proof);
    return inputs != 1 || outputs != 1 ||
           load(LOAD_GROUP_PROOF, proof, &len, 0, 0, 0) ||
           len != sizeof(proof) || le32(proof) != 42;
#elif RULE == 4
    /* Identity args are a specific 36-byte creating OutPoint. The input is
       consumed once, so it cannot create a second instance of this identity. */
    if (outputs != 1 || inputs > 1)
        return 11;
    if (inputs == 0) {
        byte script[89];
        len = sizeof(script);
        if (load(LOAD_SCRIPT, script, &len, 0, 0, 0) || len != sizeof(script))
            return 12;
        byte input[36];
        len = sizeof(input);
        if (load(LOAD_INPUT_BY_FIELD, input, &len, 0, INPUT, 0) ||
            len != sizeof(input) || !eq(script + sizeof(script) - len, input, len))
            return 13;
    }
    return 0;
#elif RULE == 5
    /* A live object can advance one lifecycle step at a time. */
    if (inputs > 1 || outputs != 1)
        return 14;
    byte before = 0;
    byte after = 0;
    len = 1;
    if (load(LOAD_DATA, &after, &len, 0, GROUP_OUTPUT, 0) || len != 1)
        return 15;
    if (inputs == 0)
        return after != 0;
    len = 1;
    if (load(LOAD_DATA, &before, &len, 0, GROUP_INPUT, 0) || len != 1)
        return 16;
    return before == 255 || after != (byte)(before + 1);
#else
    u64 sum[2] = {0, 0};
    byte own_hash[32];
    len = sizeof(own_hash);
    if (load(LOAD_SCRIPT_HASH, own_hash, &len, 0, 0, 0) || len != sizeof(own_hash))
        return 17;
    for (u32 side = 0; side < 2; side++) {
        u64 source = side == 0 ? GROUP_INPUT : GROUP_OUTPUT;
        u32 count = side == 0 ? inputs : outputs;
        for (u32 i = 0; i < count; i++) {
            byte index_bytes[4];
            len = sizeof(index_bytes);
            if (load(LOAD_GROUP_INDEX, index_bytes, &len, i, source, 0) ||
                len != sizeof(index_bytes))
                return 18;
            u32 index = le32(index_bytes);
            u64 full_source = side == 0 ? INPUT : OUTPUT;
            byte size_bytes[4];
            len = sizeof(size_bytes);
            if (load(LOAD_TYPES, size_bytes, &len, index, full_source, 0) ||
                len != le32(size_bytes))
                return 19;
            byte member[128];
            len = sizeof(member);
            if (load(FIND_TYPE, member, &len, index, full_source, (u64)own_hash) ||
                len > sizeof(member))
                return 20;
            byte script_hash[32];
            int found = 0;
            for (u32 t = 0; t < 32; t++) {
                u64 script_len = sizeof(script_hash);
                u64 r = load(LOAD_TYPE_HASH, script_hash, &script_len, i, source, t);
                if (r == OUT_OF_BOUND)
                    break;
                if (r || script_len != sizeof(script_hash))
                    return 21;
                if (eq(script_hash, own_hash, sizeof(script_hash))) {
                    byte encoded[128];
                    script_len = sizeof(encoded);
                    if (load(LOAD_TYPE, encoded, &script_len, i, source, t) ||
                        script_len != len || !eq(encoded, member, len))
                        return 22;
                    found = 1;
                    break;
                }
            }
            if (!found)
                return 23;
            byte data[9];
            len = sizeof(data);
            if (load(LOAD_DATA, data, &len, i, source, 0) || len != sizeof(data))
                return 24;
            u64 amount = le64(data);
            if (amount == 0 || __builtin_add_overflow(sum[side], amount, &sum[side]))
                return 25;
#if RULE == 2
            /* Two CKB of capacity must be committed for each unit. */
            byte capacity[8];
            u64 required;
            len = sizeof(capacity);
            if (load(LOAD_CELL_FIELD, capacity, &len, i, source, 0) ||
                len != sizeof(capacity) ||
                __builtin_mul_overflow(amount, 200000000ULL, &required) ||
                le64(capacity) < required)
                return 26;
#elif RULE == 3
            /* Restricted assets may only use permitted classifications. */
            if (data[8] != 1)
                return 27;
#endif
        }
    }
#if RULE == 6
    /* Atomic settlement uses at least two positions in each direction. */
    if (inputs < 2 || outputs < 2)
        return 28;
#endif
    return sum[0] != sum[1];
#endif
}

__attribute__((noreturn)) void _start(void) {
    register u64 a0 __asm__("a0") = (u64)verify();
    register u64 a7 __asm__("a7") = 93;
    __asm__ volatile("ecall" : : "r"(a0), "r"(a7) : "memory");
    __builtin_unreachable();
}
