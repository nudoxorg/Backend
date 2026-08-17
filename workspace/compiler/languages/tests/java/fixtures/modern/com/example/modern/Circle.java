package com.example.modern;

/**
 * A circle, identified by its {@code radius}.
 *
 * @param radius the radius; must be non-negative
 */
public record Circle(double radius) implements Shape {
    /** Compact constructor validating {@code radius}. */
    public Circle {
        if (radius < 0) {
            throw new IllegalArgumentException("radius must be non-negative");
        }
    }

    @Override
    public double area() {
        return Math.PI * radius * radius;
    }
}
