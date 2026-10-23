import { Component, computed, inject, input, model } from '@angular/core';
import { FormsModule } from '@angular/forms';
import { SettingsService } from '../../services/settings.service';
import {
  CardioLog,
  formatTime,
  parseTime,
} from '../../services/week.service';
import {
  DistanceUnit,
  displayDistance,
  distanceToCanonical,
  displayElevation,
  elevationToCanonical,
  formatPace,
} from '../../services/cardio';

/**
 * The cardio session form: duration, distance, a read-only computed pace, and
 * optional heart rate and elevation.
 *
 * Shared by the Weeks page (logging a session) and the set builder (planning
 * one). Values are bound two-way in the user's **display** units; the
 * {@link toCardioLog} / {@link fromCardioLog} helpers below convert at the
 * boundary, which is the only place canonical miles/feet and display units are
 * allowed to meet.
 *
 * Pace is always derived from duration ÷ distance and never typed — two numbers
 * that can disagree is a bug waiting to be filed.
 */
@Component({
  selector: 'app-cardio-fields',
  standalone: true,
  imports: [FormsModule],
  template: `
    <div class="form-row">
      <div class="form-group">
        <label class="form-label" [attr.for]="idPrefix() + '-time'">Duration (m:ss or h:mm:ss)</label>
        <input class="form-input" [attr.id]="idPrefix() + '-time'" type="text" placeholder="m:ss"
          [ngModel]="timeText()" (ngModelChange)="timeText.set($event)" [name]="idPrefix() + '-time'" />
      </div>
      <div class="form-group">
        <label class="form-label" [attr.for]="idPrefix() + '-distance'">Distance ({{ distanceUnit() }})</label>
        <input class="form-input" [attr.id]="idPrefix() + '-distance'" type="number" min="0" step="0.01"
          placeholder="0" [ngModel]="distance()" (ngModelChange)="distance.set($event)"
          [name]="idPrefix() + '-distance'" />
      </div>
    </div>

    @if (pace(); as p) {
      <p class="form-hint">Pace: {{ p }}</p>
    }

    <div class="form-row">
      <div class="form-group">
        <label class="form-label" [attr.for]="idPrefix() + '-hr'">Avg. heart rate (optional)</label>
        <input class="form-input" [attr.id]="idPrefix() + '-hr'" type="number" min="0" placeholder="bpm"
          [ngModel]="heartRate()" (ngModelChange)="heartRate.set($event)" [name]="idPrefix() + '-hr'" />
      </div>
      <div class="form-group">
        <label class="form-label" [attr.for]="idPrefix() + '-elevation'">
          Elevation gain (optional, {{ elevationUnitLabel() }})
        </label>
        <input class="form-input" [attr.id]="idPrefix() + '-elevation'" type="number" min="0" placeholder="0"
          [ngModel]="elevation()" (ngModelChange)="elevation.set($event)" [name]="idPrefix() + '-elevation'" />
      </div>
    </div>
  `,
  styles: [
    `
      /* A fragment of someone else's form — stay out of its layout. */
      :host {
        display: contents;
      }
    `,
  ],
})
export class CardioFieldsComponent {
  private readonly settings = inject(SettingsService);

  /** Duration as the raw "m:ss" text the user edits. */
  readonly timeText = model('');
  /** Distance in the user's display unit. */
  readonly distance = model<number | null>(null);
  readonly heartRate = model<number | null>(null);
  /** Elevation gain in the user's display unit (feet with miles, meters with km). */
  readonly elevation = model<number | null>(null);

  /** Unique prefix for `id`/`name` attributes — see `SetRowsEditorComponent`. */
  readonly idPrefix = input('cardio');

  protected readonly distanceUnit = this.settings.distanceUnit;

  /** Elevation is shown in feet alongside miles, meters alongside km. */
  protected readonly elevationUnitLabel = computed(() =>
    this.distanceUnit() === 'mi' ? 'ft' : 'm'
  );

  /** Read-only pace derived from the entered duration and distance. */
  protected readonly pace = computed(() => {
    const seconds = parseTime(this.timeText());
    const distance = this.distance();
    const unit = this.distanceUnit();
    const canonical = distance == null ? null : distanceToCanonical(distance, unit);
    return formatPace(seconds, canonical, unit);
  });
}

/**
 * The stored (canonical) log for a set of display-unit field values, or `null`
 * when duration and a positive distance aren't both present — the two fields a
 * cardio session can't be saved without.
 */
export function toCardioLog(
  fields: {
    timeText: string;
    distance: number | null;
    heartRate: number | null;
    elevation: number | null;
  },
  unit: DistanceUnit
): CardioLog | null {
  const time = parseTime(fields.timeText);
  const distance =
    fields.distance == null ? null : distanceToCanonical(fields.distance, unit);
  if (time == null || distance == null || distance <= 0) return null;
  return {
    time,
    distance,
    heartRate: fields.heartRate,
    elevation:
      fields.elevation == null ? null : elevationToCanonical(fields.elevation, unit),
  };
}

/** Inverse of {@link toCardioLog}: a stored log back to display-unit fields. */
export function fromCardioLog(
  cardio: CardioLog | null,
  unit: DistanceUnit
): {
  timeText: string;
  distance: number | null;
  heartRate: number | null;
  elevation: number | null;
} {
  return {
    timeText: formatTime(cardio?.time ?? null),
    distance: displayDistance(cardio?.distance ?? null, unit),
    heartRate: cardio?.heartRate ?? null,
    elevation: displayElevation(cardio?.elevation ?? null, unit),
  };
}
