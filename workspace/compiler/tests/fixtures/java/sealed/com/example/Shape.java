package com.example;

/**
 * A sealed hierarchy of geometric shapes.
 * Only the listed subtypes are permitted.
 */
public sealed interface Shape permits Shape.Circle, Shape.Rectangle, Shape.Triangle {

    /**
     * Returns the area of this shape.
     *
     * @return the area in square units
     */
    double area();

    /**
     * Returns the perimeter of this shape.
     *
     * @return the perimeter in linear units
     */
    double perimeter();

    /**
     * A circle defined by its radius.
     *
     * @param radius the radius; must be positive
     */
    record Circle(double radius) implements Shape {
        public double area() { return Math.PI * radius * radius; }
        public double perimeter() { return 2 * Math.PI * radius; }
    }

    /**
     * An axis-aligned rectangle.
     *
     * @param width  the width; must be positive
     * @param height the height; must be positive
     */
    record Rectangle(double width, double height) implements Shape {
        public double area() { return width * height; }
        public double perimeter() { return 2 * (width + height); }
    }

    /**
     * A triangle defined by three side lengths (validated by the triangle inequality).
     *
     * @param a side a
     * @param b side b
     * @param c side c
     */
    record Triangle(double a, double b, double c) implements Shape {
        public double area() {
            double s = (a + b + c) / 2;
            return Math.sqrt(s * (s - a) * (s - b) * (s - c));
        }
        public double perimeter() { return a + b + c; }
    }
}
