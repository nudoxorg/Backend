import { WritableSignal, signal } from '@angular/core';
import { DistanceUnit } from '../../services/cardio';
import { WeightUnit } from '../../services/weight.service';
import {
  BLANK_SEED,
  SetRow,
  canonicalSeed,
  everyRowHasReps,
  reseedRows,
  rowsFromLoggedSets,
  rowsToLoggedSets,
} from '../../services/set-rows';
import { SetItem } from '../../services/workout-set.service';
import { CARDIO_GROUP, Workout } from '../../services/workout.service';
import { fromCardioLog, toCardioLog } from '../cardio-fields/cardio-fields';

/**
 * One exercise as the set builder edits it — display units, raw text, and the
 * rows the user is typing into — as opposed to {@link SetItem}, which is what
 * gets stored.
 *
 * **Why the fields are signals.** The builder holds a list of these and hands
 * each one to a child editor by reference, so the two components share the very
 * same object. Mutating a plain property on a shared object tells neither of
 * them anything; a signal tells both. The exceptions are the two things nothing
 * ever renders *reactively* — `key`, and `rowPool`, which is only ever read
 * imperatively when the set count changes. Individual row fields stay plain for
 * the same reason the Weeks modal's always have: `[(ngModel)]` writes what the
 * user already sees, so there is nothing to re-render.
 */
export interface BuilderItem {
  /** Stable identity for `@for (… ; track …)`. An array index would do the
   *  wrong thing the moment an exercise is removed from the middle. */
  readonly key: number;
  /**
   * Which weekday this block sits on, 0 = Mon … 6 = Sun — week-set builder only,
   * where one flat list of blocks is rendered grouped under seven day sections.
   * A day set ignores it entirely and leaves it at 0.
   *
   * Builder-only state: it never reaches {@link SetItem}, because a stored item
   * doesn't carry its own day — `WeekSetDay` groups items *by* day, and a day
   * set has no day at all until it is applied to one.
   */
  readonly day: WritableSignal<number>;
  readonly muscleGroup: WritableSignal<string>;
  readonly workoutId: WritableSignal<string>;
  /**
   * The exercise's name and body-weight flag, copied from the library when one
   * is picked. Held here rather than looked up on demand because an item loaded
   * from a saved set may reference an exercise that has since been deleted —
   * the set still knows what it was called, and still applies.
   */
  readonly name: WritableSignal<string>;
  readonly bodyWeight: WritableSignal<boolean>;
  readonly trackTime: WritableSignal<boolean>;
  readonly hasNotes: WritableSignal<boolean>;
  readonly notes: WritableSignal<string>;
  /** Every row created since this item was opened, including ones hidden by a
   *  lower set count — see `growPool`. Never rendered directly. */
  rowPool: SetRow[];
  /** The visible rows: `rowPool` sliced to the current set count. */
  readonly rows: WritableSignal<SetRow[]>;
  readonly cardioTimeText: WritableSignal<string>;
  readonly cardioDistance: WritableSignal<number | null>;
  readonly cardioHeartRate: WritableSignal<number | null>;
  readonly cardioElevation: WritableSignal<number | null>;
}

/** A new, empty exercise block in `group`, on `day` when building a week set. */
export function blankItem(key: number, group: string, day = 0): BuilderItem {
  return {
    key,
    day: signal(day),
    muscleGroup: signal(group),
    workoutId: signal(''),
    name: signal(''),
    bodyWeight: signal(false),
    trackTime: signal(false),
    hasNotes: signal(false),
    notes: signal(''),
    rowPool: [],
    rows: signal<SetRow[]>([]),
    cardioTimeText: signal(''),
    cardioDistance: signal<number | null>(null),
    cardioHeartRate: signal<number | null>(null),
    cardioElevation: signal<number | null>(null),
  };
}

/** A stored item opened for editing, in the reader's display units. */
export function builderItemFrom(
  item: SetItem,
  key: number,
  unit: WeightUnit,
  distanceUnit: DistanceUnit,
  day = 0
): BuilderItem {
  const cardio = fromCardioLog(item.cardio ?? null, distanceUnit);
  const rows = rowsFromLoggedSets(item.sets, unit);
  return {
    key,
    day: signal(day),
    muscleGroup: signal(item.muscleGroup),
    workoutId: signal(item.workoutId),
    name: signal(item.workoutName),
    bodyWeight: signal(item.bodyWeight ?? false),
    trackTime: signal(item.trackTime ?? false),
    hasNotes: signal(!!item.notes),
    notes: signal(item.notes ?? ''),
    rowPool: rows,
    rows: signal(rows.slice()),
    cardioTimeText: signal(cardio.timeText),
    cardioDistance: signal(cardio.distance),
    cardioHeartRate: signal(cardio.heartRate),
    cardioElevation: signal(cardio.elevation),
  };
}

/** Whether this block is collecting a cardio session rather than sets. */
export function isCardioItem(item: BuilderItem): boolean {
  return item.muscleGroup() === CARDIO_GROUP;
}

/**
 * Point `item` at a newly-chosen exercise, re-seeding its set weights from that
 * exercise's usual weight — or clearing them for a body-weight one, which
 * carries no weight in a set at all (see {@link SetItem.bodyWeight}).
 */
export function selectWorkout(
  item: BuilderItem,
  workout: Workout,
  unit: WeightUnit
): void {
  item.workoutId.set(workout.id ?? '');
  item.name.set(workout.name);
  item.bodyWeight.set(workout.bodyWeight ?? false);
  const seed = workout.bodyWeight
    ? BLANK_SEED
    : canonicalSeed(workout.usualWeight, unit);
  item.rowPool = reseedRows(item.rowPool, seed);
  item.rows.set(item.rowPool.slice(0, item.rows().length));
}

/** What {@link toSetItem} produces: the storable item, or the reason it can't
 *  be stored yet. */
export type ItemResult =
  | { ok: true; item: SetItem }
  | { ok: false; error: string };

/**
 * A builder block converted to what gets stored, or the first thing wrong with
 * it. Validation and conversion live together because they answer the same
 * question — "is there enough here to save?" — and splitting them is how the
 * two drift apart.
 *
 * Weights convert to canonical pounds here, through the round-trip guard in
 * `set-rows.ts`: a field the user never touched is written back verbatim rather
 * than re-converted.
 */
export function toSetItem(
  item: BuilderItem,
  unit: WeightUnit,
  distanceUnit: DistanceUnit
): ItemResult {
  if (!item.workoutId()) {
    return { ok: false, error: 'Pick an exercise for every block, or remove it.' };
  }

  const name = item.name();
  const base = {
    workoutId: item.workoutId(),
    workoutName: name,
    muscleGroup: item.muscleGroup(),
    notes: item.hasNotes() ? item.notes().trim() : '',
  };

  if (isCardioItem(item)) {
    const cardio = toCardioLog(
      {
        timeText: item.cardioTimeText(),
        distance: item.cardioDistance(),
        heartRate: item.cardioHeartRate(),
        elevation: item.cardioElevation(),
      },
      distanceUnit
    );
    if (!cardio) {
      return { ok: false, error: `Enter a duration and distance for ${name}.` };
    }
    return { ok: true, item: { ...base, sets: [], cardio } };
  }

  const rows = item.rows();
  if (rows.length === 0) {
    return { ok: false, error: `Add at least one set to ${name}.` };
  }
  if (!everyRowHasReps(rows)) {
    return { ok: false, error: `Enter the reps for every set of ${name}.` };
  }

  const trackTime = item.trackTime();
  const stored: SetItem = {
    ...base,
    trackTime,
    sets: rowsToLoggedSets(rows, unit, trackTime),
  };
  if (item.bodyWeight()) stored.bodyWeight = true;
  return { ok: true, item: stored };
}
