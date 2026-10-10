#ifndef BLINK_INTRIN_H_
#define BLINK_INTRIN_H_

// clang (including clang-cl and zig cc) supports the same GNU vector
// extensions and inline asm, but reports __GNUC__ as 4 or not at all
#if defined(__x86_64__) && \
    ((defined(__GNUC__) && __GNUC__ >= 6) || defined(__clang__))
#define X86_INTRINSICS 1
typedef char char_xmmu_t __attribute__((__vector_size__(16), __may_alias__));
typedef char char_xmma_t
    __attribute__((__vector_size__(16), __aligned__(16), __may_alias__));
#else
#define X86_INTRINSICS 0
#endif
#if defined(__aarch64__) && (defined(__GNUC__) || defined(__clang__))
#define ARM_INTRINSICS 1
#else
#define ARM_INTRINSICS 0
#endif

#endif /* BLINK_INTRIN_H_ */
