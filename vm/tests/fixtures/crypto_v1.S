.section .text
.globl _start
.macro check op, src, len, status
    li a0, \op
    la a1, \src
    li a2, \len
    la a3, result
    li a7, 2131
    ecall
    li t0, \status
    bne a0, t0, fail
.endm

.macro compare_hash exp
    la t0, result
    la t1, \exp
    li t2, 32
1:
    lbu t3, 0(t0)
    lbu t4, 0(t1)
    bne t3, t4, fail
    addi t0, t0, 1
    addi t1, t1, 1
    addi t2, t2, -1
    bnez t2, 1b
.endm

_start:
    check 0, message, 32, 0
    compare_hash expected
    check 0, message, 0, 0
    compare_hash empty_hash
    check 0, zeros, 1048576, 0
    compare_hash large_hash

    check 1, secp, 129, 0
    check 1, secp, 8256, 0
    check 2, ed, 128, 0
    check 2, ed, 8192, 0
    check 1, secp_bad, 258, 4
    check 2, ed_bad, 256, 4
    check 1, high_s, 129, 4
    check 1, secp, 128, 4
    check 2, ed, 0, 4
    check 1, secp, 8385, 4
    check 2, ed, 8320, 4
    check 0, message, 1048577, 4
    check 7, message, 0, 4
    li a0, 0
    j exit
fail:
    li a0, 1
exit:
    li a7, 93
    ecall

.section .rodata
message: .incbin "message"
expected: .incbin "expected"
secp: .incbin "secp"
ed: .incbin "ed"
secp_bad: .incbin "secp_bad"
ed_bad: .incbin "ed_bad"
high_s: .incbin "high_s"
empty_hash: .incbin "empty_hash"
large_hash: .incbin "large_hash"
.section .bss
zeros: .space 1048576
result: .space 32
