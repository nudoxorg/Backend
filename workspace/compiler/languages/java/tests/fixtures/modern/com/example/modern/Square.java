package com.example.modern;

/** A square, with an explicit no-arg constructor (JPMS service provider). */
public final class Square implements Shape {
    private final double side;

    /** Construct a unit square. */
    public Square() {
        this(1.0);
    }

    /**
     * Construct a square.
     * @param side the side length
     */
    public Square(double side) {
        this.side = side;
    }

    @Override
    public double area() {
        return side * side;
    }
}
