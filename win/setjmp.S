// setjmp/longjmp for blink on Windows x64 that don't unwind the stack.
// ucrt's longjmp unwinds with SEH, which fails across blink's JIT frames
// (they have no unwind info). Saves the Windows x64 callee-saved state:
// rbx rbp rdi rsi r12-r15 rsp rip mxcsr x87cw xmm6-xmm15 (jmp_buf is 256
// bytes, 16-byte aligned; see include/setjmp.h).
	.text
	.globl	blinkwin_setjmp
	.p2align 4
blinkwin_setjmp:
	mov	%rbx, 0(%rcx)
	mov	%rbp, 8(%rcx)
	mov	%rdi, 16(%rcx)
	mov	%rsi, 24(%rcx)
	mov	%r12, 32(%rcx)
	mov	%r13, 40(%rcx)
	mov	%r14, 48(%rcx)
	mov	%r15, 56(%rcx)
	lea	8(%rsp), %rdx
	mov	%rdx, 64(%rcx)
	mov	(%rsp), %rdx
	mov	%rdx, 72(%rcx)
	stmxcsr	80(%rcx)
	fnstcw	84(%rcx)
	movdqa	%xmm6, 96(%rcx)
	movdqa	%xmm7, 112(%rcx)
	movdqa	%xmm8, 128(%rcx)
	movdqa	%xmm9, 144(%rcx)
	movdqa	%xmm10, 160(%rcx)
	movdqa	%xmm11, 176(%rcx)
	movdqa	%xmm12, 192(%rcx)
	movdqa	%xmm13, 208(%rcx)
	movdqa	%xmm14, 224(%rcx)
	movdqa	%xmm15, 240(%rcx)
	xor	%eax, %eax
	ret

	.globl	blinkwin_longjmp
	.p2align 4
blinkwin_longjmp:
	mov	%edx, %eax
	test	%eax, %eax
	jnz	1f
	mov	$1, %eax
1:	mov	0(%rcx), %rbx
	mov	8(%rcx), %rbp
	mov	16(%rcx), %rdi
	mov	24(%rcx), %rsi
	mov	32(%rcx), %r12
	mov	40(%rcx), %r13
	mov	48(%rcx), %r14
	mov	56(%rcx), %r15
	ldmxcsr	80(%rcx)
	fldcw	84(%rcx)
	movdqa	96(%rcx), %xmm6
	movdqa	112(%rcx), %xmm7
	movdqa	128(%rcx), %xmm8
	movdqa	144(%rcx), %xmm9
	movdqa	160(%rcx), %xmm10
	movdqa	176(%rcx), %xmm11
	movdqa	192(%rcx), %xmm12
	movdqa	208(%rcx), %xmm13
	movdqa	224(%rcx), %xmm14
	movdqa	240(%rcx), %xmm15
	mov	64(%rcx), %rsp
	jmp	*72(%rcx)
