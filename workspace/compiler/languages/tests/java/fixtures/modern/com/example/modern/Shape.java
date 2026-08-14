package com.example.modern;

/**
 * A closed hierarchy of shapes.
 *
 * @since 1.0
 */
public sealed interface Shape permits Circle, Square {
    /** The area of this shape. */
    double area();

    /**
     * A human-readable description, provided for every {@link Shape}.
     *
     * @return a description mentioning the {@linkplain #area() area}
     */
    default String describe() {
        return "Shape with area " + area();
    }
}
