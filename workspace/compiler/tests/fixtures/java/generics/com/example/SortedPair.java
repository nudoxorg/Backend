package com.example;

/**
 * An ordered pair of two values where both types must be {@link Comparable}.
 *
 * @param <T> the type of both elements; must be {@code Comparable} to itself
 */
public class SortedPair<T extends Comparable<T>> {

    /** The smaller (first) element. */
    private final T first;

    /** The larger (second) element. */
    private final T second;

    /**
     * Constructs a SortedPair, placing the smaller value first.
     *
     * @param a one element
     * @param b the other element
     */
    public SortedPair(T a, T b) {
        if (a.compareTo(b) <= 0) {
            this.first = a;
            this.second = b;
        } else {
            this.first = b;
            this.second = a;
        }
    }

    /** Returns the smaller element. */
    public T getFirst() {
        return first;
    }

    /** Returns the larger element. */
    public T getSecond() {
        return second;
    }

    /**
     * Computes the distance between the two elements using
     * {@link Comparable#compareTo}.
     *
     * @return the comparator result (always {@code >= 0})
     */
    public int distance() {
        return second.compareTo(first);
    }
}
