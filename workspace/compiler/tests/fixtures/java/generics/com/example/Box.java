package com.example;

import java.util.List;
import java.util.Optional;

/**
 * A generic container holding a single value.
 *
 * @param <T> the type of the contained value
 */
public class Box<T> {

    /** The wrapped value. */
    private T value;

    /**
     * Creates a Box containing {@code value}.
     *
     * @param value the value to wrap
     */
    public Box(T value) {
        this.value = value;
    }

    /**
     * Returns the contained value.
     *
     * @return the value
     */
    public T get() {
        return value;
    }

    /**
     * Returns the value wrapped in an Optional.
     *
     * @return an Optional of the value
     */
    public Optional<T> asOptional() {
        return Optional.ofNullable(value);
    }

    /**
     * Wraps each element of the list in a Box.
     *
     * @param <E>  the element type
     * @param list the source list
     * @return a list of Box instances
     */
    public static <E> List<Box<E>> wrapAll(List<E> list) {
        return list.stream().map(Box::new).toList();
    }
}
