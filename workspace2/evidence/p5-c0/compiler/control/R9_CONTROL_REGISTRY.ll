; ModuleID = 'nudox_compile_registry.f59f4b6349e970a1-cgu.0'
source_filename = "nudox_compile_registry.f59f4b6349e970a1-cgu.0"
target datalayout = "e-m:o-p270:32:32-p271:32:32-p272:64:64-i64:64-i128:128-n32:64-S128-Fn32"
target triple = "arm64-apple-macosx11.0.0"

; <nudox_compile_registry::FullRegistry>::dispatch
; Function Attrs: mustprogress nofree norecurse nosync nounwind willreturn memory(argmem: write) uwtable
define void @_RNvMs0_Csl5rmTUODvuT_22nudox_compile_registryNtB5_12FullRegistry8dispatch(ptr dead_on_unwind noalias noundef writable writeonly sret([16 x i8]) align 8 captures(none) dereferenceable(16) initializes((0, 10)) %_0, i1 noundef zeroext %language, i1 noundef zeroext %stage, ptr noalias noundef nonnull readonly captures(address, read_provenance) %source.0, i64 noundef range(i64 0, -9223372036854775808) %source.1) unnamed_addr #0 {
start:
  br i1 %language, label %bb2, label %bb3

bb2:                                              ; preds = %start
  %0 = getelementptr inbounds nuw i8, ptr %_0, i64 8
  br i1 %stage, label %bb2.i, label %bb3.i

bb2.i:                                            ; preds = %bb2
  store i8 1, ptr %0, align 8, !alias.scope !2, !noalias !7
  %1 = getelementptr inbounds nuw i8, ptr %_0, i64 9
  store i8 1, ptr %1, align 1, !alias.scope !2, !noalias !7
  br label %_RINvCsl5rmTUODvuT_22nudox_compile_registry5driveNtB2_18TypeScriptFrontendEB2_.exit

bb3.i:                                            ; preds = %bb2
  store i64 %source.1, ptr %0, align 8, !alias.scope !9, !noalias !7
  br label %_RINvCsl5rmTUODvuT_22nudox_compile_registry5driveNtB2_18TypeScriptFrontendEB2_.exit

_RINvCsl5rmTUODvuT_22nudox_compile_registry5driveNtB2_18TypeScriptFrontendEB2_.exit: ; preds = %bb2.i, %bb3.i
  %source.0.sink.i = phi ptr [ null, %bb2.i ], [ %source.0, %bb3.i ]
  store ptr %source.0.sink.i, ptr %_0, align 8, !alias.scope !9, !noalias !7
  br label %bb4

bb3:                                              ; preds = %start
  store ptr %source.0, ptr %_0, align 8, !alias.scope !10, !noalias !13
  %2 = getelementptr inbounds nuw i8, ptr %_0, i64 8
  store i64 %source.1, ptr %2, align 8, !alias.scope !10, !noalias !13
  br label %bb4

bb4:                                              ; preds = %_RINvCsl5rmTUODvuT_22nudox_compile_registry5driveNtB2_18TypeScriptFrontendEB2_.exit, %bb3
  ret void
}

attributes #0 = { mustprogress nofree norecurse nosync nounwind willreturn memory(argmem: write) uwtable "frame-pointer"="non-leaf" "probe-stack"="inline-asm" "target-cpu"="apple-m1" }

!llvm.module.flags = !{!0}
!llvm.ident = !{!1}

!0 = !{i32 8, !"PIC Level", i32 2}
!1 = !{!"rustc version 1.97.1 (8bab26f4f 2026-07-14)"}
!2 = !{!3, !5}
!3 = distinct !{!3, !4, !"_RNvXs_Csl5rmTUODvuT_22nudox_compile_registryNtB4_18TypeScriptFrontendNtB4_8Frontend5lower: %_0"}
!4 = distinct !{!4, !"_RNvXs_Csl5rmTUODvuT_22nudox_compile_registryNtB4_18TypeScriptFrontendNtB4_8Frontend5lower"}
!5 = distinct !{!5, !6, !"_RINvCsl5rmTUODvuT_22nudox_compile_registry5driveNtB2_18TypeScriptFrontendEB2_: %_0"}
!6 = distinct !{!6, !"_RINvCsl5rmTUODvuT_22nudox_compile_registry5driveNtB2_18TypeScriptFrontendEB2_"}
!7 = !{!8}
!8 = distinct !{!8, !6, !"_RINvCsl5rmTUODvuT_22nudox_compile_registry5driveNtB2_18TypeScriptFrontendEB2_: %source.0"}
!9 = !{!5}
!10 = !{!11}
!11 = distinct !{!11, !12, !"_RINvCsl5rmTUODvuT_22nudox_compile_registry5driveNtB2_12RustFrontendEB2_: %_0"}
!12 = distinct !{!12, !"_RINvCsl5rmTUODvuT_22nudox_compile_registry5driveNtB2_12RustFrontendEB2_"}
!13 = !{!14}
!14 = distinct !{!14, !12, !"_RINvCsl5rmTUODvuT_22nudox_compile_registry5driveNtB2_12RustFrontendEB2_: %source.0"}
