package com.example;

/**
 * Anything that can be drawn to a canvas.
 *
 * <p>Implementors must supply {@link #draw(String)} and {@link #resize(double)}.
 * The {@link #clear(String)} method has a default no-op implementation.
 */
public interface Drawable {

    /**
     * Draws this shape on the given canvas.
     *
     * @param canvas the target canvas identifier
     */
    void draw(String canvas);

    /**
     * Resizes this shape by the given factor.
     *
     * @param factor must be positive
     */
    void resize(double factor);

    /**
     * Clears this shape from the canvas.
     * Default implementation does nothing.
     *
     * @param canvas the target canvas identifier
     */
    default void clear(String canvas) {
        // default: no-op
    }

    /**
     * Returns the bounding area of this shape.
     *
     * @return the area in square pixels
     */
    double area();
}
