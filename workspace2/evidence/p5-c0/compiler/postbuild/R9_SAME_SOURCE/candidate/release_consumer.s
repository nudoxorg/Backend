	.build_version macos, 11, 0
	.section	__TEXT,__text,regular,pure_instructions
	.private_extern	__RINvNtCsa9BKTri5B3M_3std2rt10lang_startINtNtCsgtPOCBgevO_4core6result6ResultuNtCs6RePiqkWNcT_16release_consumer13ConsumerErrorEEB1f_
	.globl	__RINvNtCsa9BKTri5B3M_3std2rt10lang_startINtNtCsgtPOCBgevO_4core6result6ResultuNtCs6RePiqkWNcT_16release_consumer13ConsumerErrorEEB1f_
	.p2align	2
__RINvNtCsa9BKTri5B3M_3std2rt10lang_startINtNtCsgtPOCBgevO_4core6result6ResultuNtCs6RePiqkWNcT_16release_consumer13ConsumerErrorEEB1f_:
	.cfi_startproc
	sub	sp, sp, #32
	.cfi_def_cfa_offset 32
	stp	x29, x30, [sp, #16]
	add	x29, sp, #16
	.cfi_def_cfa w29, 16
	.cfi_offset w30, -8
	.cfi_offset w29, -16
	mov	x4, x3
	mov	x3, x2
	mov	x2, x1
	str	x0, [sp, #8]
Lloh0:
	adrp	x1, l_vtable.0@PAGE
Lloh1:
	add	x1, x1, l_vtable.0@PAGEOFF
	add	x0, sp, #8
	bl	__RNvNtCsa9BKTri5B3M_3std2rt19lang_start_internal
	.cfi_def_cfa wsp, 32
	ldp	x29, x30, [sp, #16]
	add	sp, sp, #32
	.cfi_def_cfa_offset 0
	.cfi_restore w30
	.cfi_restore w29
	ret
	.loh AdrpAdd	Lloh0, Lloh1
	.cfi_endproc

	.p2align	2
__RINvNtNtCsa9BKTri5B3M_3std3sys9backtrace28___rust_begin_short_backtraceFEINtNtCsgtPOCBgevO_4core6result6ResultuNtCs6RePiqkWNcT_16release_consumer13ConsumerErrorEB19_EB1N_:
	.cfi_startproc
	stp	x29, x30, [sp, #-16]!
	.cfi_def_cfa_offset 16
	mov	x29, sp
	.cfi_def_cfa w29, 16
	.cfi_offset w30, -8
	.cfi_offset w29, -16
	blr	x0
	; InlineAsm Start
	; InlineAsm End
	.cfi_def_cfa wsp, 16
	ldp	x29, x30, [sp], #16
	.cfi_def_cfa_offset 0
	.cfi_restore w30
	.cfi_restore w29
	ret
	.cfi_endproc

	.p2align	2
__RNCINvNtCsa9BKTri5B3M_3std2rt10lang_startINtNtCsgtPOCBgevO_4core6result6ResultuNtCs6RePiqkWNcT_16release_consumer13ConsumerErrorEE0B1h_:
	.cfi_startproc
	sub	sp, sp, #48
	.cfi_def_cfa_offset 48
	stp	x29, x30, [sp, #32]
	add	x29, sp, #32
	.cfi_def_cfa w29, 16
	.cfi_offset w30, -8
	.cfi_offset w29, -16
	ldr	x0, [x0]
	bl	__RINvNtNtCsa9BKTri5B3M_3std3sys9backtrace28___rust_begin_short_backtraceFEINtNtCsgtPOCBgevO_4core6result6ResultuNtCs6RePiqkWNcT_16release_consumer13ConsumerErrorEB19_EB1N_
	mov	w8, #255
	bics	wzr, w8, w0
	b.eq	LBB2_2
	strb	w0, [sp, #14]
	strb	w1, [sp, #15]
	add	x8, sp, #14
Lloh2:
	adrp	x9, __RNvXCs6RePiqkWNcT_16release_consumerNtB2_13ConsumerErrorNtNtCsgtPOCBgevO_4core3fmt5Debug3fmt@PAGE
Lloh3:
	add	x9, x9, __RNvXCs6RePiqkWNcT_16release_consumerNtB2_13ConsumerErrorNtNtCsgtPOCBgevO_4core3fmt5Debug3fmt@PAGEOFF
	stp	x8, x9, [sp, #16]
Lloh4:
	adrp	x0, l_alloc_26978532d6004174455ea2130c477035@PAGE
Lloh5:
	add	x0, x0, l_alloc_26978532d6004174455ea2130c477035@PAGEOFF
	add	x1, sp, #16
	bl	__RNvNtNtCsa9BKTri5B3M_3std2io5stdio23attempt_print_to_stderr
	mov	w0, #1
	b	LBB2_3
LBB2_2:
	mov	w0, #0
LBB2_3:
	.cfi_def_cfa wsp, 48
	ldp	x29, x30, [sp, #32]
	add	sp, sp, #48
	.cfi_def_cfa_offset 0
	.cfi_restore w30
	.cfi_restore w29
	ret
	.loh AdrpAdd	Lloh4, Lloh5
	.loh AdrpAdd	Lloh2, Lloh3
	.cfi_endproc

	.p2align	2
__RNSNvYNCINvNtCsa9BKTri5B3M_3std2rt10lang_startINtNtCsgtPOCBgevO_4core6result6ResultuNtCs6RePiqkWNcT_16release_consumer13ConsumerErrorEE0INtNtNtBN_3ops8function6FnOnceuE9call_once6vtableB1m_:
	.cfi_startproc
	sub	sp, sp, #48
	.cfi_def_cfa_offset 48
	stp	x29, x30, [sp, #32]
	add	x29, sp, #32
	.cfi_def_cfa w29, 16
	.cfi_offset w30, -8
	.cfi_offset w29, -16
	ldr	x0, [x0]
	bl	__RINvNtNtCsa9BKTri5B3M_3std3sys9backtrace28___rust_begin_short_backtraceFEINtNtCsgtPOCBgevO_4core6result6ResultuNtCs6RePiqkWNcT_16release_consumer13ConsumerErrorEB19_EB1N_
	mov	w8, #255
	bics	wzr, w8, w0
	b.eq	LBB3_2
	strb	w0, [sp, #14]
	strb	w1, [sp, #15]
	add	x8, sp, #14
Lloh6:
	adrp	x9, __RNvXCs6RePiqkWNcT_16release_consumerNtB2_13ConsumerErrorNtNtCsgtPOCBgevO_4core3fmt5Debug3fmt@PAGE
Lloh7:
	add	x9, x9, __RNvXCs6RePiqkWNcT_16release_consumerNtB2_13ConsumerErrorNtNtCsgtPOCBgevO_4core3fmt5Debug3fmt@PAGEOFF
	stp	x8, x9, [sp, #16]
Lloh8:
	adrp	x0, l_alloc_26978532d6004174455ea2130c477035@PAGE
Lloh9:
	add	x0, x0, l_alloc_26978532d6004174455ea2130c477035@PAGEOFF
	add	x1, sp, #16
	bl	__RNvNtNtCsa9BKTri5B3M_3std2io5stdio23attempt_print_to_stderr
	mov	w0, #1
	b	LBB3_3
LBB3_2:
	mov	w0, #0
LBB3_3:
	.cfi_def_cfa wsp, 48
	ldp	x29, x30, [sp, #32]
	add	sp, sp, #48
	.cfi_def_cfa_offset 0
	.cfi_restore w30
	.cfi_restore w29
	ret
	.loh AdrpAdd	Lloh8, Lloh9
	.loh AdrpAdd	Lloh6, Lloh7
	.cfi_endproc

	.p2align	2
__RNvCs6RePiqkWNcT_16release_consumer10rust_lower:
	.cfi_startproc
	sub	sp, sp, #64
	.cfi_def_cfa_offset 64
	stp	x22, x21, [sp, #16]
	stp	x20, x19, [sp, #32]
	stp	x29, x30, [sp, #48]
	add	x29, sp, #48
	.cfi_def_cfa w29, 16
	.cfi_offset w30, -8
	.cfi_offset w29, -16
	.cfi_offset w19, -24
	.cfi_offset w20, -32
	.cfi_offset w21, -40
	.cfi_offset w22, -48
	mov	x20, x2
	mov	x21, x1
	mov	x19, x0
	mov	x8, sp
	mov	w0, #0
	mov	w1, #1
	mov	x2, x21
	mov	x3, x20
	bl	__RNvMs2_Csbl94khd4Rel_22nudox_compile_registryNtB5_12FullRegistry8dispatch
	ldr	x8, [sp]
	cbz	x8, LBB4_3
	ldr	x9, [sp, #8]
	cmp	x8, x21
	ccmp	x9, x20, #0, eq
	b.eq	LBB4_5
	mov	w8, #2
	strb	w8, [x19, #8]
	b	LBB4_4
LBB4_3:
	ldrh	w8, [sp, #8]
	strh	w8, [x19, #8]
LBB4_4:
	str	xzr, [x19]
	b	LBB4_6
LBB4_5:
	stp	x8, x20, [x19]
LBB4_6:
	.cfi_def_cfa wsp, 64
	ldp	x29, x30, [sp, #48]
	ldp	x20, x19, [sp, #32]
	ldp	x22, x21, [sp, #16]
	add	sp, sp, #64
	.cfi_def_cfa_offset 0
	.cfi_restore w30
	.cfi_restore w29
	.cfi_restore w19
	.cfi_restore w20
	.cfi_restore w21
	.cfi_restore w22
	ret
	.cfi_endproc

	.p2align	2
__RNvCs6RePiqkWNcT_16release_consumer10rust_parse:
	.cfi_startproc
	sub	sp, sp, #64
	.cfi_def_cfa_offset 64
	stp	x22, x21, [sp, #16]
	stp	x20, x19, [sp, #32]
	stp	x29, x30, [sp, #48]
	add	x29, sp, #48
	.cfi_def_cfa w29, 16
	.cfi_offset w30, -8
	.cfi_offset w29, -16
	.cfi_offset w19, -24
	.cfi_offset w20, -32
	.cfi_offset w21, -40
	.cfi_offset w22, -48
	mov	x20, x2
	mov	x21, x1
	mov	x19, x0
	mov	x8, sp
	mov	w0, #0
	mov	w1, #0
	mov	x2, x21
	mov	x3, x20
	bl	__RNvMs2_Csbl94khd4Rel_22nudox_compile_registryNtB5_12FullRegistry8dispatch
	ldr	x8, [sp]
	cbz	x8, LBB5_3
	ldr	x9, [sp, #8]
	cmp	x8, x21
	ccmp	x9, x20, #0, eq
	b.eq	LBB5_5
	mov	w8, #2
	strb	w8, [x19, #8]
	b	LBB5_4
LBB5_3:
	ldrh	w8, [sp, #8]
	strh	w8, [x19, #8]
LBB5_4:
	str	xzr, [x19]
	b	LBB5_6
LBB5_5:
	stp	x8, x20, [x19]
LBB5_6:
	.cfi_def_cfa wsp, 64
	ldp	x29, x30, [sp, #48]
	ldp	x20, x19, [sp, #32]
	ldp	x22, x21, [sp, #16]
	add	sp, sp, #64
	.cfi_def_cfa_offset 0
	.cfi_restore w30
	.cfi_restore w29
	.cfi_restore w19
	.cfi_restore w20
	.cfi_restore w21
	.cfi_restore w22
	ret
	.cfi_endproc

	.p2align	2
__RNvCs6RePiqkWNcT_16release_consumer16typescript_parse:
	.cfi_startproc
	sub	sp, sp, #64
	.cfi_def_cfa_offset 64
	stp	x22, x21, [sp, #16]
	stp	x20, x19, [sp, #32]
	stp	x29, x30, [sp, #48]
	add	x29, sp, #48
	.cfi_def_cfa w29, 16
	.cfi_offset w30, -8
	.cfi_offset w29, -16
	.cfi_offset w19, -24
	.cfi_offset w20, -32
	.cfi_offset w21, -40
	.cfi_offset w22, -48
	mov	x20, x2
	mov	x21, x1
	mov	x19, x0
	mov	x8, sp
	mov	w0, #1
	mov	w1, #0
	mov	x2, x21
	mov	x3, x20
	bl	__RNvMs2_Csbl94khd4Rel_22nudox_compile_registryNtB5_12FullRegistry8dispatch
	ldr	x8, [sp]
	cbz	x8, LBB6_3
	ldr	x9, [sp, #8]
	cmp	x8, x21
	ccmp	x9, x20, #0, eq
	b.eq	LBB6_5
	mov	w8, #2
	strb	w8, [x19, #8]
	b	LBB6_4
LBB6_3:
	ldrh	w8, [sp, #8]
	strh	w8, [x19, #8]
LBB6_4:
	str	xzr, [x19]
	b	LBB6_6
LBB6_5:
	stp	x8, x20, [x19]
LBB6_6:
	.cfi_def_cfa wsp, 64
	ldp	x29, x30, [sp, #48]
	ldp	x20, x19, [sp, #32]
	ldp	x22, x21, [sp, #16]
	add	sp, sp, #64
	.cfi_def_cfa_offset 0
	.cfi_restore w30
	.cfi_restore w29
	.cfi_restore w19
	.cfi_restore w20
	.cfi_restore w21
	.cfi_restore w22
	ret
	.cfi_endproc

	.private_extern	__RNvCs6RePiqkWNcT_16release_consumer4main
	.globl	__RNvCs6RePiqkWNcT_16release_consumer4main
	.p2align	2
__RNvCs6RePiqkWNcT_16release_consumer4main:
	.cfi_startproc
	sub	sp, sp, #64
	.cfi_def_cfa_offset 64
	stp	x22, x21, [sp, #16]
	stp	x20, x19, [sp, #32]
	stp	x29, x30, [sp, #48]
	add	x29, sp, #48
	.cfi_def_cfa w29, 16
	.cfi_offset w30, -8
	.cfi_offset w29, -16
	.cfi_offset w19, -24
	.cfi_offset w20, -32
	.cfi_offset w21, -40
	.cfi_offset w22, -48
Lloh10:
	adrp	x8, l_alloc_19e1629a66736c3c0127852ef9f3276c@PAGE
Lloh11:
	add	x8, x8, l_alloc_19e1629a66736c3c0127852ef9f3276c@PAGEOFF
	mov	w9, #21
	stp	x8, x9, [sp]
	mov	x8, sp
	; InlineAsm Start
	; InlineAsm End
	ldp	x1, x2, [sp]
Lloh12:
	adrp	x8, l_alloc_c8ae94ea3405bd1d1572531fe5ad5793@PAGE
Lloh13:
	add	x8, x8, l_alloc_c8ae94ea3405bd1d1572531fe5ad5793@PAGEOFF
	stp	x8, x9, [sp]
	mov	x8, sp
	; InlineAsm Start
	; InlineAsm End
	ldp	x21, x22, [sp]
Lloh14:
	adrp	x8, l_alloc_052b66adb9d7d7d61473efdf8055f48b@PAGE
Lloh15:
	add	x8, x8, l_alloc_052b66adb9d7d7d61473efdf8055f48b@PAGEOFF
	mov	w9, #24
	stp	x8, x9, [sp]
	mov	x8, sp
	; InlineAsm Start
	; InlineAsm End
	ldp	x19, x20, [sp]
	mov	x0, sp
	bl	__RNvCs6RePiqkWNcT_16release_consumer10rust_parse
	ldr	x8, [sp]
	cbz	x8, LBB7_4
	ldr	x9, [sp, #8]
	stp	x8, x9, [sp]
	mov	x8, sp
	; InlineAsm Start
	; InlineAsm End
	mov	x0, sp
	mov	x1, x21
	mov	x2, x22
	bl	__RNvCs6RePiqkWNcT_16release_consumer10rust_lower
	ldr	x8, [sp]
	cbz	x8, LBB7_4
	ldr	x9, [sp, #8]
	stp	x8, x9, [sp]
	mov	x8, sp
	; InlineAsm Start
	; InlineAsm End
	mov	x0, sp
	mov	x1, x19
	mov	x2, x20
	bl	__RNvCs6RePiqkWNcT_16release_consumer16typescript_parse
	ldr	x8, [sp]
	cbz	x8, LBB7_4
	ldr	x9, [sp, #8]
	stp	x8, x9, [sp]
	mov	x8, sp
	; InlineAsm Start
	; InlineAsm End
	mov	w0, #255
	b	LBB7_5
LBB7_4:
	ldrb	w0, [sp, #8]
	ldrb	w1, [sp, #9]
LBB7_5:
	.cfi_def_cfa wsp, 64
	ldp	x29, x30, [sp, #48]
	ldp	x20, x19, [sp, #32]
	ldp	x22, x21, [sp, #16]
	add	sp, sp, #64
	.cfi_def_cfa_offset 0
	.cfi_restore w30
	.cfi_restore w29
	.cfi_restore w19
	.cfi_restore w20
	.cfi_restore w21
	.cfi_restore w22
	ret
	.loh AdrpAdd	Lloh14, Lloh15
	.loh AdrpAdd	Lloh12, Lloh13
	.loh AdrpAdd	Lloh10, Lloh11
	.cfi_endproc

	.p2align	2
__RNvXCs6RePiqkWNcT_16release_consumerNtB2_13ConsumerErrorNtNtCsgtPOCBgevO_4core3fmt5Debug3fmt:
	.cfi_startproc
	mov	x9, x1
	ldrb	w8, [x0]
	sub	x10, x8, #1
	cmp	w8, #1
	csel	x8, x10, xzr, hi
	cbz	x8, LBB8_3
	cmp	x8, #1
	b.ne	LBB8_4
Lloh16:
	adrp	x1, l_alloc_bac30f724c94d05a07257438bff897a2@PAGE
Lloh17:
	add	x1, x1, l_alloc_bac30f724c94d05a07257438bff897a2@PAGEOFF
	mov	x0, x9
	mov	w2, #16
	b	__RNvMsa_NtCsgtPOCBgevO_4core3fmtNtB5_9Formatter9write_str
LBB8_3:
	sub	sp, sp, #64
	.cfi_def_cfa_offset 64
	stp	x20, x19, [sp, #32]
	stp	x29, x30, [sp, #48]
	add	x29, sp, #48
	.cfi_def_cfa w29, 16
	.cfi_offset w30, -8
	.cfi_offset w29, -16
	.cfi_offset w19, -24
	.cfi_offset w20, -32
Lloh18:
	adrp	x1, l_alloc_f823262ef2001b6b275b03e1c243ec3b@PAGE
Lloh19:
	add	x1, x1, l_alloc_f823262ef2001b6b275b03e1c243ec3b@PAGEOFF
	add	x8, sp, #8
	mov	x19, x0
	mov	x0, x9
	mov	w2, #8
	bl	__RNvMsa_NtCsgtPOCBgevO_4core3fmtNtB5_9Formatter11debug_tuple
Lloh20:
	adrp	x2, l_vtable.1@PAGE
Lloh21:
	add	x2, x2, l_vtable.1@PAGEOFF
	add	x0, sp, #8
	mov	x1, x19
	bl	__RNvMs2_NtNtCsgtPOCBgevO_4core3fmt8buildersNtB5_10DebugTuple5field
	bl	__RNvMs2_NtNtCsgtPOCBgevO_4core3fmt8buildersNtB5_10DebugTuple6finish
	.cfi_def_cfa wsp, 64
	ldp	x29, x30, [sp, #48]
	ldp	x20, x19, [sp, #32]
	add	sp, sp, #64
	.cfi_def_cfa_offset 0
	.cfi_restore w30
	.cfi_restore w29
	.cfi_restore w19
	.cfi_restore w20
	ret
LBB8_4:
Lloh22:
	adrp	x1, l_alloc_051699efd7a6f01d4870ec6de7d9c676@PAGE
Lloh23:
	add	x1, x1, l_alloc_051699efd7a6f01d4870ec6de7d9c676@PAGEOFF
	mov	x0, x9
	mov	w2, #15
	b	__RNvMsa_NtCsgtPOCBgevO_4core3fmtNtB5_9Formatter9write_str
	.loh AdrpAdd	Lloh16, Lloh17
	.loh AdrpAdd	Lloh20, Lloh21
	.loh AdrpAdd	Lloh18, Lloh19
	.loh AdrpAdd	Lloh22, Lloh23
	.cfi_endproc

	.p2align	2
__RNvXs1_Cs4z5X8QAwfxR_19nudox_compile_vocabNtB5_8LanguageNtNtCsgtPOCBgevO_4core3fmt5Debug3fmt:
	.cfi_startproc
	mov	x8, x1
	ldrb	w9, [x0]
	cmp	w9, #0
	mov	w9, #10
	mov	w10, #16
	csel	x2, x10, x9, ne
Lloh24:
	adrp	x9, l_alloc_f16b25e52a01538245d98357ad63ee2c@PAGE
Lloh25:
	add	x9, x9, l_alloc_f16b25e52a01538245d98357ad63ee2c@PAGEOFF
Lloh26:
	adrp	x10, l_alloc_9f9bfae2a1c54e2e9245008f77214b29@PAGE
Lloh27:
	add	x10, x10, l_alloc_9f9bfae2a1c54e2e9245008f77214b29@PAGEOFF
	csel	x1, x10, x9, ne
	mov	x0, x8
	b	__RNvMsa_NtCsgtPOCBgevO_4core3fmtNtB5_9Formatter9write_str
	.loh AdrpAdd	Lloh26, Lloh27
	.loh AdrpAdd	Lloh24, Lloh25
	.cfi_endproc

	.p2align	2
__RNvXs1g_NtCsgtPOCBgevO_4core3fmtRNtCs4z5X8QAwfxR_19nudox_compile_vocab5StageNtB6_5Debug3fmtCs6RePiqkWNcT_16release_consumer:
	.cfi_startproc
	mov	x8, x1
	ldr	x9, [x0]
	ldrb	w9, [x9]
	cmp	w9, #0
	mov	w9, #5
	mov	w10, #7
	csel	x2, x10, x9, ne
Lloh28:
	adrp	x9, l_alloc_4cf7911ffc1fa65c3bf9af4755f6af39@PAGE
Lloh29:
	add	x9, x9, l_alloc_4cf7911ffc1fa65c3bf9af4755f6af39@PAGEOFF
Lloh30:
	adrp	x10, l_alloc_98a73cdd092a18d6389fd557415299d7@PAGE
Lloh31:
	add	x10, x10, l_alloc_98a73cdd092a18d6389fd557415299d7@PAGEOFF
	csel	x1, x10, x9, ne
	mov	x0, x8
	b	__RNvMsa_NtCsgtPOCBgevO_4core3fmtNtB5_9Formatter9write_str
	.loh AdrpAdd	Lloh30, Lloh31
	.loh AdrpAdd	Lloh28, Lloh29
	.cfi_endproc

	.p2align	2
__RNvXsf_Cs4z5X8QAwfxR_19nudox_compile_vocabNtB5_13FrontendErrorNtNtCsgtPOCBgevO_4core3fmt5Debug3fmt:
	.cfi_startproc
	sub	sp, sp, #48
	.cfi_def_cfa_offset 48
	stp	x29, x30, [sp, #32]
	add	x29, sp, #32
	.cfi_def_cfa w29, 16
	.cfi_offset w30, -8
	.cfi_offset w29, -16
	mov	x8, x1
	mov	x5, x0
	add	x9, x0, #1
	stur	x9, [x29, #-8]
Lloh32:
	adrp	x9, l_vtable.3@PAGE
Lloh33:
	add	x11, x9, l_vtable.3@PAGEOFF
	sub	x9, x29, #8
	mov	w10, #5
	stp	x9, x11, [sp, #8]
	str	x10, [sp]
Lloh34:
	adrp	x1, l_alloc_1dc2bfd5907371d8024f4a89167a40c3@PAGE
Lloh35:
	add	x1, x1, l_alloc_1dc2bfd5907371d8024f4a89167a40c3@PAGEOFF
Lloh36:
	adrp	x3, l_alloc_6804fcd76d287f42ed3f019f5ce179ec@PAGE
Lloh37:
	add	x3, x3, l_alloc_6804fcd76d287f42ed3f019f5ce179ec@PAGEOFF
Lloh38:
	adrp	x6, l_vtable.2@PAGE
Lloh39:
	add	x6, x6, l_vtable.2@PAGEOFF
Lloh40:
	adrp	x7, l_alloc_39819854344d49e678f457feac9e6644@PAGE
Lloh41:
	add	x7, x7, l_alloc_39819854344d49e678f457feac9e6644@PAGEOFF
	mov	x0, x8
	mov	w2, #16
	mov	w4, #8
	bl	__RNvMsa_NtCsgtPOCBgevO_4core3fmtNtB5_9Formatter26debug_struct_field2_finish
	.cfi_def_cfa wsp, 48
	ldp	x29, x30, [sp, #32]
	add	sp, sp, #48
	.cfi_def_cfa_offset 0
	.cfi_restore w30
	.cfi_restore w29
	ret
	.loh AdrpAdd	Lloh40, Lloh41
	.loh AdrpAdd	Lloh38, Lloh39
	.loh AdrpAdd	Lloh36, Lloh37
	.loh AdrpAdd	Lloh34, Lloh35
	.loh AdrpAdd	Lloh32, Lloh33
	.cfi_endproc

	.globl	_main
	.p2align	2
_main:
	.cfi_startproc
	sub	sp, sp, #32
	stp	x29, x30, [sp, #16]
	add	x29, sp, #16
	.cfi_def_cfa w29, 16
	.cfi_offset w30, -8
	.cfi_offset w29, -16
	mov	x3, x1
	sxtw	x2, w0
Lloh42:
	adrp	x8, __RNvCs6RePiqkWNcT_16release_consumer4main@PAGE
Lloh43:
	add	x8, x8, __RNvCs6RePiqkWNcT_16release_consumer4main@PAGEOFF
	str	x8, [sp, #8]
Lloh44:
	adrp	x1, l_vtable.0@PAGE
Lloh45:
	add	x1, x1, l_vtable.0@PAGEOFF
	add	x0, sp, #8
	mov	w4, #0
	bl	__RNvNtCsa9BKTri5B3M_3std2rt19lang_start_internal
	ldp	x29, x30, [sp, #16]
	add	sp, sp, #32
	ret
	.loh AdrpAdd	Lloh44, Lloh45
	.loh AdrpAdd	Lloh42, Lloh43
	.cfi_endproc

	.section	__DATA,__const
	.p2align	3, 0x0
l_vtable.0:
	.asciz	"\000\000\000\000\000\000\000\000\b\000\000\000\000\000\000\000\b\000\000\000\000\000\000"
	.quad	__RNSNvYNCINvNtCsa9BKTri5B3M_3std2rt10lang_startINtNtCsgtPOCBgevO_4core6result6ResultuNtCs6RePiqkWNcT_16release_consumer13ConsumerErrorEE0INtNtNtBN_3ops8function6FnOnceuE9call_once6vtableB1m_
	.quad	__RNCINvNtCsa9BKTri5B3M_3std2rt10lang_startINtNtCsgtPOCBgevO_4core6result6ResultuNtCs6RePiqkWNcT_16release_consumer13ConsumerErrorEE0B1h_
	.quad	__RNCINvNtCsa9BKTri5B3M_3std2rt10lang_startINtNtCsgtPOCBgevO_4core6result6ResultuNtCs6RePiqkWNcT_16release_consumer13ConsumerErrorEE0B1h_

	.section	__TEXT,__const
l_alloc_19e1629a66736c3c0127852ef9f3276c:
	.ascii	"fn release_parse() {}"

l_alloc_c8ae94ea3405bd1d1572531fe5ad5793:
	.ascii	"fn release_lower() {}"

l_alloc_052b66adb9d7d7d61473efdf8055f48b:
	.ascii	"const release_parse = 1;"

	.section	__TEXT,__literal8,8byte_literals
l_alloc_f823262ef2001b6b275b03e1c243ec3b:
	.ascii	"Frontend"

	.section	__DATA,__const
	.p2align	3, 0x0
l_vtable.1:
	.asciz	"\000\000\000\000\000\000\000\000\002\000\000\000\000\000\000\000\001\000\000\000\000\000\000"
	.quad	__RNvXsf_Cs4z5X8QAwfxR_19nudox_compile_vocabNtB5_13FrontendErrorNtNtCsgtPOCBgevO_4core3fmt5Debug3fmt

	.section	__TEXT,__literal16,16byte_literals
l_alloc_bac30f724c94d05a07257438bff897a2:
	.ascii	"pointer mismatch"

	.section	__TEXT,__const
l_alloc_051699efd7a6f01d4870ec6de7d9c676:
	.ascii	"length mismatch"

	.section	__TEXT,__cstring,cstring_literals
l_alloc_26978532d6004174455ea2130c477035:
	.asciz	"\007Error: \300\001\n"

	.section	__TEXT,__const
l_alloc_f16b25e52a01538245d98357ad63ee2c:
	.ascii	"RustSubset"

	.section	__TEXT,__literal16,16byte_literals
l_alloc_9f9bfae2a1c54e2e9245008f77214b29:
	.ascii	"TypeScriptSubset"

	.section	__TEXT,__const
l_alloc_4cf7911ffc1fa65c3bf9af4755f6af39:
	.ascii	"Parse"

l_alloc_98a73cdd092a18d6389fd557415299d7:
	.ascii	"LowerIr"

	.section	__DATA,__const
	.p2align	3, 0x0
l_vtable.2:
	.asciz	"\000\000\000\000\000\000\000\000\001\000\000\000\000\000\000\000\001\000\000\000\000\000\000"
	.quad	__RNvXs1_Cs4z5X8QAwfxR_19nudox_compile_vocabNtB5_8LanguageNtNtCsgtPOCBgevO_4core3fmt5Debug3fmt

	.p2align	3, 0x0
l_vtable.3:
	.asciz	"\000\000\000\000\000\000\000\000\b\000\000\000\000\000\000\000\b\000\000\000\000\000\000"
	.quad	__RNvXs1g_NtCsgtPOCBgevO_4core3fmtRNtCs4z5X8QAwfxR_19nudox_compile_vocab5StageNtB6_5Debug3fmtCs6RePiqkWNcT_16release_consumer

	.section	__TEXT,__literal16,16byte_literals
l_alloc_1dc2bfd5907371d8024f4a89167a40c3:
	.ascii	"UnsupportedStage"

	.section	__TEXT,__literal8,8byte_literals
l_alloc_6804fcd76d287f42ed3f019f5ce179ec:
	.ascii	"language"

	.section	__TEXT,__const
l_alloc_39819854344d49e678f457feac9e6644:
	.ascii	"stage"

.subsections_via_symbols
