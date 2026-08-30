//! Standalone compiler-layout inventory for the data-layout audit.
//!
//! This executable intentionally measures only public representations. Private implementation
//! records are captured by the owning crate's nightly layout dump, rather than widening a public
//! API merely to make the audit convenient.

use core::mem::{align_of, offset_of, size_of};

use nudox_frame::{EncodeError, PreparedFrame, SectionInput};
use nudox_hydration::{
    BoundNeed, DemandBindError, Fetch, FetchRoute, HydrationPlanView, Need, PlanCoverage,
    PlanError, PlanScratch, PlanScratchFacts, Projection, Promise, StagedGeneration,
    VerificationError, VerifiedGeneration,
};
use nudox_id::{
    ArtifactHasher, ArtifactId, ContentAuthority, ContentHasher, ContentId, ContentRoutingWord,
    DomainTag, EncodingTag, FrameEncoding, GenerationId, ObjectDomain,
};
use nudox_object::{
    DepSetId, ObjectKind, ObjectLength, ObjectRef, ProviderId, ProviderSet, RemoteBase,
};
use nudox_observe::{DropNewest, FlightRecorder, OverwriteOldest};
use nudox_operation::{
    LocalObjectError, LocalObjectProvider, LocalObjectRun, MissingObject, ObjectBatch,
    ObjectProvenance, PinnedObjectOperation, PinnedObjectRequest, SourcePoll, TerminalSummary,
};
use nudox_root::{
    ClosureScratch, ClosureScratchFacts, EntryKey, EntryRange, GenerationEntry, GenerationRoot,
    GenerationRootBuilder, GenerationRootFacts, GenerationScan, GenerationView, Locality,
    LocalityException, LocalityLayout, LocalityLookupWork, LocalityRow, LocalityScanWork,
    LocalityValidator, LocalityWriteError, MeasuredGenerationScan, NonResident, PreparedLocality,
    RootBuildError, RootChange, RootDiff, RootEntry, RootPushError, SelectedClosure, SelectedCount,
    SelectedGeneration, SelectionWork, ValidatedLocality,
};
use nudox_runtime::{
    Admission, AdmissionError, AdmissionFuture, ByteBudget, ByteBudgetError, ByteQuantum,
    CancelResult, Owner, OwnerFault, RejectedWork, RejectionReason, RemoteRuntime, RetainedBytes,
    RuntimeConfigError, RuntimeMetrics, SlotClaimError, TerminalClass, TerminalEvent,
    TerminalOutcome, WaiterRegistrationError, WorkHandle,
};
use nudox_schema::{
    DecodeLimits, FrameBytesLimit, FrameHeader, LimitError, LimitKind, RowCount,
    SectionCompatibilityError, SectionDescriptor, SectionDisposition, SectionKind,
    UnknownSectionKind,
};
use nudox_store_memory::{
    ByteCapacity, InlineMemoryStore, InsertOutcome, Lookup, MemoryStore, RejectedInsert,
    SlotCapacity, StoreAdmission, StoreCapacity, StoreError, StoreInitError, StoreProbeEvent,
    StoreStats, StoredObjectView,
};
use nudox_view::{
    DescriptorError, Section, SectionReadError, Sections, ValidateError, ValidatedFrame,
};
use nudox_workflow::{
    CommitError, Effect, EffectAction, EventKind, EventName, FailureCode, LogConfigError,
    MemoryWorkflowLog, Phase, PhaseName, PriorFacts, Recovery, Reduction, ReductionError, StageId,
    StageInput, StageKey, WorkflowEvent, WorkflowRecord, WorkflowRecordError, WorkflowState,
    WorkflowVersion,
};

const CACHE_LINE_BYTES: usize = 64;

macro_rules! layout {
    ($family:literal, $type:ty, $contract:literal) => {
        print_layout::<$type>($family, stringify!($type), $contract)
    };
}

macro_rules! field_offset {
    ($type:ty, $field:ident) => {
        print_offset(
            stringify!($type),
            stringify!($field),
            offset_of!($type, $field),
        )
    };
}

fn print_layout<Type>(family: &str, name: &str, contract: &str) {
    let bytes = size_of::<Type>();
    let cache_lines = bytes.div_ceil(CACHE_LINE_BYTES);
    println!(
        "type\t{family}\t{name}\t{bytes}\t{}\t{cache_lines}\t{contract}",
        align_of::<Type>(),
    );
}

fn print_offset(name: &str, field: &str, offset: usize) {
    println!("offset\t{name}\t{field}\t{offset}");
}

fn main() {
    println!("format\tnudox-layout-baseline-v1");
    println!("target_arch\t{}", std::env::consts::ARCH);
    println!("target_os\t{}", std::env::consts::OS);
    println!("pointer_width\t{}", usize::BITS);
    println!("cache_line_assumption_bytes\t{CACHE_LINE_BYTES}");
    println!("columns\tkind\tfamily\ttype\tsize_bytes\talign_bytes\tcache_lines\tcontract");

    layout!(
        "identity",
        DomainTag,
        "repr(transparent), protocol-label width"
    );
    layout!(
        "identity",
        EncodingTag,
        "repr(transparent), protocol-label width"
    );
    layout!(
        "identity",
        ContentId<ObjectDomain>,
        "repr(transparent), explicit exact-layout test"
    );
    layout!(
        "identity",
        ContentAuthority<ObjectDomain>,
        "zero-sized checked artifact-global authority proof"
    );
    layout!(
        "identity",
        ArtifactId<FrameEncoding, ObjectDomain>,
        "repr(transparent), explicit exact-layout test"
    );
    layout!(
        "identity",
        ContentRoutingWord,
        "repr(transparent), explicit exact-layout test"
    );
    layout!("identity", GenerationId, "ContentId alias");
    layout!("identity", DepSetId, "ContentId alias");
    layout!(
        "identity",
        ContentHasher<ObjectDomain>,
        "compiler discovery"
    );
    layout!(
        "identity",
        ArtifactHasher<FrameEncoding, ObjectDomain>,
        "compiler discovery"
    );

    layout!("frame", FrameHeader, "repr(C), exact wire layout test");
    layout!(
        "frame",
        SectionDescriptor,
        "repr(C), exact wire layout test"
    );
    layout!("frame", SectionKind, "repr(u8), closed wire tag");
    layout!("frame", SectionDisposition, "compiler discovery");
    layout!("frame", SectionCompatibilityError, "compiler discovery");
    layout!("frame", UnknownSectionKind, "tuple newtype");
    layout!("frame", DecodeLimits, "compiler discovery");
    layout!("frame", FrameBytesLimit, "repr(transparent)");
    layout!("frame", RowCount, "repr(transparent)");
    layout!("frame", LimitKind, "compiler discovery");
    layout!("frame", LimitError, "compiler discovery");
    layout!("frame", SectionInput<'static>, "borrowed input");
    layout!(
        "frame",
        PreparedFrame<'static, 'static>,
        "inline prepared-section capacity"
    );
    layout!("frame", EncodeError, "compiler discovery");
    layout!(
        "frame",
        ValidatedFrame<'static>,
        "inline validated-section capacity"
    );
    layout!("frame", Section<'static>, "borrowed section");
    layout!("frame", Sections<'static>, "fallible borrowing iterator");
    layout!("frame", SectionReadError, "post-validation read error");
    layout!("frame", ValidateError, "compiler discovery");
    layout!("frame", DescriptorError, "compiler discovery");

    layout!("object", ObjectLength, "repr(transparent)");
    layout!("object", ObjectKind, "repr(transparent)");
    layout!(
        "object",
        ObjectRef<ObjectDomain>,
        "repr(C), semantic descriptor"
    );
    layout!("object", ProviderId, "repr(transparent)");
    layout!("object", ProviderSet, "repr(transparent) bitset");
    layout!("object", RemoteBase<ObjectDomain>, "compiler discovery");

    layout!("root", EntryKey, "repr(transparent)");
    layout!("root", EntryRange, "compiler discovery");
    layout!("root", RootEntry<ObjectDomain>, "compiler discovery");
    layout!(
        "root",
        GenerationRoot<ObjectDomain>,
        "boxed packed-row owner"
    );
    layout!("root", GenerationRootFacts, "compiler discovery");
    layout!(
        "root",
        GenerationRootBuilder<ObjectDomain>,
        "vector-backed builder"
    );
    layout!("root", RootBuildError, "cold error, compiler discovery");
    layout!("root", RootPushError, "cold error, compiler discovery");
    layout!("root", Locality<ObjectDomain>, "compiler discovery");
    layout!(
        "root",
        LocalityRow<ObjectDomain>,
        "root-issued compact coordinate"
    );
    layout!("root", NonResident<ObjectDomain>, "sparse placement input");
    layout!(
        "root",
        LocalityException<ObjectDomain>,
        "root-proven sparse placement"
    );
    layout!("root", GenerationEntry<ObjectDomain>, "composed scan row");
    layout!(
        "root",
        PreparedLocality<'static, ObjectDomain>,
        "borrowed exact-layout write plan"
    );
    layout!("root", LocalityLayout, "validated canonical geometry");
    layout!("root", LocalityValidator, "cached static SIMD dispatch");
    layout!(
        "root",
        ValidatedLocality<'static, ObjectDomain>,
        "borrowed canonical locality witness"
    );
    layout!("root", LocalityWriteError, "typed caller-output rejection");
    layout!(
        "root",
        SelectedCount,
        "repr(transparent) selection cardinality"
    );
    layout!(
        "root",
        GenerationView<'static, 'static, ObjectDomain>,
        "borrowed composition"
    );
    layout!(
        "root",
        GenerationScan<'static, 'static, ObjectDomain>,
        "borrowing iterator"
    );
    layout!(
        "root",
        MeasuredGenerationScan<'static, 'static, ObjectDomain>,
        "borrowing iterator plus work sink"
    );
    layout!(
        "root",
        SelectedGeneration<'static, ObjectDomain>,
        "borrowing selection view"
    );
    layout!("root", LocalityScanWork, "work counter");
    layout!("root", LocalityLookupWork, "work counter");
    layout!("root", ClosureScratch, "two reusable vectors");
    layout!("root", ClosureScratchFacts, "compiler discovery");
    layout!("root", SelectionWork, "work counter");
    layout!(
        "root",
        SelectedClosure<'static, ObjectDomain>,
        "borrowing selection view"
    );
    layout!("root", RootChange<ObjectDomain>, "diff event");
    layout!(
        "root",
        RootDiff<'static, 'static, ObjectDomain>,
        "borrowing diff cursor"
    );

    layout!("hydration", Projection, "compiler discovery");
    layout!("hydration", Need, "request fact");
    layout!(
        "hydration",
        BoundNeed<'static, 'static, 'static, ObjectDomain>,
        "borrowed generation witness"
    );
    layout!(
        "hydration",
        DemandBindError,
        "cold error, compiler discovery"
    );
    layout!("hydration", Promise<ObjectDomain>, "plan item");
    layout!("hydration", FetchRoute, "closed route");
    layout!("hydration", Fetch<ObjectDomain>, "plan item");
    layout!("hydration", PlanCoverage, "aggregate counters");
    layout!("hydration", PlanScratch, "coverage-sidecar vector");
    layout!("hydration", PlanScratchFacts, "compiler discovery");
    layout!("hydration", PlanError, "cold error, compiler discovery");
    layout!(
        "hydration",
        HydrationPlanView<'static, 'static, ObjectDomain>,
        "borrowing plan view"
    );
    layout!(
        "hydration",
        StagedGeneration<'static, 'static, 'static, ObjectDomain>,
        "borrowing typestate"
    );
    layout!(
        "hydration",
        VerifiedGeneration<'static, ()>,
        "sealed facts plus exact evidence borrow"
    );
    layout!(
        "hydration",
        VerificationError<ObjectDomain>,
        "cold error, compiler discovery"
    );

    layout!("store", ByteCapacity, "repr(transparent)");
    layout!("store", SlotCapacity, "repr(transparent)");
    layout!("store", StoreCapacity, "compiler discovery");
    layout!("store", StoreStats, "compiler discovery");
    layout!("store", StoreInitError, "cold error, compiler discovery");
    layout!(
        "store",
        InlineMemoryStore<ObjectDomain, 0, 0>,
        "zero-inline candidate; compiler discovery"
    );
    layout!(
        "store",
        InlineMemoryStore<ObjectDomain, 1, 2>,
        "one-entry/two-bucket client candidate; compiler discovery"
    );
    layout!(
        "store",
        MemoryStore<ObjectDomain>,
        "default four-entry/eight-bucket inline metadata"
    );
    layout!(
        "store",
        StoredObjectView<'static, ObjectDomain>,
        "borrowed lookup result"
    );
    layout!("store", Lookup, "probe count result");
    layout!("store", InsertOutcome, "closed admission outcome");
    layout!("store", StoreAdmission, "closed probe outcome");
    layout!("store", StoreProbeEvent, "aggregate event");
    layout!(
        "store",
        StoreError<ObjectDomain>,
        "cold error, compiler discovery"
    );
    layout!(
        "store",
        RejectedInsert<ObjectDomain>,
        "owns rejected payload box"
    );

    layout!("operation", ObjectProvenance, "compiler discovery");
    layout!(
        "operation",
        PinnedObjectRequest<ObjectDomain>,
        "operation request"
    );
    layout!("operation", MissingObject<ObjectDomain>, "terminal fact");
    layout!(
        "operation",
        PinnedObjectOperation<ObjectDomain>,
        "zero-sized operation brand"
    );
    layout!(
        "operation",
        LocalObjectError<ObjectDomain>,
        "cold error, compiler discovery"
    );
    layout!(
        "operation",
        ObjectBatch<'static, ObjectDomain>,
        "borrowed batch"
    );
    layout!(
        "operation",
        LocalObjectProvider<ObjectDomain>,
        "provider state"
    );
    layout!("operation", LocalObjectRun<ObjectDomain>, "cursor state");
    layout!("operation", SourcePoll<u64, u64>, "generic source poll");
    layout!("operation", TerminalSummary<u64>, "generic terminal fact");

    layout!("runtime", RetainedBytes, "repr(transparent)");
    layout!("runtime", ByteQuantum, "repr(transparent)");
    layout!("runtime", ByteBudget, "compiler discovery");
    layout!("runtime", ByteBudgetError, "cold error, compiler discovery");
    layout!("runtime", WorkHandle, "public generational handle");
    layout!("runtime", CancelResult, "compiler discovery");
    layout!("runtime", SlotClaimError, "cold error, compiler discovery");
    layout!(
        "runtime",
        WaiterRegistrationError,
        "cold error, compiler discovery"
    );
    layout!(
        "runtime",
        RuntimeMetrics,
        "structural physical-credit snapshot"
    );
    layout!("runtime", RemoteRuntime<u64, RetainedBytes>, "heap-reserved remote runtime");
    layout!(
        "runtime",
        Admission<'static, u64, u64>,
        "borrowed producer handle"
    );
    layout!(
        "runtime",
        AdmissionFuture<'static, u64, u64>,
        "async admission state"
    );
    layout!("runtime", Owner<'static, u64, u64>, "borrowed owner handle");
    layout!("runtime", RejectedWork<u64>, "returns rejected work");
    layout!("runtime", RejectionReason, "closed rejection");
    layout!(
        "runtime",
        AdmissionError<u64>,
        "cold error, compiler discovery"
    );
    layout!(
        "runtime",
        TerminalClass,
        "closed observation-only terminal class"
    );
    layout!(
        "runtime",
        TerminalOutcome<core::convert::Infallible>,
        "typed terminal outcome"
    );
    layout!(
        "runtime",
        TerminalEvent<u64, core::convert::Infallible>,
        "in-place terminal record projection"
    );
    layout!("runtime", OwnerFault, "cold error, compiler discovery");
    layout!(
        "runtime",
        RuntimeConfigError,
        "cold error, compiler discovery"
    );

    layout!("workflow", WorkflowVersion, "repr(transparent)");
    layout!("workflow", StageId, "closed tag");
    layout!("workflow", FailureCode, "closed tag");
    layout!("workflow", EventName, "closed diagnostic tag");
    layout!("workflow", StageKey, "32-byte canonical identity");
    layout!("workflow", StageInput<ObjectDomain>, "key derivation input");
    layout!("workflow", EventKind, "durable event payload");
    layout!("workflow", WorkflowEvent, "durable log row");
    layout!("workflow", WorkflowRecord, "fixed canonical durable record");
    layout!(
        "workflow",
        WorkflowRecordError,
        "typed canonical record rejection"
    );
    layout!("workflow", PriorFacts, "reducer state fact");
    layout!("workflow", Phase, "reducer state");
    layout!("workflow", PhaseName, "closed phase tag");
    layout!("workflow", WorkflowState, "reducer state");
    layout!("workflow", Effect, "pending effect");
    layout!("workflow", EffectAction, "closed idempotent effect action");
    layout!("workflow", Reduction, "reduction result");
    layout!("workflow", ReductionError, "cold error, compiler discovery");
    layout!(
        "workflow",
        MemoryWorkflowLog,
        "bounded in-memory durable adapter"
    );
    layout!("workflow", LogConfigError, "cold error, compiler discovery");
    layout!("workflow", CommitError, "cold error, compiler discovery");
    layout!("workflow", Recovery, "recovery result");

    layout!("observe", (), "zero-sized static policy");
    layout!(
        "observe",
        FlightRecorder<WorkflowEvent, DropNewest, 0>,
        "zero-capacity recorder policy"
    );
    layout!(
        "observe",
        FlightRecorder<WorkflowEvent, DropNewest, 4>,
        "inline four-event ring"
    );
    layout!(
        "observe",
        FlightRecorder<WorkflowEvent, OverwriteOldest, 4>,
        "inline four-event ring"
    );
    print_offset("FrameHeader", "magic", offset_of!(FrameHeader, magic));
    print_offset("FrameHeader", "major", offset_of!(FrameHeader, major));
    print_offset("FrameHeader", "minor", offset_of!(FrameHeader, minor));
    print_offset(
        "FrameHeader",
        "header_bytes",
        offset_of!(FrameHeader, header_bytes),
    );
    print_offset(
        "FrameHeader",
        "total_bytes",
        offset_of!(FrameHeader, total_bytes),
    );
    print_offset(
        "FrameHeader",
        "section_count",
        offset_of!(FrameHeader, section_count),
    );
    print_offset(
        "FrameHeader",
        "reserved_a",
        offset_of!(FrameHeader, reserved_a),
    );
    print_offset("FrameHeader", "schema", offset_of!(FrameHeader, schema));
    print_offset(
        "FrameHeader",
        "reserved_b",
        offset_of!(FrameHeader, reserved_b),
    );
    print_offset(
        "SectionDescriptor",
        "kind",
        offset_of!(SectionDescriptor, kind),
    );
    print_offset(
        "SectionDescriptor",
        "flags",
        offset_of!(SectionDescriptor, flags),
    );
    print_offset(
        "SectionDescriptor",
        "reserved",
        offset_of!(SectionDescriptor, reserved),
    );
    print_offset(
        "SectionDescriptor",
        "body_offset",
        offset_of!(SectionDescriptor, body_offset),
    );
    print_offset(
        "SectionDescriptor",
        "body_length",
        offset_of!(SectionDescriptor, body_length),
    );
    print_offset(
        "SectionDescriptor",
        "rows",
        offset_of!(SectionDescriptor, rows),
    );
    field_offset!(ObjectRef<ObjectDomain>, content);
    field_offset!(ObjectRef<ObjectDomain>, length);
    field_offset!(ObjectRef<ObjectDomain>, schema);
    field_offset!(ObjectRef<ObjectDomain>, kind);
    field_offset!(RootEntry<ObjectDomain>, key);
    field_offset!(RootEntry<ObjectDomain>, parent);
    field_offset!(RootEntry<ObjectDomain>, object);
    field_offset!(LocalityException<ObjectDomain>, row);
    field_offset!(LocalityException<ObjectDomain>, placement);
    field_offset!(GenerationEntry<ObjectDomain>, key);
    field_offset!(GenerationEntry<ObjectDomain>, parent);
    field_offset!(GenerationEntry<ObjectDomain>, object);
    field_offset!(GenerationEntry<ObjectDomain>, locality);
    field_offset!(Need, pinned_root);
    field_offset!(Need, projection);
    field_offset!(PlanCoverage, required);
    field_offset!(PlanCoverage, present);
    field_offset!(PlanCoverage, promised);
    field_offset!(PlanCoverage, missing);
    field_offset!(StoreCapacity, bytes);
    field_offset!(StoreCapacity, slots);
    field_offset!(StoreStats, capacity);
    field_offset!(StoreStats, index_buckets);
    field_offset!(StoreStats, index_bytes);
    field_offset!(StoreStats, retained_bytes);
    field_offset!(StoreStats, occupied_slots);
    field_offset!(WorkflowEvent, version);
    field_offset!(WorkflowEvent, key);
    field_offset!(WorkflowEvent, kind);
}
