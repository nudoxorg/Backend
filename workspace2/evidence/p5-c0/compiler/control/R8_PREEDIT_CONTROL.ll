; ModuleID = 'release_consumer.4fe331842fe10c15-cgu.0'
source_filename = "release_consumer.4fe331842fe10c15-cgu.0"
target datalayout = "e-m:o-p270:32:32-p271:32:32-p272:64:64-i64:64-i128:128-n32:64-S128-Fn32"
target triple = "arm64-apple-macosx11.0.0"

@vtable.0 = private unnamed_addr constant <{ [24 x i8], ptr, ptr, ptr }> <{ [24 x i8] c"\00\00\00\00\00\00\00\00\08\00\00\00\00\00\00\00\08\00\00\00\00\00\00\00", ptr @_RNSNvYNCINvNtCsa9BKTri5B3M_3std2rt10lang_startINtNtCsgtPOCBgevO_4core6result6ResultuNtCs6RePiqkWNcT_16release_consumer13ConsumerErrorEE0INtNtNtBN_3ops8function6FnOnceuE9call_once6vtableB1m_, ptr @_RNCINvNtCsa9BKTri5B3M_3std2rt10lang_startINtNtCsgtPOCBgevO_4core6result6ResultuNtCs6RePiqkWNcT_16release_consumer13ConsumerErrorEE0B1h_, ptr @_RNCINvNtCsa9BKTri5B3M_3std2rt10lang_startINtNtCsgtPOCBgevO_4core6result6ResultuNtCs6RePiqkWNcT_16release_consumer13ConsumerErrorEE0B1h_ }>, align 8
@alloc_19e1629a66736c3c0127852ef9f3276c = private unnamed_addr constant [21 x i8] c"fn release_parse() {}", align 1
@alloc_c8ae94ea3405bd1d1572531fe5ad5793 = private unnamed_addr constant [21 x i8] c"fn release_lower() {}", align 1
@alloc_052b66adb9d7d7d61473efdf8055f48b = private unnamed_addr constant [24 x i8] c"const release_parse = 1;", align 1
@alloc_f823262ef2001b6b275b03e1c243ec3b = private unnamed_addr constant [8 x i8] c"Frontend", align 1
@vtable.1 = private unnamed_addr constant <{ [24 x i8], ptr }> <{ [24 x i8] c"\00\00\00\00\00\00\00\00\02\00\00\00\00\00\00\00\01\00\00\00\00\00\00\00", ptr @_RNvXsf_Cs5UHFsggAg5t_19nudox_compile_vocabNtB5_13FrontendErrorNtNtCsgtPOCBgevO_4core3fmt5Debug3fmt }>, align 8
@alloc_bac30f724c94d05a07257438bff897a2 = private unnamed_addr constant [16 x i8] c"pointer mismatch", align 1
@alloc_051699efd7a6f01d4870ec6de7d9c676 = private unnamed_addr constant [15 x i8] c"length mismatch", align 1
@alloc_26978532d6004174455ea2130c477035 = private unnamed_addr constant [12 x i8] c"\07Error: \C0\01\0A\00", align 1
@alloc_f16b25e52a01538245d98357ad63ee2c = private unnamed_addr constant [10 x i8] c"RustSubset", align 1
@alloc_9f9bfae2a1c54e2e9245008f77214b29 = private unnamed_addr constant [16 x i8] c"TypeScriptSubset", align 1
@alloc_4cf7911ffc1fa65c3bf9af4755f6af39 = private unnamed_addr constant [5 x i8] c"Parse", align 1
@alloc_98a73cdd092a18d6389fd557415299d7 = private unnamed_addr constant [7 x i8] c"LowerIr", align 1
@vtable.2 = private unnamed_addr constant <{ [24 x i8], ptr }> <{ [24 x i8] c"\00\00\00\00\00\00\00\00\01\00\00\00\00\00\00\00\01\00\00\00\00\00\00\00", ptr @_RNvXs1_Cs5UHFsggAg5t_19nudox_compile_vocabNtB5_8LanguageNtNtCsgtPOCBgevO_4core3fmt5Debug3fmt }>, align 8
@vtable.3 = private unnamed_addr constant <{ [24 x i8], ptr }> <{ [24 x i8] c"\00\00\00\00\00\00\00\00\08\00\00\00\00\00\00\00\08\00\00\00\00\00\00\00", ptr @_RNvXs1g_NtCsgtPOCBgevO_4core3fmtRNtCs5UHFsggAg5t_19nudox_compile_vocab5StageNtB6_5Debug3fmtCs6RePiqkWNcT_16release_consumer }>, align 8
@alloc_1dc2bfd5907371d8024f4a89167a40c3 = private unnamed_addr constant [16 x i8] c"UnsupportedStage", align 1
@alloc_6804fcd76d287f42ed3f019f5ce179ec = private unnamed_addr constant [8 x i8] c"language", align 1
@alloc_39819854344d49e678f457feac9e6644 = private unnamed_addr constant [5 x i8] c"stage", align 1

; std::rt::lang_start::<core::result::Result<(), release_consumer::ConsumerError>>
; Function Attrs: uwtable
define hidden noundef i64 @_RINvNtCsa9BKTri5B3M_3std2rt10lang_startINtNtCsgtPOCBgevO_4core6result6ResultuNtCs6RePiqkWNcT_16release_consumer13ConsumerErrorEEB1f_(ptr noundef nonnull %main, i64 noundef %argc, ptr noundef %argv, i8 noundef %sigpipe) unnamed_addr #0 {
start:
  %_7 = alloca [8 x i8], align 8
  call void @llvm.lifetime.start.p0(ptr nonnull %_7)
  store ptr %main, ptr %_7, align 8
; call std::rt::lang_start_internal
  %_0 = call noundef i64 @_RNvNtCsa9BKTri5B3M_3std2rt19lang_start_internal(ptr noundef nonnull %_7, ptr noalias noundef readonly align 8 captures(address, read_provenance) dereferenceable(48) @vtable.0, i64 noundef %argc, ptr noundef %argv, i8 noundef %sigpipe)
  call void @llvm.lifetime.end.p0(ptr nonnull %_7)
  ret i64 %_0
}

; std::sys::backtrace::__rust_begin_short_backtrace::<fn() -> core::result::Result<(), release_consumer::ConsumerError>, core::result::Result<(), release_consumer::ConsumerError>>
; Function Attrs: noinline uwtable
define internal fastcc { i8, i8 } @_RINvNtNtCsa9BKTri5B3M_3std3sys9backtrace28___rust_begin_short_backtraceFEINtNtCsgtPOCBgevO_4core6result6ResultuNtCs6RePiqkWNcT_16release_consumer13ConsumerErrorEB19_EB1N_(ptr noundef nonnull readonly captures(none) %f) unnamed_addr #1 {
start:
  %0 = tail call { i8, i8 } %f()
  tail call void asm sideeffect "", "~{memory}"() #6, !srcloc !3
  ret { i8, i8 } %0
}

; std::rt::lang_start::<core::result::Result<(), release_consumer::ConsumerError>>::{closure#0}
; Function Attrs: inlinehint uwtable
define internal noundef range(i32 0, 2) i32 @_RNCINvNtCsa9BKTri5B3M_3std2rt10lang_startINtNtCsgtPOCBgevO_4core6result6ResultuNtCs6RePiqkWNcT_16release_consumer13ConsumerErrorEE0B1h_(ptr noalias noundef readonly align 8 captures(none) dereferenceable(8) %_1) unnamed_addr #2 personality ptr @rust_eh_personality {
start:
  %args.i = alloca [16 x i8], align 8
  %err.i = alloca [2 x i8], align 1
  %_4 = load ptr, ptr %_1, align 8, !nonnull !4, !noundef !4
; call std::sys::backtrace::__rust_begin_short_backtrace::<fn() -> core::result::Result<(), release_consumer::ConsumerError>, core::result::Result<(), release_consumer::ConsumerError>>
  %0 = tail call fastcc { i8, i8 } @_RINvNtNtCsa9BKTri5B3M_3std3sys9backtrace28___rust_begin_short_backtraceFEINtNtCsgtPOCBgevO_4core6result6ResultuNtCs6RePiqkWNcT_16release_consumer13ConsumerErrorEB19_EB1N_(ptr noundef nonnull %_4)
  %_3.0 = extractvalue { i8, i8 } %0, 0
  %.not.i = icmp eq i8 %_3.0, -1
  br i1 %.not.i, label %_RNvXs13_NtCsa9BKTri5B3M_3std7processINtNtCsgtPOCBgevO_4core6result6ResultuNtCs6RePiqkWNcT_16release_consumer13ConsumerErrorENtB6_11Termination6reportB1c_.exit, label %bb2.i

bb2.i:                                            ; preds = %start
  %_3.1 = extractvalue { i8, i8 } %0, 1
  call void @llvm.lifetime.start.p0(ptr nonnull %err.i)
  store i8 %_3.0, ptr %err.i, align 1
  %1 = getelementptr inbounds nuw i8, ptr %err.i, i64 1
  store i8 %_3.1, ptr %1, align 1
  call void @llvm.lifetime.start.p0(ptr nonnull %args.i)
  store ptr %err.i, ptr %args.i, align 8
  %_9.sroa.4.0..sroa_idx.i = getelementptr inbounds nuw i8, ptr %args.i, i64 8
  store ptr @_RNvXCs6RePiqkWNcT_16release_consumerNtB2_13ConsumerErrorNtNtCsgtPOCBgevO_4core3fmt5Debug3fmt, ptr %_9.sroa.4.0..sroa_idx.i, align 8
; call std::io::stdio::attempt_print_to_stderr
  call void @_RNvNtNtCsa9BKTri5B3M_3std2io5stdio23attempt_print_to_stderr(ptr noundef nonnull @alloc_26978532d6004174455ea2130c477035, ptr noundef nonnull %args.i)
  call void @llvm.lifetime.end.p0(ptr nonnull %args.i)
  call void @llvm.lifetime.end.p0(ptr nonnull %err.i)
  br label %_RNvXs13_NtCsa9BKTri5B3M_3std7processINtNtCsgtPOCBgevO_4core6result6ResultuNtCs6RePiqkWNcT_16release_consumer13ConsumerErrorENtB6_11Termination6reportB1c_.exit

_RNvXs13_NtCsa9BKTri5B3M_3std7processINtNtCsgtPOCBgevO_4core6result6ResultuNtCs6RePiqkWNcT_16release_consumer13ConsumerErrorENtB6_11Termination6reportB1c_.exit: ; preds = %start, %bb2.i
  %_0.sroa.0.0.i = phi i32 [ 1, %bb2.i ], [ 0, %start ]
  ret i32 %_0.sroa.0.0.i
}

; <std::rt::lang_start<core::result::Result<(), release_consumer::ConsumerError>>::{closure#0} as core::ops::function::FnOnce<()>>::call_once::{shim:vtable#0}
; Function Attrs: inlinehint uwtable
define internal noundef range(i32 0, 2) i32 @_RNSNvYNCINvNtCsa9BKTri5B3M_3std2rt10lang_startINtNtCsgtPOCBgevO_4core6result6ResultuNtCs6RePiqkWNcT_16release_consumer13ConsumerErrorEE0INtNtNtBN_3ops8function6FnOnceuE9call_once6vtableB1m_(ptr noundef readonly captures(none) %_1) unnamed_addr #2 personality ptr @rust_eh_personality {
start:
  %args.i.i.i = alloca [16 x i8], align 8
  %err.i.i.i = alloca [2 x i8], align 1
  %0 = load ptr, ptr %_1, align 8, !nonnull !4, !noundef !4
; call std::sys::backtrace::__rust_begin_short_backtrace::<fn() -> core::result::Result<(), release_consumer::ConsumerError>, core::result::Result<(), release_consumer::ConsumerError>>
  %1 = tail call fastcc { i8, i8 } @_RINvNtNtCsa9BKTri5B3M_3std3sys9backtrace28___rust_begin_short_backtraceFEINtNtCsgtPOCBgevO_4core6result6ResultuNtCs6RePiqkWNcT_16release_consumer13ConsumerErrorEB19_EB1N_(ptr noundef nonnull readonly %0), !noalias !5
  %_3.0.i.i = extractvalue { i8, i8 } %1, 0
  %.not.i.i.i = icmp eq i8 %_3.0.i.i, -1
  br i1 %.not.i.i.i, label %_RNvYNCINvNtCsa9BKTri5B3M_3std2rt10lang_startINtNtCsgtPOCBgevO_4core6result6ResultuNtCs6RePiqkWNcT_16release_consumer13ConsumerErrorEE0INtNtNtBL_3ops8function6FnOnceuE9call_onceB1k_.exit, label %bb2.i.i.i

bb2.i.i.i:                                        ; preds = %start
  %_3.1.i.i = extractvalue { i8, i8 } %1, 1
  call void @llvm.lifetime.start.p0(ptr nonnull %err.i.i.i), !noalias !5
  store i8 %_3.0.i.i, ptr %err.i.i.i, align 1, !noalias !5
  %2 = getelementptr inbounds nuw i8, ptr %err.i.i.i, i64 1
  store i8 %_3.1.i.i, ptr %2, align 1, !noalias !5
  call void @llvm.lifetime.start.p0(ptr nonnull %args.i.i.i), !noalias !5
  store ptr %err.i.i.i, ptr %args.i.i.i, align 8, !noalias !5
  %_9.sroa.4.0..sroa_idx.i.i.i = getelementptr inbounds nuw i8, ptr %args.i.i.i, i64 8
  store ptr @_RNvXCs6RePiqkWNcT_16release_consumerNtB2_13ConsumerErrorNtNtCsgtPOCBgevO_4core3fmt5Debug3fmt, ptr %_9.sroa.4.0..sroa_idx.i.i.i, align 8, !noalias !5
; call std::io::stdio::attempt_print_to_stderr
  call void @_RNvNtNtCsa9BKTri5B3M_3std2io5stdio23attempt_print_to_stderr(ptr noundef nonnull @alloc_26978532d6004174455ea2130c477035, ptr noundef nonnull %args.i.i.i), !noalias !5
  call void @llvm.lifetime.end.p0(ptr nonnull %args.i.i.i), !noalias !5
  call void @llvm.lifetime.end.p0(ptr nonnull %err.i.i.i), !noalias !5
  br label %_RNvYNCINvNtCsa9BKTri5B3M_3std2rt10lang_startINtNtCsgtPOCBgevO_4core6result6ResultuNtCs6RePiqkWNcT_16release_consumer13ConsumerErrorEE0INtNtNtBL_3ops8function6FnOnceuE9call_onceB1k_.exit

_RNvYNCINvNtCsa9BKTri5B3M_3std2rt10lang_startINtNtCsgtPOCBgevO_4core6result6ResultuNtCs6RePiqkWNcT_16release_consumer13ConsumerErrorEE0INtNtNtBL_3ops8function6FnOnceuE9call_onceB1k_.exit: ; preds = %start, %bb2.i.i.i
  %_0.sroa.0.0.i.i.i = phi i32 [ 1, %bb2.i.i.i ], [ 0, %start ]
  ret i32 %_0.sroa.0.0.i.i.i
}

; release_consumer::rust_lower
; Function Attrs: noinline uwtable
define internal fastcc void @_RNvCs6RePiqkWNcT_16release_consumer10rust_lower(ptr dead_on_unwind noalias noundef nonnull writable writeonly align 8 captures(none) dereferenceable(16) initializes((0, 9)) %_0, ptr noalias noundef nonnull readonly captures(address, read_provenance) %source.0, i64 noundef range(i64 0, -9223372036854775808) %source.1) unnamed_addr #1 {
start:
  %_2 = alloca [16 x i8], align 8
  call void @llvm.lifetime.start.p0(ptr nonnull %_2)
; call <nudox_compile_registry::FullRegistry>::dispatch
  call void @_RNvMs0_CsjeakGkMLXDR_22nudox_compile_registryNtB5_12FullRegistry8dispatch(ptr noalias noundef nonnull sret([16 x i8]) align 8 captures(address) dereferenceable(16) %_2, i1 noundef zeroext false, i1 noundef zeroext true, ptr noalias noundef nonnull readonly captures(address, read_provenance) %source.0, i64 noundef %source.1)
  %0 = load ptr, ptr %_2, align 8, !noundef !4
  %1 = icmp eq ptr %0, null
  %2 = getelementptr inbounds nuw i8, ptr %_2, i64 8
  br i1 %1, label %bb4, label %bb5

bb4:                                              ; preds = %start
  %3 = load i8, ptr %2, align 8, !range !8, !noundef !4
  %4 = getelementptr inbounds nuw i8, ptr %_2, i64 9
  %5 = load i8, ptr %4, align 1, !range !8, !noundef !4
  %6 = getelementptr inbounds nuw i8, ptr %_0, i64 8
  store i8 %3, ptr %6, align 8
  %7 = getelementptr inbounds nuw i8, ptr %_0, i64 9
  store i8 %5, ptr %7, align 1
  store ptr null, ptr %_0, align 8
  br label %bb2

bb5:                                              ; preds = %start
  %_4.1 = load i64, ptr %2, align 8, !noundef !4
  %8 = icmp eq ptr %0, %source.0
  %9 = icmp eq i64 %_4.1, %source.1
  %_7 = and i1 %8, %9
  br i1 %_7, label %bb9, label %bb7

bb7:                                              ; preds = %bb5
  %10 = getelementptr inbounds nuw i8, ptr %_0, i64 8
  store i8 2, ptr %10, align 8
  store ptr null, ptr %_0, align 8
  br label %bb2

bb2:                                              ; preds = %bb4, %bb9, %bb7
  call void @llvm.lifetime.end.p0(ptr nonnull %_2)
  ret void

bb9:                                              ; preds = %bb5
  store ptr %0, ptr %_0, align 8
  %11 = getelementptr inbounds nuw i8, ptr %_0, i64 8
  store i64 %source.1, ptr %11, align 8
  br label %bb2
}

; release_consumer::rust_parse
; Function Attrs: noinline uwtable
define internal fastcc void @_RNvCs6RePiqkWNcT_16release_consumer10rust_parse(ptr dead_on_unwind noalias noundef nonnull writable writeonly align 8 captures(none) dereferenceable(16) initializes((0, 9)) %_0, ptr noalias noundef nonnull readonly captures(address, read_provenance) %source.0, i64 noundef range(i64 0, -9223372036854775808) %source.1) unnamed_addr #1 {
start:
  %_2 = alloca [16 x i8], align 8
  call void @llvm.lifetime.start.p0(ptr nonnull %_2)
; call <nudox_compile_registry::FullRegistry>::dispatch
  call void @_RNvMs0_CsjeakGkMLXDR_22nudox_compile_registryNtB5_12FullRegistry8dispatch(ptr noalias noundef nonnull sret([16 x i8]) align 8 captures(address) dereferenceable(16) %_2, i1 noundef zeroext false, i1 noundef zeroext false, ptr noalias noundef nonnull readonly captures(address, read_provenance) %source.0, i64 noundef %source.1)
  %0 = load ptr, ptr %_2, align 8, !noundef !4
  %1 = icmp eq ptr %0, null
  %2 = getelementptr inbounds nuw i8, ptr %_2, i64 8
  br i1 %1, label %bb4, label %bb5

bb4:                                              ; preds = %start
  %3 = load i8, ptr %2, align 8, !range !8, !noundef !4
  %4 = getelementptr inbounds nuw i8, ptr %_2, i64 9
  %5 = load i8, ptr %4, align 1, !range !8, !noundef !4
  %6 = getelementptr inbounds nuw i8, ptr %_0, i64 8
  store i8 %3, ptr %6, align 8
  %7 = getelementptr inbounds nuw i8, ptr %_0, i64 9
  store i8 %5, ptr %7, align 1
  store ptr null, ptr %_0, align 8
  br label %bb2

bb5:                                              ; preds = %start
  %_4.1 = load i64, ptr %2, align 8, !noundef !4
  %8 = icmp eq ptr %0, %source.0
  %9 = icmp eq i64 %_4.1, %source.1
  %_7 = and i1 %8, %9
  br i1 %_7, label %bb9, label %bb7

bb7:                                              ; preds = %bb5
  %10 = getelementptr inbounds nuw i8, ptr %_0, i64 8
  store i8 2, ptr %10, align 8
  store ptr null, ptr %_0, align 8
  br label %bb2

bb2:                                              ; preds = %bb4, %bb9, %bb7
  call void @llvm.lifetime.end.p0(ptr nonnull %_2)
  ret void

bb9:                                              ; preds = %bb5
  store ptr %0, ptr %_0, align 8
  %11 = getelementptr inbounds nuw i8, ptr %_0, i64 8
  store i64 %source.1, ptr %11, align 8
  br label %bb2
}

; release_consumer::typescript_parse
; Function Attrs: noinline uwtable
define internal fastcc void @_RNvCs6RePiqkWNcT_16release_consumer16typescript_parse(ptr dead_on_unwind noalias noundef nonnull writable writeonly align 8 captures(none) dereferenceable(16) initializes((0, 9)) %_0, ptr noalias noundef nonnull readonly captures(address, read_provenance) %source.0, i64 noundef range(i64 0, -9223372036854775808) %source.1) unnamed_addr #1 {
start:
  %_2 = alloca [16 x i8], align 8
  call void @llvm.lifetime.start.p0(ptr nonnull %_2)
; call <nudox_compile_registry::FullRegistry>::dispatch
  call void @_RNvMs0_CsjeakGkMLXDR_22nudox_compile_registryNtB5_12FullRegistry8dispatch(ptr noalias noundef nonnull sret([16 x i8]) align 8 captures(address) dereferenceable(16) %_2, i1 noundef zeroext true, i1 noundef zeroext false, ptr noalias noundef nonnull readonly captures(address, read_provenance) %source.0, i64 noundef %source.1)
  %0 = load ptr, ptr %_2, align 8, !noundef !4
  %1 = icmp eq ptr %0, null
  %2 = getelementptr inbounds nuw i8, ptr %_2, i64 8
  br i1 %1, label %bb4, label %bb5

bb4:                                              ; preds = %start
  %3 = load i8, ptr %2, align 8, !range !8, !noundef !4
  %4 = getelementptr inbounds nuw i8, ptr %_2, i64 9
  %5 = load i8, ptr %4, align 1, !range !8, !noundef !4
  %6 = getelementptr inbounds nuw i8, ptr %_0, i64 8
  store i8 %3, ptr %6, align 8
  %7 = getelementptr inbounds nuw i8, ptr %_0, i64 9
  store i8 %5, ptr %7, align 1
  store ptr null, ptr %_0, align 8
  br label %bb2

bb5:                                              ; preds = %start
  %_4.1 = load i64, ptr %2, align 8, !noundef !4
  %8 = icmp eq ptr %0, %source.0
  %9 = icmp eq i64 %_4.1, %source.1
  %_7 = and i1 %8, %9
  br i1 %_7, label %bb9, label %bb7

bb7:                                              ; preds = %bb5
  %10 = getelementptr inbounds nuw i8, ptr %_0, i64 8
  store i8 2, ptr %10, align 8
  store ptr null, ptr %_0, align 8
  br label %bb2

bb2:                                              ; preds = %bb4, %bb9, %bb7
  call void @llvm.lifetime.end.p0(ptr nonnull %_2)
  ret void

bb9:                                              ; preds = %bb5
  store ptr %0, ptr %_0, align 8
  %11 = getelementptr inbounds nuw i8, ptr %_0, i64 8
  store i64 %source.1, ptr %11, align 8
  br label %bb2
}

; release_consumer::main
; Function Attrs: uwtable
define hidden { i8, i8 } @_RNvCs6RePiqkWNcT_16release_consumer4main() unnamed_addr #0 {
start:
  %0 = alloca [16 x i8], align 8
  %1 = alloca [16 x i8], align 8
  %2 = alloca [16 x i8], align 8
  %3 = alloca [16 x i8], align 8
  %4 = alloca [16 x i8], align 8
  %5 = alloca [16 x i8], align 8
  %_19 = alloca [16 x i8], align 8
  %_14 = alloca [16 x i8], align 8
  %_9 = alloca [16 x i8], align 8
  call void @llvm.lifetime.start.p0(ptr nonnull %5)
  store ptr @alloc_19e1629a66736c3c0127852ef9f3276c, ptr %5, align 8
  %6 = getelementptr inbounds nuw i8, ptr %5, i64 8
  store i64 21, ptr %6, align 8
  call void asm sideeffect "", "r,~{memory}"(ptr nonnull %5) #6, !srcloc !3
  %rust_parse_input.0 = load ptr, ptr %5, align 8, !nonnull !4, !noundef !4
  %rust_parse_input.1 = load i64, ptr %6, align 8, !noundef !4
  call void @llvm.lifetime.end.p0(ptr nonnull %5)
  call void @llvm.lifetime.start.p0(ptr nonnull %4)
  store ptr @alloc_c8ae94ea3405bd1d1572531fe5ad5793, ptr %4, align 8
  %7 = getelementptr inbounds nuw i8, ptr %4, i64 8
  store i64 21, ptr %7, align 8
  call void asm sideeffect "", "r,~{memory}"(ptr nonnull %4) #6, !srcloc !3
  %rust_lower_input.0 = load ptr, ptr %4, align 8, !nonnull !4, !noundef !4
  %rust_lower_input.1 = load i64, ptr %7, align 8, !noundef !4
  call void @llvm.lifetime.end.p0(ptr nonnull %4)
  call void @llvm.lifetime.start.p0(ptr nonnull %3)
  store ptr @alloc_052b66adb9d7d7d61473efdf8055f48b, ptr %3, align 8
  %8 = getelementptr inbounds nuw i8, ptr %3, i64 8
  store i64 24, ptr %8, align 8
  call void asm sideeffect "", "r,~{memory}"(ptr nonnull %3) #6, !srcloc !3
  %typescript_parse_input.0 = load ptr, ptr %3, align 8, !nonnull !4, !noundef !4
  %typescript_parse_input.1 = load i64, ptr %8, align 8, !noundef !4
  call void @llvm.lifetime.end.p0(ptr nonnull %3)
  call void @llvm.lifetime.start.p0(ptr nonnull %_9)
; call release_consumer::rust_parse
  call fastcc void @_RNvCs6RePiqkWNcT_16release_consumer10rust_parse(ptr noalias noundef align 8 captures(none) dereferenceable(16) %_9, ptr noalias noundef nonnull readonly captures(address, read_provenance) %rust_parse_input.0, i64 noundef %rust_parse_input.1)
  %9 = load ptr, ptr %_9, align 8, !noundef !4
  %10 = icmp eq ptr %9, null
  %11 = getelementptr inbounds nuw i8, ptr %_9, i64 8
  br i1 %10, label %bb10, label %bb11

bb10:                                             ; preds = %start
  %_24.0 = load i8, ptr %11, align 8, !range !9, !noundef !4
  %12 = getelementptr inbounds nuw i8, ptr %_9, i64 9
  %_24.1 = load i8, ptr %12, align 1
  call void @llvm.lifetime.end.p0(ptr nonnull %_9)
  br label %bb6

bb11:                                             ; preds = %start
  %_23.1 = load i64, ptr %11, align 8, !noundef !4
  call void @llvm.lifetime.end.p0(ptr nonnull %_9)
  call void @llvm.lifetime.start.p0(ptr nonnull %2)
  store ptr %9, ptr %2, align 8
  %13 = getelementptr inbounds nuw i8, ptr %2, i64 8
  store i64 %_23.1, ptr %13, align 8
  call void asm sideeffect "", "r,~{memory}"(ptr nonnull %2) #6, !srcloc !3
  call void @llvm.lifetime.end.p0(ptr nonnull %2)
  call void @llvm.lifetime.start.p0(ptr nonnull %_14)
; call release_consumer::rust_lower
  call fastcc void @_RNvCs6RePiqkWNcT_16release_consumer10rust_lower(ptr noalias noundef align 8 captures(none) dereferenceable(16) %_14, ptr noalias noundef nonnull readonly captures(address, read_provenance) %rust_lower_input.0, i64 noundef %rust_lower_input.1)
  %14 = load ptr, ptr %_14, align 8, !noundef !4
  %15 = icmp eq ptr %14, null
  %16 = getelementptr inbounds nuw i8, ptr %_14, i64 8
  br i1 %15, label %bb13, label %bb14

bb13:                                             ; preds = %bb11
  %_31.0 = load i8, ptr %16, align 8, !range !9, !noundef !4
  %17 = getelementptr inbounds nuw i8, ptr %_14, i64 9
  %_31.1 = load i8, ptr %17, align 1
  call void @llvm.lifetime.end.p0(ptr nonnull %_14)
  br label %bb6

bb14:                                             ; preds = %bb11
  %_30.1 = load i64, ptr %16, align 8, !noundef !4
  call void @llvm.lifetime.end.p0(ptr nonnull %_14)
  call void @llvm.lifetime.start.p0(ptr nonnull %1)
  store ptr %14, ptr %1, align 8
  %18 = getelementptr inbounds nuw i8, ptr %1, i64 8
  store i64 %_30.1, ptr %18, align 8
  call void asm sideeffect "", "r,~{memory}"(ptr nonnull %1) #6, !srcloc !3
  call void @llvm.lifetime.end.p0(ptr nonnull %1)
  call void @llvm.lifetime.start.p0(ptr nonnull %_19)
; call release_consumer::typescript_parse
  call fastcc void @_RNvCs6RePiqkWNcT_16release_consumer16typescript_parse(ptr noalias noundef align 8 captures(none) dereferenceable(16) %_19, ptr noalias noundef nonnull readonly captures(address, read_provenance) %typescript_parse_input.0, i64 noundef %typescript_parse_input.1)
  %19 = load ptr, ptr %_19, align 8, !noundef !4
  %20 = icmp eq ptr %19, null
  %21 = getelementptr inbounds nuw i8, ptr %_19, i64 8
  br i1 %20, label %bb16, label %bb17

bb16:                                             ; preds = %bb14
  %_38.0 = load i8, ptr %21, align 8, !range !9, !noundef !4
  %22 = getelementptr inbounds nuw i8, ptr %_19, i64 9
  %_38.1 = load i8, ptr %22, align 1
  call void @llvm.lifetime.end.p0(ptr nonnull %_19)
  br label %bb6

bb17:                                             ; preds = %bb14
  %_37.1 = load i64, ptr %21, align 8, !noundef !4
  call void @llvm.lifetime.end.p0(ptr nonnull %_19)
  call void @llvm.lifetime.start.p0(ptr nonnull %0)
  store ptr %19, ptr %0, align 8
  %23 = getelementptr inbounds nuw i8, ptr %0, i64 8
  store i64 %_37.1, ptr %23, align 8
  call void asm sideeffect "", "r,~{memory}"(ptr nonnull %0) #6, !srcloc !3
  call void @llvm.lifetime.end.p0(ptr nonnull %0)
  br label %bb6

bb6:                                              ; preds = %bb16, %bb13, %bb10, %bb17
  %_0.sroa.5.0 = phi i8 [ %_24.1, %bb10 ], [ %_31.1, %bb13 ], [ %_38.1, %bb16 ], [ undef, %bb17 ]
  %_0.sroa.0.0 = phi i8 [ %_24.0, %bb10 ], [ %_31.0, %bb13 ], [ %_38.0, %bb16 ], [ -1, %bb17 ]
  %24 = insertvalue { i8, i8 } poison, i8 %_0.sroa.0.0, 0
  %25 = insertvalue { i8, i8 } %24, i8 %_0.sroa.5.0, 1
  ret { i8, i8 } %25
}

; <release_consumer::ConsumerError as core::fmt::Debug>::fmt
; Function Attrs: uwtable
define internal noundef zeroext i1 @_RNvXCs6RePiqkWNcT_16release_consumerNtB2_13ConsumerErrorNtNtCsgtPOCBgevO_4core3fmt5Debug3fmt(ptr noalias noundef readonly captures(address, read_provenance) dereferenceable(2) %self, ptr noalias noundef align 8 dereferenceable(24) %formatter) unnamed_addr #0 {
start:
  %_7 = alloca [24 x i8], align 8
  %0 = load i8, ptr %self, align 1, !range !9, !noundef !4
  %1 = icmp samesign ugt i8 %0, 1
  %2 = zext nneg i8 %0 to i64
  %3 = add nsw i64 %2, -1
  %_3 = select i1 %1, i64 %3, i64 0
  switch i64 %_3, label %bb1 [
    i64 0, label %bb4
    i64 1, label %bb3
    i64 2, label %bb2
  ]

bb1:                                              ; preds = %start
  unreachable

bb4:                                              ; preds = %start
  call void @llvm.lifetime.start.p0(ptr nonnull %_7)
; call <core::fmt::Formatter>::debug_tuple
  call void @_RNvMsa_NtCsgtPOCBgevO_4core3fmtNtB5_9Formatter11debug_tuple(ptr noalias noundef nonnull sret([24 x i8]) align 8 captures(address) dereferenceable(24) %_7, ptr noalias noundef nonnull align 8 dereferenceable(24) %formatter, ptr noalias noundef nonnull readonly captures(address, read_provenance) @alloc_f823262ef2001b6b275b03e1c243ec3b, i64 noundef 8)
; call <core::fmt::builders::DebugTuple>::field
  %_5 = call noundef nonnull align 8 ptr @_RNvMs2_NtNtCsgtPOCBgevO_4core3fmt8buildersNtB5_10DebugTuple5field(ptr noalias noundef nonnull align 8 dereferenceable(24) %_7, ptr noundef nonnull %self, ptr noalias noundef readonly align 8 captures(address, read_provenance) dereferenceable(32) @vtable.1)
; call <core::fmt::builders::DebugTuple>::finish
  %4 = call noundef zeroext i1 @_RNvMs2_NtNtCsgtPOCBgevO_4core3fmt8buildersNtB5_10DebugTuple6finish(ptr noalias noundef nonnull align 8 dereferenceable(24) %_5)
  call void @llvm.lifetime.end.p0(ptr nonnull %_7)
  br label %bb8

bb3:                                              ; preds = %start
; call <core::fmt::Formatter>::write_str
  %5 = tail call noundef zeroext i1 @_RNvMsa_NtCsgtPOCBgevO_4core3fmtNtB5_9Formatter9write_str(ptr noalias noundef nonnull align 8 dereferenceable(24) %formatter, ptr noalias noundef nonnull readonly captures(address, read_provenance) @alloc_bac30f724c94d05a07257438bff897a2, i64 noundef 16)
  br label %bb8

bb2:                                              ; preds = %start
; call <core::fmt::Formatter>::write_str
  %6 = tail call noundef zeroext i1 @_RNvMsa_NtCsgtPOCBgevO_4core3fmtNtB5_9Formatter9write_str(ptr noalias noundef nonnull align 8 dereferenceable(24) %formatter, ptr noalias noundef nonnull readonly captures(address, read_provenance) @alloc_051699efd7a6f01d4870ec6de7d9c676, i64 noundef 15)
  br label %bb8

bb8:                                              ; preds = %bb2, %bb3, %bb4
  %_0.sroa.0.0.in = phi i1 [ %4, %bb4 ], [ %5, %bb3 ], [ %6, %bb2 ]
  ret i1 %_0.sroa.0.0.in
}

; <nudox_compile_vocab::Language as core::fmt::Debug>::fmt
; Function Attrs: inlinehint uwtable
define internal noundef zeroext i1 @_RNvXs1_Cs5UHFsggAg5t_19nudox_compile_vocabNtB5_8LanguageNtNtCsgtPOCBgevO_4core3fmt5Debug3fmt(ptr noalias noundef readonly captures(none) dereferenceable(1) %self, ptr noalias noundef align 8 dereferenceable(24) %f) unnamed_addr #2 {
start:
  %0 = load i8, ptr %self, align 1, !range !8, !noundef !4
  %1 = trunc nuw i8 %0 to i1
  %. = select i1 %1, i64 16, i64 10
  %alloc_9f9bfae2a1c54e2e9245008f77214b29.alloc_f16b25e52a01538245d98357ad63ee2c = select i1 %1, ptr @alloc_9f9bfae2a1c54e2e9245008f77214b29, ptr @alloc_f16b25e52a01538245d98357ad63ee2c
; call <core::fmt::Formatter>::write_str
  %_0 = tail call noundef zeroext i1 @_RNvMsa_NtCsgtPOCBgevO_4core3fmtNtB5_9Formatter9write_str(ptr noalias noundef nonnull align 8 dereferenceable(24) %f, ptr noalias noundef nonnull readonly captures(address, read_provenance) %alloc_9f9bfae2a1c54e2e9245008f77214b29.alloc_f16b25e52a01538245d98357ad63ee2c, i64 noundef %.)
  ret i1 %_0
}

; <&nudox_compile_vocab::Stage as core::fmt::Debug>::fmt
; Function Attrs: uwtable
define internal noundef zeroext i1 @_RNvXs1g_NtCsgtPOCBgevO_4core3fmtRNtCs5UHFsggAg5t_19nudox_compile_vocab5StageNtB6_5Debug3fmtCs6RePiqkWNcT_16release_consumer(ptr noalias noundef readonly align 8 captures(none) dereferenceable(8) %self, ptr noalias noundef align 8 dereferenceable(24) %f) unnamed_addr #0 {
start:
  %_3 = load ptr, ptr %self, align 8, !nonnull !4, !noundef !4
  %_3.val = load i8, ptr %_3, align 1, !range !8, !noundef !4
  %0 = trunc nuw i8 %_3.val to i1
  %..i = select i1 %0, i64 7, i64 5
  %alloc_98a73cdd092a18d6389fd557415299d7.alloc_4cf7911ffc1fa65c3bf9af4755f6af39.i = select i1 %0, ptr @alloc_98a73cdd092a18d6389fd557415299d7, ptr @alloc_4cf7911ffc1fa65c3bf9af4755f6af39
; call <core::fmt::Formatter>::write_str
  %_0.i = tail call noundef zeroext i1 @_RNvMsa_NtCsgtPOCBgevO_4core3fmtNtB5_9Formatter9write_str(ptr noalias noundef nonnull align 8 dereferenceable(24) %f, ptr noalias noundef nonnull readonly captures(address, read_provenance) %alloc_98a73cdd092a18d6389fd557415299d7.alloc_4cf7911ffc1fa65c3bf9af4755f6af39.i, i64 noundef %..i)
  ret i1 %_0.i
}

; <nudox_compile_vocab::FrontendError as core::fmt::Debug>::fmt
; Function Attrs: inlinehint uwtable
define internal noundef zeroext i1 @_RNvXsf_Cs5UHFsggAg5t_19nudox_compile_vocabNtB5_13FrontendErrorNtNtCsgtPOCBgevO_4core3fmt5Debug3fmt(ptr noalias noundef readonly captures(address, read_provenance) dereferenceable(2) %self, ptr noalias noundef align 8 dereferenceable(24) %f) unnamed_addr #2 {
start:
  %__self_1 = alloca [8 x i8], align 8
  call void @llvm.lifetime.start.p0(ptr nonnull %__self_1)
  %0 = getelementptr inbounds nuw i8, ptr %self, i64 1
  store ptr %0, ptr %__self_1, align 8
; call <core::fmt::Formatter>::debug_struct_field2_finish
  %_0 = call noundef zeroext i1 @_RNvMsa_NtCsgtPOCBgevO_4core3fmtNtB5_9Formatter26debug_struct_field2_finish(ptr noalias noundef nonnull align 8 dereferenceable(24) %f, ptr noalias noundef nonnull readonly captures(address, read_provenance) @alloc_1dc2bfd5907371d8024f4a89167a40c3, i64 noundef 16, ptr noalias noundef nonnull readonly captures(address, read_provenance) @alloc_6804fcd76d287f42ed3f019f5ce179ec, i64 noundef 8, ptr noundef nonnull %self, ptr noalias noundef readonly align 8 captures(address, read_provenance) dereferenceable(32) @vtable.2, ptr noalias noundef nonnull readonly captures(address, read_provenance) @alloc_39819854344d49e678f457feac9e6644, i64 noundef 5, ptr noundef nonnull %__self_1, ptr noalias noundef readonly align 8 captures(address, read_provenance) dereferenceable(32) @vtable.3)
  call void @llvm.lifetime.end.p0(ptr nonnull %__self_1)
  ret i1 %_0
}

; Function Attrs: mustprogress nocallback nofree nosync nounwind willreturn memory(argmem: readwrite)
declare void @llvm.lifetime.start.p0(ptr captures(none)) #3

; std::rt::lang_start_internal
; Function Attrs: uwtable
declare noundef i64 @_RNvNtCsa9BKTri5B3M_3std2rt19lang_start_internal(ptr noundef nonnull, ptr noalias noundef readonly align 8 captures(address, read_provenance) dereferenceable(48), i64 noundef, ptr noundef, i8 noundef) unnamed_addr #0

; Function Attrs: mustprogress nocallback nofree nosync nounwind willreturn memory(argmem: readwrite)
declare void @llvm.lifetime.end.p0(ptr captures(none)) #3

; <nudox_compile_registry::FullRegistry>::dispatch
; Function Attrs: uwtable
declare void @_RNvMs0_CsjeakGkMLXDR_22nudox_compile_registryNtB5_12FullRegistry8dispatch(ptr dead_on_unwind noalias noundef writable sret([16 x i8]) align 8 captures(address) dereferenceable(16), i1 noundef zeroext, i1 noundef zeroext, ptr noalias noundef nonnull readonly captures(address, read_provenance), i64 noundef range(i64 0, -9223372036854775808)) unnamed_addr #0

; <core::fmt::Formatter>::debug_tuple
; Function Attrs: uwtable
declare void @_RNvMsa_NtCsgtPOCBgevO_4core3fmtNtB5_9Formatter11debug_tuple(ptr dead_on_unwind noalias noundef writable sret([24 x i8]) align 8 captures(address) dereferenceable(24), ptr noalias noundef align 8 dereferenceable(24), ptr noalias noundef nonnull readonly captures(address, read_provenance), i64 noundef) unnamed_addr #0

; <core::fmt::builders::DebugTuple>::field
; Function Attrs: uwtable
declare noundef nonnull align 8 ptr @_RNvMs2_NtNtCsgtPOCBgevO_4core3fmt8buildersNtB5_10DebugTuple5field(ptr noalias noundef align 8 dereferenceable(24), ptr noundef nonnull, ptr noalias noundef readonly align 8 captures(address, read_provenance) dereferenceable(32)) unnamed_addr #0

; <core::fmt::builders::DebugTuple>::finish
; Function Attrs: uwtable
declare noundef zeroext i1 @_RNvMs2_NtNtCsgtPOCBgevO_4core3fmt8buildersNtB5_10DebugTuple6finish(ptr noalias noundef align 8 dereferenceable(24)) unnamed_addr #0

; <core::fmt::Formatter>::write_str
; Function Attrs: uwtable
declare noundef zeroext i1 @_RNvMsa_NtCsgtPOCBgevO_4core3fmtNtB5_9Formatter9write_str(ptr noalias noundef align 8 dereferenceable(24), ptr noalias noundef nonnull readonly captures(address, read_provenance), i64 noundef) unnamed_addr #0

; Function Attrs: nounwind uwtable
declare noundef range(i32 0, 10) i32 @rust_eh_personality(i32 noundef, i32 noundef, i64 noundef, ptr noundef, ptr noundef) unnamed_addr #4

; std::io::stdio::attempt_print_to_stderr
; Function Attrs: uwtable
declare void @_RNvNtNtCsa9BKTri5B3M_3std2io5stdio23attempt_print_to_stderr(ptr noundef nonnull, ptr noundef nonnull) unnamed_addr #0

; <core::fmt::Formatter>::debug_struct_field2_finish
; Function Attrs: uwtable
declare noundef zeroext i1 @_RNvMsa_NtCsgtPOCBgevO_4core3fmtNtB5_9Formatter26debug_struct_field2_finish(ptr noalias noundef align 8 dereferenceable(24), ptr noalias noundef nonnull readonly captures(address, read_provenance), i64 noundef, ptr noalias noundef nonnull readonly captures(address, read_provenance), i64 noundef, ptr noundef nonnull, ptr noalias noundef readonly align 8 captures(address, read_provenance) dereferenceable(32), ptr noalias noundef nonnull readonly captures(address, read_provenance), i64 noundef, ptr noundef nonnull, ptr noalias noundef readonly align 8 captures(address, read_provenance) dereferenceable(32)) unnamed_addr #0

define noundef i32 @main(i32 %0, ptr %1) unnamed_addr #5 {
top:
  %_7.i = alloca [8 x i8], align 8
  %2 = sext i32 %0 to i64
  call void @llvm.lifetime.start.p0(ptr nonnull %_7.i)
  store ptr @_RNvCs6RePiqkWNcT_16release_consumer4main, ptr %_7.i, align 8
; call std::rt::lang_start_internal
  %_0.i = call noundef i64 @_RNvNtCsa9BKTri5B3M_3std2rt19lang_start_internal(ptr noundef nonnull %_7.i, ptr noalias noundef readonly align 8 captures(address, read_provenance) dereferenceable(48) @vtable.0, i64 noundef %2, ptr noundef %1, i8 noundef 0)
  call void @llvm.lifetime.end.p0(ptr nonnull %_7.i)
  %3 = trunc i64 %_0.i to i32
  ret i32 %3
}

attributes #0 = { uwtable "frame-pointer"="non-leaf" "probe-stack"="inline-asm" "target-cpu"="apple-m1" }
attributes #1 = { noinline uwtable "frame-pointer"="non-leaf" "probe-stack"="inline-asm" "target-cpu"="apple-m1" }
attributes #2 = { inlinehint uwtable "frame-pointer"="non-leaf" "probe-stack"="inline-asm" "target-cpu"="apple-m1" }
attributes #3 = { mustprogress nocallback nofree nosync nounwind willreturn memory(argmem: readwrite) }
attributes #4 = { nounwind uwtable "frame-pointer"="non-leaf" "probe-stack"="inline-asm" "target-cpu"="apple-m1" }
attributes #5 = { "frame-pointer"="non-leaf" "probe-stack"="inline-asm" "target-cpu"="apple-m1" }
attributes #6 = { nounwind }

!llvm.module.flags = !{!0, !1}
!llvm.ident = !{!2}

!0 = !{i32 8, !"PIC Level", i32 2}
!1 = !{i32 7, !"PIE Level", i32 2}
!2 = !{!"rustc version 1.97.1 (8bab26f4f 2026-07-14)"}
!3 = !{i64 5846743276373362}
!4 = !{}
!5 = !{!6}
!6 = distinct !{!6, !7, !"_RNCINvNtCsa9BKTri5B3M_3std2rt10lang_startINtNtCsgtPOCBgevO_4core6result6ResultuNtCs6RePiqkWNcT_16release_consumer13ConsumerErrorEE0B1h_: %_1"}
!7 = distinct !{!7, !"_RNCINvNtCsa9BKTri5B3M_3std2rt10lang_startINtNtCsgtPOCBgevO_4core6result6ResultuNtCs6RePiqkWNcT_16release_consumer13ConsumerErrorEE0B1h_"}
!8 = !{i8 0, i8 2}
!9 = !{i8 0, i8 4}
