/**
 * JSDoc documentation fixture.
 *
 * Exercises `.with_docs(true)` rendering: doc comments on types, interfaces,
 * functions, and their members should flow through into the snapshot.
 */

/**
 * A geographic coordinate pair.
 *
 * Latitude must be in [-90, 90] and longitude in [-180, 180].
 */
export interface Coordinate {
  /** Degrees of latitude — positive is north. */
  readonly lat: number;
  /** Degrees of longitude — positive is east. */
  readonly lng: number;
}

/**
 * A named geographic location.
 *
 * Bundles a human-readable label with its coordinates.
 */
export interface Location {
  /** Human-readable name of the place. */
  name: string;
  /** Geographic position. */
  coords: Coordinate;
  /** Optional description of the location. */
  description?: string;
}

/**
 * Calculate the Haversine distance between two coordinates (in kilometres).
 *
 * @param from - the starting coordinate
 * @param to   - the destination coordinate
 * @returns distance in km
 */
export function haversine(from: Coordinate, to: Coordinate): number {
  // simplified stub
  return 0;
}

/**
 * Format a coordinate as a human-readable string.
 *
 * @param coord - the coordinate to format
 * @param precision - decimal digits (default 4)
 */
export function formatCoord(coord: Coordinate, precision?: number): string {
  const p = precision ?? 4;
  return `${coord.lat.toFixed(p)}, ${coord.lng.toFixed(p)}`;
}
