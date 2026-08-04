package com.example;

/**
 * A named, labeled 2D point — a record with a generic label type.
 *
 * @param <L> the label type
 * @param label the human-readable label
 * @param x     the x-coordinate
 * @param y     the y-coordinate
 */
public record NamedPoint<L>(L label, double x, double y) {

    /**
     * Returns a new point translated by the given deltas.
     *
     * @param dx x translation
     * @param dy y translation
     * @return the translated point
     */
    public NamedPoint<L> translate(double dx, double dy) {
        return new NamedPoint<>(label, x + dx, y + dy);
    }
}
