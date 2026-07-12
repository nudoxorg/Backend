package com.example;

/**
 * An immutable 2D point (Java 16+ record).
 *
 * @param x the x-coordinate
 * @param y the y-coordinate
 */
public record Point(double x, double y) {

    /**
     * Compact constructor that validates coordinates.
     *
     * @throws IllegalArgumentException if either coordinate is NaN or infinite
     */
    public Point {
        if (!Double.isFinite(x) || !Double.isFinite(y)) {
            throw new IllegalArgumentException("Coordinates must be finite");
        }
    }

    /**
     * Returns the Euclidean distance from this point to another.
     *
     * @param other the other point
     * @return the distance
     */
    public double distanceTo(Point other) {
        double dx = this.x - other.x;
        double dy = this.y - other.y;
        return Math.sqrt(dx * dx + dy * dy);
    }

    /**
     * Returns the origin (0, 0).
     *
     * @return the origin Point
     */
    public static Point origin() {
        return new Point(0.0, 0.0);
    }
}
