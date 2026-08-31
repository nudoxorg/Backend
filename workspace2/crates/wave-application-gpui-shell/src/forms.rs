//! Closed, bounded GPUI form state that constructs only typed application inputs.

use wave_application_core::{
    ApplicationInput, ContentId, CorrelationId, InconsistentRecovery, InputText,
    InputTextJoinError, OperationKey, Pin, ResourceBudget,
};

use crate::ServiceAction;

/// One visible text field in a closed action form.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum FormField {
    /// Compiler language vocabulary token.
    Language,
    /// Compiler stage vocabulary token.
    Stage,
    /// Compiler source input.
    Source,
    /// Immutable snapshot selector.
    Snapshot,
    /// Bounded retrieval query.
    Query,
}

/// A bounded retrieval row limit selected by the UI without text parsing.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct ResultLimit(pub u8);

impl ResultLimit {
    /// Largest caller-selected result count supported by the current core service.
    pub const MAXIMUM: u8 = 4;

    /// Makes one typed result limit.
    ///
    /// # Errors
    ///
    /// Returns [`FormError::LimitExceeded`] when the limit exceeds the core command bound.
    pub const fn new(value: u8) -> Result<Self, FormError> {
        if value > Self::MAXIMUM {
            Err(FormError::LimitExceeded {
                requested: value,
                maximum: Self::MAXIMUM,
            })
        } else {
            Ok(Self(value))
        }
    }
}

/// A canonical recovery selection supplied by a future publication/retrieval seam.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct RecoverySelection {
    /// Pinned expected authority.
    pub expected: Pin,
    /// Optional distinct observed authority for inconsistency recovery.
    pub observed: Option<Pin>,
    /// Verified analyzer bundle.
    pub bundle: ContentId<wave_application_core::CapabilityDomain>,
    /// Resource credits selected by the canonical owner.
    pub budget: ResourceBudget,
}

/// Closed internal form variant; no string action or field name is accepted.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum FormState {
    /// Typed compiler request fields.
    Generate {
        /// Keyboard-selected field.
        focused: FormField,
        /// Typed language field.
        language: Option<InputText>,
        /// Typed stage field.
        stage: Option<InputText>,
        /// Typed source field.
        source: Option<InputText>,
    },
    /// Snapshot/locality/graph/vector form.
    Snapshot {
        /// Closed selected action.
        action: SnapshotAction,
        /// Keyboard-selected field.
        focused: FormField,
        /// Typed snapshot selector.
        snapshot: Option<InputText>,
        /// Caller-selected bounded graph/vector limit.
        limit: ResultLimit,
    },
    /// Exact/lexical search form.
    Search {
        /// Keyboard-selected field.
        focused: FormField,
        /// Typed snapshot selector.
        snapshot: Option<InputText>,
        /// Typed query field.
        query: Option<InputText>,
        /// Caller-selected bounded result limit.
        limit: ResultLimit,
    },
    /// Argument-free capability health form.
    Health,
    /// Recovery/release form awaiting canonical immutable selection facts.
    Recovery {
        /// Closed selected action.
        action: RecoveryAction,
        /// Canonical selection; the view never guesses it.
        selection: Option<RecoverySelection>,
    },
    /// Operation form populated only from an actual service operation.
    Operation {
        /// Closed selected action.
        action: OperationAction,
        /// Existing operation authority.
        operation: Option<OperationKey>,
    },
}

/// Closed snapshot operations that share the one typed snapshot field.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum SnapshotAction {
    /// Inspect snapshot status.
    Status,
    /// Inspect locality.
    Locality,
    /// Query graph facts.
    Graph,
    /// Query vector facts.
    Vector,
}

/// Closed recovery operations that require a canonical immutable selection.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum RecoveryAction {
    /// Recover local analyzer state.
    Local,
    /// Recover a mismatched immutable authority.
    Inconsistent,
    /// Release the local analyzer bundle.
    Release,
}

/// Closed operation operations that require a real service operation key.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum OperationAction {
    /// Observe service-owned execution.
    Poll,
    /// Cancel service-owned execution.
    Cancel,
}

/// A closed UI-form rejection before business dispatch.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum FormError {
    /// No visible form is active.
    NoActiveForm,
    /// The selected form does not contain that closed text field.
    FieldUnavailable(FormField),
    /// A required typed text field has not been entered.
    MissingText(FormField),
    /// A canonical recovery selection has not reached the UI.
    MissingRecoverySelection,
    /// Inconsistent recovery needs both canonical expected and observed pins.
    MissingObservedPin,
    /// No actual service operation is selected.
    MissingOperation,
    /// A selected typed result limit is above the core command maximum.
    LimitExceeded {
        /// Requested typed result count.
        requested: u8,
        /// Maximum supported count.
        maximum: u8,
    },
    /// A text-edit request supplied no replacement text where insertion was required.
    EmptyTextEdit {
        /// Closed field that rejected the edit.
        field: FormField,
    },
    /// A text edit would exceed the fixed transport-width boundary.
    InputTooLong {
        /// Closed field that rejected the edit.
        field: FormField,
        /// Requested total UTF-8 byte length.
        actual: usize,
        /// Fixed accepted UTF-8 byte length.
        maximum: usize,
    },
    /// A text edit's mathematical joined length cannot be represented by `usize`.
    InputLengthOverflow {
        /// Closed field that rejected the edit.
        field: FormField,
        /// Existing prefix byte length.
        prefix: usize,
        /// Inserted byte length.
        inserted: usize,
        /// Existing suffix byte length.
        suffix: usize,
    },
    /// The selected closed form has no caller-controlled result limit.
    LimitUnavailable,
    /// The selected closed form has no editable text field.
    NoEditableField,
}

impl FormState {
    /// Starts the specific closed form for an action.
    #[must_use]
    pub const fn from_action(action: ServiceAction, operation: Option<OperationKey>) -> Self {
        match action {
            ServiceAction::Generate => Self::Generate {
                focused: FormField::Language,
                language: None,
                stage: None,
                source: None,
            },
            ServiceAction::SnapshotStatus => Self::Snapshot {
                action: SnapshotAction::Status,
                focused: FormField::Snapshot,
                snapshot: None,
                limit: ResultLimit(4),
            },
            ServiceAction::Search => Self::Search {
                focused: FormField::Snapshot,
                snapshot: None,
                query: None,
                limit: ResultLimit(4),
            },
            ServiceAction::Graph => Self::Snapshot {
                action: SnapshotAction::Graph,
                focused: FormField::Snapshot,
                snapshot: None,
                limit: ResultLimit(4),
            },
            ServiceAction::Vector => Self::Snapshot {
                action: SnapshotAction::Vector,
                focused: FormField::Snapshot,
                snapshot: None,
                limit: ResultLimit(4),
            },
            ServiceAction::Locality => Self::Snapshot {
                action: SnapshotAction::Locality,
                focused: FormField::Snapshot,
                snapshot: None,
                limit: ResultLimit(4),
            },
            ServiceAction::Health => Self::Health,
            ServiceAction::RecoverLocal => Self::Recovery {
                action: RecoveryAction::Local,
                selection: None,
            },
            ServiceAction::RecoverInconsistent => Self::Recovery {
                action: RecoveryAction::Inconsistent,
                selection: None,
            },
            ServiceAction::ReleaseLocal => Self::Recovery {
                action: RecoveryAction::Release,
                selection: None,
            },
            ServiceAction::PollExecution => Self::Operation {
                action: OperationAction::Poll,
                operation,
            },
            ServiceAction::Cancel => Self::Operation {
                action: OperationAction::Cancel,
                operation,
            },
        }
    }

    /// Replaces a selected bounded text field with a transport-validated input.
    ///
    /// # Errors
    ///
    /// Returns [`FormError::FieldUnavailable`] when a field does not belong to this form.
    pub fn replace_text(&mut self, field: FormField, value: InputText) -> Result<(), FormError> {
        match self {
            Self::Generate {
                focused: _,
                language,
                stage,
                source,
            } => match field {
                FormField::Language => *language = Some(value),
                FormField::Stage => *stage = Some(value),
                FormField::Source => *source = Some(value),
                FormField::Snapshot | FormField::Query => {
                    return Err(FormError::FieldUnavailable(field));
                }
            },
            Self::Snapshot { snapshot, .. } => match field {
                FormField::Snapshot => *snapshot = Some(value),
                _ => return Err(FormError::FieldUnavailable(field)),
            },
            Self::Search {
                snapshot, query, ..
            } => match field {
                FormField::Snapshot => *snapshot = Some(value),
                FormField::Query => *query = Some(value),
                _ => return Err(FormError::FieldUnavailable(field)),
            },
            Self::Health | Self::Recovery { .. } | Self::Operation { .. } => {
                return Err(FormError::FieldUnavailable(field));
            }
        }
        match self {
            Self::Generate { focused, .. }
            | Self::Snapshot { focused, .. }
            | Self::Search { focused, .. } => {
                *focused = field;
            }
            Self::Health | Self::Recovery { .. } | Self::Operation { .. } => {}
        }
        Ok(())
    }

    /// Selects a field that belongs to this form.
    ///
    /// # Errors
    ///
    /// Returns [`FormError::FieldUnavailable`] for a field outside this form's closed shape.
    pub fn select_field(&mut self, field: FormField) -> Result<(), FormError> {
        match self {
            Self::Generate { focused, .. }
                if matches!(
                    field,
                    FormField::Language | FormField::Stage | FormField::Source
                ) =>
            {
                *focused = field;
            }
            Self::Snapshot { focused, .. } if field == FormField::Snapshot => *focused = field,
            Self::Search { focused, .. }
                if matches!(field, FormField::Snapshot | FormField::Query) =>
            {
                *focused = field;
            }
            _ => return Err(FormError::FieldUnavailable(field)),
        }
        Ok(())
    }

    /// Advances or reverses the focused field in this form's closed field order.
    ///
    /// # Errors
    ///
    /// Returns a closed form error when this action has no editable text field.
    pub fn move_field_focus(&mut self, forward: bool) -> Result<(), FormError> {
        match self {
            Self::Generate { focused, .. } => {
                *focused = adjacent_generate_field(*focused, forward);
            }
            Self::Snapshot { .. } => {}
            Self::Search { focused, .. } => {
                *focused = if *focused == FormField::Snapshot {
                    FormField::Query
                } else {
                    FormField::Snapshot
                };
            }
            Self::Health | Self::Recovery { .. } | Self::Operation { .. } => {
                return Err(FormError::FieldUnavailable(FormField::Query));
            }
        }
        Ok(())
    }

    /// Appends text to the currently focused bounded form field.
    ///
    /// # Errors
    ///
    /// Returns a closed form error when no text field is active or the fixed transport bound would overflow.
    pub fn append_focused_text(&mut self, text: &str) -> Result<(), FormError> {
        match self {
            Self::Generate {
                focused,
                language,
                stage,
                source,
            } => match focused {
                FormField::Language => append_input(language, text, FormField::Language)?,
                FormField::Stage => append_input(stage, text, FormField::Stage)?,
                FormField::Source => append_input(source, text, FormField::Source)?,
                FormField::Snapshot | FormField::Query => {
                    return Err(FormError::FieldUnavailable(*focused));
                }
            },
            Self::Snapshot { snapshot, .. } => append_input(snapshot, text, FormField::Snapshot)?,
            Self::Search {
                focused,
                snapshot,
                query,
                ..
            } => match focused {
                FormField::Snapshot => append_input(snapshot, text, FormField::Snapshot)?,
                FormField::Query => append_input(query, text, FormField::Query)?,
                _ => return Err(FormError::FieldUnavailable(*focused)),
            },
            Self::Health | Self::Recovery { .. } | Self::Operation { .. } => {
                return Err(FormError::NoEditableField);
            }
        }
        Ok(())
    }

    /// Erases one UTF-8 character from the focused field.
    ///
    /// # Errors
    ///
    /// Returns a closed form error when the active field is absent or empty.
    pub fn erase_focused_text(&mut self) -> Result<(), FormError> {
        match self {
            Self::Generate {
                focused,
                language,
                stage,
                source,
            } => match focused {
                FormField::Language => erase_input(language, FormField::Language)?,
                FormField::Stage => erase_input(stage, FormField::Stage)?,
                FormField::Source => erase_input(source, FormField::Source)?,
                FormField::Snapshot | FormField::Query => {
                    return Err(FormError::FieldUnavailable(*focused));
                }
            },
            Self::Snapshot { snapshot, .. } => erase_input(snapshot, FormField::Snapshot)?,
            Self::Search {
                focused,
                snapshot,
                query,
                ..
            } => match focused {
                FormField::Snapshot => erase_input(snapshot, FormField::Snapshot)?,
                FormField::Query => erase_input(query, FormField::Query)?,
                _ => return Err(FormError::FieldUnavailable(*focused)),
            },
            Self::Health | Self::Recovery { .. } | Self::Operation { .. } => {
                return Err(FormError::NoEditableField);
            }
        }
        Ok(())
    }

    /// Replaces the typed retrieval limit for graph, vector, or search forms.
    ///
    /// # Errors
    ///
    /// Returns a closed form error when the active action has no row limit or the requested
    /// value exceeds the core command bound.
    pub fn replace_limit(&mut self, value: ResultLimit) -> Result<(), FormError> {
        match self {
            Self::Snapshot {
                action: SnapshotAction::Graph | SnapshotAction::Vector,
                limit,
                ..
            }
            | Self::Search { limit, .. } => *limit = value,
            Self::Generate { .. }
            | Self::Snapshot { .. }
            | Self::Health
            | Self::Recovery { .. }
            | Self::Operation { .. } => return Err(FormError::LimitUnavailable),
        }
        Ok(())
    }

    /// Constructs one core command without repeating core semantic validation.
    ///
    /// # Errors
    ///
    /// Returns a closed form rejection when a required typed field or canonical authority is absent.
    pub fn submit(self, correlation: CorrelationId) -> Result<ApplicationInput, FormError> {
        match self {
            Self::Generate {
                language,
                stage,
                source,
                ..
            } => submit_generate(correlation, language, stage, source),
            Self::Snapshot {
                action,
                snapshot,
                limit,
                ..
            } => submit_snapshot(correlation, action, snapshot, limit),
            Self::Search {
                snapshot,
                query,
                limit,
                ..
            } => submit_search(correlation, snapshot, query, limit),
            Self::Health => Ok(ApplicationInput::Health { correlation }),
            Self::Recovery { action, selection } => submit_recovery(correlation, action, selection),
            Self::Operation { action, operation } => {
                submit_operation(correlation, action, operation)
            }
        }
    }
}

const fn adjacent_generate_field(current: FormField, forward: bool) -> FormField {
    let fields = [FormField::Language, FormField::Stage, FormField::Source];
    let index = match current {
        FormField::Language | FormField::Snapshot | FormField::Query => 0,
        FormField::Stage => 1,
        FormField::Source => 2,
    };
    if forward {
        fields[(index + 1) % fields.len()]
    } else {
        fields[(index + fields.len() - 1) % fields.len()]
    }
}

fn submit_generate(
    correlation: CorrelationId,
    language: Option<InputText>,
    stage: Option<InputText>,
    source: Option<InputText>,
) -> Result<ApplicationInput, FormError> {
    Ok(ApplicationInput::Generate {
        correlation,
        language: language.ok_or(FormError::MissingText(FormField::Language))?,
        stage: stage.ok_or(FormError::MissingText(FormField::Stage))?,
        source: source.ok_or(FormError::MissingText(FormField::Source))?,
    })
}

fn submit_snapshot(
    correlation: CorrelationId,
    action: SnapshotAction,
    snapshot: Option<InputText>,
    limit: ResultLimit,
) -> Result<ApplicationInput, FormError> {
    let snapshot = snapshot.ok_or(FormError::MissingText(FormField::Snapshot))?;
    match action {
        SnapshotAction::Status => Ok(ApplicationInput::SnapshotStatus {
            correlation,
            snapshot,
        }),
        SnapshotAction::Locality => Ok(ApplicationInput::Locality {
            correlation,
            snapshot,
        }),
        SnapshotAction::Graph => Ok(ApplicationInput::Graph {
            correlation,
            snapshot,
            limit: limit.0,
        }),
        SnapshotAction::Vector => Ok(ApplicationInput::Vector {
            correlation,
            snapshot,
            limit: limit.0,
        }),
    }
}

fn submit_search(
    correlation: CorrelationId,
    snapshot: Option<InputText>,
    query: Option<InputText>,
    limit: ResultLimit,
) -> Result<ApplicationInput, FormError> {
    Ok(ApplicationInput::Search {
        correlation,
        snapshot: snapshot.ok_or(FormError::MissingText(FormField::Snapshot))?,
        query: query.ok_or(FormError::MissingText(FormField::Query))?,
        limit: limit.0,
    })
}

fn submit_recovery(
    correlation: CorrelationId,
    action: RecoveryAction,
    selection: Option<RecoverySelection>,
) -> Result<ApplicationInput, FormError> {
    let selection = selection.ok_or(FormError::MissingRecoverySelection)?;
    match action {
        RecoveryAction::Local => Ok(ApplicationInput::RecoverLocal {
            correlation,
            pin: selection.expected,
            bundle: selection.bundle,
            budget: selection.budget,
        }),
        RecoveryAction::Inconsistent => Ok(ApplicationInput::RecoverInconsistent(
            InconsistentRecovery {
                correlation,
                expected: selection.expected,
                observed: selection.observed.ok_or(FormError::MissingObservedPin)?,
                bundle: selection.bundle,
                budget: selection.budget,
            },
        )),
        RecoveryAction::Release => Ok(ApplicationInput::ReleaseLocal {
            correlation,
            pin: selection.expected,
            bundle: selection.bundle,
            budget: selection.budget,
        }),
    }
}

fn submit_operation(
    correlation: CorrelationId,
    action: OperationAction,
    operation: Option<OperationKey>,
) -> Result<ApplicationInput, FormError> {
    let operation = operation.ok_or(FormError::MissingOperation)?;
    match action {
        OperationAction::Poll => Ok(ApplicationInput::PollExecution {
            correlation,
            operation,
        }),
        OperationAction::Cancel => Ok(ApplicationInput::Cancel {
            correlation,
            operation,
        }),
    }
}

fn append_input(
    slot: &mut Option<InputText>,
    appended: &str,
    field: FormField,
) -> Result<(), FormError> {
    if appended.is_empty() {
        return Err(FormError::EmptyTextEdit { field });
    }
    let current = slot.as_deref().map_or("", |current| current);
    *slot = Some(
        InputText::try_from_parts(current, appended, "")
            .map_err(|error| form_join_error(field, error))?,
    );
    Ok(())
}

fn erase_input(slot: &mut Option<InputText>, field: FormField) -> Result<(), FormError> {
    let Some(current) = slot else {
        return Err(FormError::EmptyTextEdit { field });
    };
    if current.is_empty() {
        return Err(FormError::EmptyTextEdit { field });
    }
    let boundary = current
        .char_indices()
        .next_back()
        .map_or(0, |(boundary, _character)| boundary);
    if boundary == 0 {
        *slot = None;
        return Ok(());
    }
    *slot = Some(
        InputText::try_from_parts(&current[..boundary], "", "")
            .map_err(|error| form_join_error(field, error))?,
    );
    Ok(())
}

const fn form_join_error(field: FormField, error: InputTextJoinError) -> FormError {
    match error {
        InputTextJoinError::LengthOverflow {
            prefix,
            inserted,
            suffix,
        } => FormError::InputLengthOverflow {
            field,
            prefix,
            inserted,
            suffix,
        },
        InputTextJoinError::InputTooLong(error) => FormError::InputTooLong {
            field,
            actual: error.actual,
            maximum: error.maximum,
        },
    }
}
